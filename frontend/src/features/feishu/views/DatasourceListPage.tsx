/**
 * 飞书数据源列表页（路由 `/feishu/datasources`，路由级 lazy → 必须 default 导出）。
 *
 * 页面负责的是「接线」：查询状态交给 `useListQuery()`，数据交给 `useDatasourceList()`，
 * 渲染交给若干个受控组件。四个不变量写在这里而不是组件里：
 *
 * 1. **卡片与台账是同一份查询状态、同一份数据的两种渲染**——切视图只换渲染方式，
 *    不重新发请求、不丢搜索词与页码（设计 §5.4）；
 * 2. **权限门控是「不渲染」而不是「禁用」**：无写权限时「添加数据源」与列表项上的
 *    「⋯」菜单整个不出现（禁用表示「此刻不可用」，而这里表示「这个入口不属于你」）；
 * 3. **指引只在卡住的那一刻出现**：空态（一个数据源都没有）与「搜索无结果」是两个
 *    不同的分支——前者整屏归四步建档，连工具栏都不渲染；后者要保留工具栏并给
 *    「清除筛选」。
 * 4. **写操作全部落在表级**：建源走配置向导（一次事务写表级行 + N 条字段绑定），
 *    改名称/删源按表级主键 `id` 定位。字段级那套按 `source_key` 定位的可写入口
 *    在服务端已经退役，界面上不能留任何指向它们的按钮。
 *
 * 提交后一律**回读列表**而不做乐观更新：`update` 只回三个计数、`delete` 只回两个计数，
 * 都不含最新记录，既然回读是必需的，乐观更新只买到一次闪烁。
 *
 * 空态判定遵循「只说能证明的话」：`total` 为 0 才有资格说「没有数据源」/「没有匹配」；
 * 「当前页为空」不构成任何结论（结果集可能只是缩小了），那是页码越界，夹回有效页即可。
 */

import { useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { useQueryClient } from "@tanstack/react-query";
import { Plus, RefreshCw } from "lucide-react";

import { Button } from "@/shared/ui/button";

import {
  enabledBindingInputs,
  feishuQueryKeys,
  useDatasourceList,
  useFeishuActions,
  useTableWizardClient,
} from "../api";
import type { CreatedTable, CreateTableSubmission } from "../api";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { DatasourceCardGrid } from "../components/DatasourceCardGrid";
import { DatasourceEditDialog } from "../components/DatasourceEditDialog";
import { DatasourceLedger } from "../components/DatasourceLedger";
import { DatasourceTableWizard } from "../components/DatasourceTableWizard";
import { ListPagination } from "../components/ListPagination";
import { ListToolbar } from "../components/ListToolbar";
import {
  TokenPrecheckNotice,
  type TokenPrecheckMode,
} from "../components/TokenPrecheckNotice";
import { useListQuery } from "../list-query";
import type {
  DatasourceFieldBinding,
  DatasourceItem,
  TokenPrecheckResult,
} from "../types";

/// 四步指引（设计 §5.5 的四步原文）：只写「去哪做、填什么」，不写接口路径与字段名。
const FOUR_STEPS: ReadonlyArray<{ title: string; detail: string }> = [
  {
    title: "在飞书审批后台配置控件",
    detail:
      "把单选或多选控件的取值方式改成「使用外部选项」，自定义一个 Token 并记住它——服务端只保存它的摘要，之后无法回显，请先复制到安全的地方。",
  },
  {
    title: "在这里建一个数据源",
    detail:
      "点「添加数据源」走配置向导：选表 → 选视图 → 勾要接的列。每条列会自动拿到自己的源标识与 Token——标识进接口地址，Token 由服务端生成，建好后到详情页的凭据清单里复制。",
  },
  {
    title: "把接口地址与 Token 填回审批后台",
    detail:
      "回到那个控件的外部选项配置填上这两项，并在飞书那边点「校验数据」确认能拉到选项。",
  },
  {
    title: "在多维表格配自动化推送选项",
    detail:
      "自动化里的 HTTP 请求带 Authorization: Bearer <管理 Token> 调写入接口，选项随表格变动自动更新。数据源建好之后一直是空的，多半是这一步还没跑过。",
  },
];

/// 编辑对话框的目标：主键 + 现值 + 这一刻的绑定快照。
///
/// 绑定快照在这里取而不是提交时再取：对话框开着的时候列表可能因为轮询/回读换了一页，
/// 提交时再去找那一行会拿错人的绑定。
type EditTarget = {
  datasourceId: number;
  title: string;
  bindings: DatasourceFieldBinding[];
};

/// 删除确认的目标。表级世界里的身份是 `id`，标题给人看。
type ConfirmTarget = {
  datasourceId: number;
  title: string;
};

/// 预检状态。`result` 为 null 表示还在检——这一屏必须显示真实标识。
///
/// `datasourceId` 与 `sourceKey` **两个都要**：前者是「哪一条数据源」（详情页的身份，
/// 路由 `/feishu/datasources/:id`），后者是「哪一条绑定」（预检与轮换都按字段做，
/// 而 `precheckOutdated` 也要用它判断那条绑定还在不在）。
type PrecheckState = {
  datasourceId: number;
  sourceKey: string;
  title: string;
  mode: TokenPrecheckMode;
  result: TokenPrecheckResult | null;
};

function messageOf(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

function idOf(item: DatasourceItem): number | null {
  return item.id;
}

export default function DatasourceListPage() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const actions = useFeishuActions();
  const wizardClient = useTableWizardClient();
  const controller = useListQuery();
  const listQuery = useDatasourceList(controller.query);

  const [wizardOpen, setWizardOpen] = useState(false);
  const [editTarget, setEditTarget] = useState<EditTarget | null>(null);
  const [confirmTarget, setConfirmTarget] = useState<ConfirmTarget | null>(
    null,
  );
  const [precheck, setPrecheck] = useState<PrecheckState | null>(null);
  const [precheckPending, setPrecheckPending] = useState(false);
  const [editPending, setEditPending] = useState(false);
  const [deletePending, setDeletePending] = useState(false);
  const [editError, setEditError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [actionNotice, setActionNotice] = useState<string | null>(null);

  const items = listQuery.data?.items ?? [];
  const total = listQuery.data?.total ?? null;
  const page = controller.state.page;
  const pageSize = controller.state.pageSize;
  const { setPage } = controller;
  // 判定用去抖后的查询快照：它才是产出这份结果集的那次请求。
  const filtering =
    controller.query.search.trim() !== "" || controller.query.status !== "all";
  /**
   * 「这份 data 已经落定、且属于当前查询键」。
   *
   * 必须排除 `isPlaceholderData`：`keepPreviousData` 让切查询键的那一帧先拿**上一个键**
   * 的 items/total 顶上，而 `isPending` / `isError` 都还是 false。此时 items 与当前的
   * 搜索词、状态筛选、页码统统对不上，拿它判空就会得出与事实相反的结论（典型表现：
   * 从「已停用」切回「全部」，而「全部 + 当前排序」这个键从没被取过时，整屏闪出四步
   * 建档空态，连工具栏一起被藏掉）。排除之后，data 必然出自当前查询键。
   */
  const settled =
    !listQuery.isPending && !listQuery.isError && !listQuery.isPlaceholderData;
  // 拿到 total 与每页条数就能算出最后一页——当前页越界时，它是唯一能证明「必然有行」的落点。
  // total 为 null 表示这次查询没拿到总数（本页恒发 count_total），那就不猜。
  const lastPage =
    total === null ? null : Math.max(1, Math.ceil(total / pageSize));

  const emptyPage = settled && items.length === 0;
  // 一个数据源都没有：整屏归四步指引，不渲染空栅格/空表，工具栏也不渲染。
  // 只有 total 为 0 才是证据——「当前页是空的」不是（见下面的 pageOutOfRange）。
  const showGuide = emptyPage && !filtering && total === 0;
  // 搜索/筛选无结果：与上面区分开，保留工具栏并给「清除筛选」。同样只要 total 为 0 这个证据。
  const showNoResults = emptyPage && filtering && total === 0;
  // 结果集收缩（删除 / 重命名，或别处改了数据后回读）会让当前页越界：
  // items 空、total 却大于 0，而且页码排在最后一页之后。这与「真的没有匹配」是两回事，
  // 不能拿它说任何结论——把页码夹回最后一页（那一页必然有行）就够了。
  const pageOutOfRange = emptyPage && lastPage !== null && page > lastPage;

  useEffect(() => {
    if (pageOutOfRange && lastPage !== null) setPage(lastPage);
  }, [pageOutOfRange, lastPage, setPage]);

  /**
   * 预检回执说的是「刚刚建好 / 刚轮换过的那一个字段绑定」。它一旦已经不在了，
   * 就不只是过期难看：那句「数据源已创建」把人按在一个不存在的标识上。
   *
   * 判据只能是**这一页看到了整个结果集，而里面没有它**：有筛选、或这只是结果集的
   * 一页，那都只是「没看到」，不是「不存在」。自己删掉的另在删除成功处当场清
   * （见 `confirmAction`）——那条路径连回读都不用等。
   *
   * 表级化之后 `source_key` 挂在 `fields[]` 上，所以「还在不在」要连绑定一起看。
   */
  const precheckOutdated =
    precheck !== null &&
    !listQuery.isPending &&
    !listQuery.isError &&
    !listQuery.isPlaceholderData &&
    !filtering &&
    total !== null &&
    items.length >= total &&
    !items.some((item) =>
      item.fields.some((binding) => binding.sourceKey === precheck.sourceKey),
    );

  /**
   * 回读列表。
   *
   * `shrink: true` = 这次变更**可能让当前结果集变小**，必须同时把页码归位：不归位的
   * 后果不是报错，而是停在一个不存在的页码上——当前页为空，界面就会把「数据缩水了」
   * 说成「没有匹配」。两条路径都属于这一类：
   * 1. 删除：那一行直接没了；
   * 2. 改名称：改完可能不再命中当前搜索词。
   * 新建不在此列（结果集只会变大），走默认的 false。
   */
  async function refreshList({ shrink = false }: { shrink?: boolean } = {}) {
    if (shrink) setPage(1);
    // 前缀失效：本次列表的所有 key 一起作废（搜索词/页码不同的那些也在内）。
    await queryClient.invalidateQueries({
      queryKey: feishuQueryKeys.datasources(),
    });
  }

  function openCreate() {
    setEditError(null);
    setActionError(null);
    setWizardOpen(true);
  }

  function closeWizard() {
    setWizardOpen(false);
  }

  /// 列表项上的「编辑」入口：那一行握着名称与绑定，编辑态因此不必再查一次。
  function openEditForItem(item: DatasourceItem) {
    const datasourceId = idOf(item);
    if (datasourceId === null) return;
    setEditError(null);
    setActionError(null);
    setEditTarget({ datasourceId, title: item.title, bindings: item.fields });
  }

  function requestDelete(item: DatasourceItem) {
    const datasourceId = idOf(item);
    if (datasourceId === null) return;
    setActionError(null);
    setConfirmTarget({ datasourceId, title: item.title });
  }

  function openDetail(item: DatasourceItem) {
    // 详情页按**表级主键**路由：一条数据源的身份是 `id`，而 `source_key` 属于
    // 某一条字段绑定（一条表级行有 N 个）。曾经这里传的是首条绑定的 `source_key`，
    // 于是详情页只能看那一个字段，且它自己的查询还得反过来按 `source_key` 找数据源。
    //
    // 没有绑定不再挡路：详情页本来就要显示「这条源还没勾字段」，那比一个点不动的
    // 行更该被看见。
    if (item.id === null) {
      setActionNotice(
        "这条数据源没有主键，打不开详情页——多半是列表响应缺了 `id`，刷新一次界面目录与列表再试。",
      );
      return;
    }
    void navigate(`/feishu/datasources/${item.id}`);
  }

  /**
   * 用刚生成的明文 Token 试拉一次选项。
   *
   * 这是**唯一**能验证一次凭据的时刻：明文只在创建与轮换的响应里出现，
   * 服务端只存 SHA-256 摘要，事后无法再校验。失败也不阻塞上面那次操作的结果。
   */
  async function runPrecheck(
    datasourceId: number,
    sourceKey: string,
    title: string,
    token: string,
    mode: TokenPrecheckMode,
  ) {
    setPrecheck({ datasourceId, sourceKey, title, mode, result: null });
    setPrecheckPending(true);
    try {
      const result = await actions.precheckToken(sourceKey, token);
      setPrecheck({ datasourceId, sourceKey, title, mode, result });
    } finally {
      setPrecheckPending(false);
    }
  }

  /// 向导提交成功：关掉向导、回读列表，若服务端回了一组凭据就拿第一条跑一次预检。
  function submitWizard(
    created: CreatedTable,
    submission: CreateTableSubmission,
  ) {
    setWizardOpen(false);
    void (async () => {
      await refreshList();
      const first = created.credentials[0];
      if (first !== undefined) {
        await runPrecheck(
          created.datasourceId,
          first.sourceKey,
          submission.title,
          first.token,
          "create",
        );
      } else {
        setActionNotice(
          `已创建「${submission.title}」，但它一条字段绑定都没有——回配置向导勾几列。`,
        );
      }
    })();
  }

  /// 编辑提交：名称之外把这一刻的绑定**原样**带回（只送启用中的那几条）。
  function submitEdit(submission: { datasourceId: number; title: string }) {
    if (editTarget === null) return;
    setEditError(null);
    setActionError(null);
    setEditPending(true);
    void (async () => {
      try {
        await actions.updateDatasourceTable({
          datasourceId: submission.datasourceId,
          title: submission.title,
          fields: enabledBindingInputs({ fields: editTarget.bindings }),
        });
        setEditTarget(null);
        // 改名称可能让这一行不再命中当前搜索词：结果集可能收缩。
        await refreshList({ shrink: true });
        setActionNotice(`已更新「${submission.title}」`);
      } catch (cause) {
        // 服务端拒绝类错误直接回显后端原文，留在对话框里让人改。
        setEditError(messageOf(cause));
      } finally {
        setEditPending(false);
      }
    })();
  }

  function confirmDelete() {
    if (confirmTarget === null) return;
    const { datasourceId, title } = confirmTarget;
    setActionError(null);
    setActionNotice(null);
    setDeletePending(true);
    void (async () => {
      try {
        await actions.deleteDatasourceTable(datasourceId);
        setConfirmTarget(null);
        // 回执指着就是这条源的字段绑定：删掉之后它还亮在屏幕上就没有了指向。
        setPrecheck(null);
        // 后端会连带停用其下全部选项：详情页那份缓存一并作废。
        await queryClient.invalidateQueries({
          queryKey: feishuQueryKeys.datasources(),
        });
        await refreshList({ shrink: true });
        setActionNotice(`已删除「${title}」，其下选项已同时停用且不可恢复。`);
      } catch (cause) {
        setConfirmTarget(null);
        setActionError(messageOf(cause));
      } finally {
        setDeletePending(false);
      }
    })();
  }

  return (
    <main className="mx-auto w-full max-w-6xl space-y-6 p-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="space-y-1">
          <h1 className="text-xl font-semibold">飞书数据源</h1>
          <p className="text-sm text-muted-foreground">
            把飞书审批控件的外部选项接在这里；选项由多维表格自动推送，控制台只读。
          </p>
        </div>
        {actions.canWrite ? (
          <Button onClick={openCreate}>
            <Plus aria-hidden="true" />
            添加数据源
          </Button>
        ) : null}
      </div>

      {precheck && !precheckOutdated ? (
        <TokenPrecheckNotice
          sourceKey={precheck.sourceKey}
          result={precheck.result}
          mode={precheck.mode}
          pending={precheckPending}
          onRotate={() => {
            // 轮换是**逐字段**的，且必须二次确认：所以这里把人送到详情页的
            // 凭据清单，而不是在这一屏直接换掉（那会作废已配好的控件）。
            //
            // 跳转用的是**表级主键**：详情页按 `:id` 路由，而 `source_key` 是一条
            // 绑定的标识（纯字母数字下划线，永远解析不成数字 id）——把它拼进 URL
            // 只会让详情页判定「地址里没有有效的主键」，一个请求都不发。
            void navigate(`/feishu/datasources/${precheck.datasourceId}`);
          }}
        />
      ) : null}

      {actionError ? (
        <p
          role="alert"
          className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
        >
          {actionError}
        </p>
      ) : null}

      {actionNotice ? (
        <p
          aria-live="polite"
          className="rounded-md border border-border bg-muted/50 px-3 py-2 text-sm"
        >
          {actionNotice}
        </p>
      ) : null}

      {!actions.canRead ? (
        <p
          aria-live="polite"
          className="rounded-md border border-border bg-muted/50 px-3 py-2 text-sm"
        >
          当前身份没有查看飞书数据源的权限，请联系运维管理员开通。
        </p>
      ) : (
        <>
          {showGuide ? null : (
            <ListToolbar
              view={controller.state.view}
              onViewChange={controller.setView}
              search={controller.state.search}
              onSearchChange={controller.setSearch}
              status={controller.state.status}
              onStatusChange={controller.setStatus}
            />
          )}

          {listQuery.isError ? (
            <div
              role="alert"
              className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
            >
              <span>{messageOf(listQuery.error)}</span>
              <Button
                variant="outline"
                size="sm"
                onClick={() => void listQuery.refetch()}
              >
                <RefreshCw aria-hidden="true" />
                重试
              </Button>
            </div>
          ) : showGuide ? (
            <EmptyGuide canWrite={actions.canWrite} onAdd={openCreate} />
          ) : showNoResults ? (
            <NoResults onClear={controller.clearFilters} />
          ) : (
            <div className="space-y-4">
              {controller.state.view === "cards" ? (
                <DatasourceCardGrid
                  items={items}
                  canWrite={actions.canWrite}
                  pending={listQuery.isPending}
                  onOpen={openDetail}
                  onEdit={openEditForItem}
                  onDelete={requestDelete}
                />
              ) : (
                <DatasourceLedger
                  items={items}
                  // 传用户选的那一份（单条），不是查询快照里补过收尾键的那份——
                  // 否则台账会把 source_key 也画成「用户在排的列」。
                  orderBy={controller.state.orderBy}
                  canWrite={actions.canWrite}
                  pending={listQuery.isPending}
                  onOpen={openDetail}
                  onSort={controller.setSort}
                  onEdit={openEditForItem}
                  onDelete={requestDelete}
                />
              )}
              {/*
                分页控件由 total 决定是否渲染，不看当前页有几行：结果集收缩后当前页恰好为空，
                这时它正是页内唯一的自救入口（没有它，用户连「第 2 / 1 页」都看不到）。
              */}
              {total !== null && total > 0 ? (
                <ListPagination
                  page={controller.state.page}
                  pageSize={controller.state.pageSize}
                  total={total}
                  pending={listQuery.isFetching}
                  onPageChange={controller.setPage}
                  onPageSizeChange={controller.setPageSize}
                />
              ) : null}
            </div>
          )}
        </>
      )}

      <DatasourceTableWizard
        open={wizardOpen}
        client={wizardClient}
        onCancel={closeWizard}
        onSubmitted={submitWizard}
      />

      <DatasourceEditDialog
        open={editTarget !== null}
        datasourceId={editTarget?.datasourceId ?? 0}
        initialTitle={editTarget?.title ?? ""}
        enabledBindingCount={
          editTarget === null
            ? 0
            : enabledBindingInputs({ fields: editTarget.bindings }).length
        }
        pending={editPending}
        serverError={editError}
        onSubmit={submitEdit}
        onCancel={() => {
          setEditTarget(null);
          setEditError(null);
        }}
      />

      <ConfirmDialog
        open={confirmTarget !== null}
        kind="delete"
        // 删除不可逆：正文之外必须看得见删的是哪一条。表级世界的身份是主键，
        // 所以两个都带上——名称给人认，主键给「两条同名时」分辨。
        target={
          confirmTarget === null
            ? undefined
            : `${confirmTarget.title}（#${confirmTarget.datasourceId}）`
        }
        pending={deletePending}
        onConfirm={confirmDelete}
        onCancel={() => setConfirmTarget(null)}
      />
    </main>
  );
}

/// 落点 1：一个数据源都没有时，四步指引就是页面正文（不渲染空栅格/空表）。
function EmptyGuide({
  canWrite,
  onAdd,
}: {
  canWrite: boolean;
  onAdd: () => void;
}) {
  return (
    <section className="rounded-xl border border-border bg-card p-5">
      <div className="space-y-1">
        <h2 className="text-base font-medium">还没有数据源</h2>
        <p className="text-sm text-muted-foreground">
          先在飞书审批后台配好控件并拿到
          Token，再回来建第一个。四步是这样接起来的：
        </p>
      </div>
      <ol className="mt-4 space-y-4">
        {FOUR_STEPS.map((step, index) => (
          <li key={step.title} className="flex gap-3">
            <span className="mt-0.5 flex size-5 shrink-0 items-center justify-center rounded-full border border-border text-xs font-medium tabular-nums">
              {index + 1}
            </span>
            <div className="space-y-1">
              <h3 className="text-sm font-medium">{step.title}</h3>
              <p className="text-sm text-muted-foreground">{step.detail}</p>
            </div>
          </li>
        ))}
      </ol>
      <div className="mt-5">
        {canWrite ? (
          <Button onClick={onAdd}>
            <Plus aria-hidden="true" />
            添加数据源
          </Button>
        ) : (
          <p className="text-xs text-muted-foreground">
            当前身份没有创建数据源的权限，请联系运维管理员。
          </p>
        )}
      </div>
    </section>
  );
}

/// 「搜索无结果」：与「一个数据源都没有」区分开，给一个能立刻脱困的动作。
function NoResults({ onClear }: { onClear: () => void }) {
  return (
    <div className="space-y-3 rounded-xl border border-border bg-card p-5">
      <div className="space-y-1">
        <h2 className="text-base font-medium">没有匹配的数据源</h2>
        <p className="text-sm text-muted-foreground">
          当前的搜索词或状态筛选下没有结果。换个关键词，或者直接清掉筛选条件看全部。
        </p>
      </div>
      <Button variant="outline" size="sm" onClick={onClear}>
        清除筛选
      </Button>
    </div>
  );
}

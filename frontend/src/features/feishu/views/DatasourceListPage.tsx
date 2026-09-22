/**
 * 飞书数据源列表页（路由 `/feishu/datasources`，路由级 lazy → 必须 default 导出）。
 *
 * 页面负责的是「接线」：查询状态交给 `useListQuery()`，数据交给 `useDatasourceList()`，
 * 渲染交给 9 个受控组件。三个不变量写在这里而不是组件里：
 *
 * 1. **卡片与台账是同一份查询状态、同一份数据的两种渲染**——切视图只换渲染方式，
 *    不重新发请求、不丢搜索词与页码（设计 §5.4）；
 * 2. **权限门控是「不渲染」而不是「禁用」**：无写权限时「添加数据源」与列表项上的
 *    「⋯」菜单整个不出现（禁用表示「此刻不可用」，而这里表示「这个入口不属于你」）；
 * 3. **指引只在卡住的那一刻出现**：空态（一个数据源都没有）与「搜索无结果」是两个
 *    不同的分支——前者整屏归四步建档，连工具栏都不渲染；后者要保留工具栏并给
 *    「清除筛选」。
 *
 * 提交后一律**回读列表**而不做乐观更新：`create/update` 只回 `{source_key}` /
 * `{affected}`，不含最新记录，既然回读是必需的，乐观更新只买到一次闪烁。
 *
 * 空态判定遵循「只说能证明的话」：`total` 为 0 才有资格说「没有数据源」/「没有匹配」；
 * 「当前页为空」不构成任何结论（结果集可能只是缩小了），那是页码越界，夹回有效页即可。
 */

import { useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { useQueryClient } from "@tanstack/react-query";
import { Plus, RefreshCw } from "lucide-react";

import { Button } from "@/shared/ui/button";

import { feishuQueryKeys, useDatasourceList, useFeishuActions } from "../api";
import { ConfirmDialog, type ConfirmKind } from "../components/ConfirmDialog";
import { DatasourceCardGrid } from "../components/DatasourceCardGrid";
import {
  DatasourceFormDialog,
  type DatasourceFormSubmission,
  type DatasourceCoordinatesFormValue,
} from "../components/DatasourceFormDialog";
import { DatasourceLedger } from "../components/DatasourceLedger";
import { ListPagination } from "../components/ListPagination";
import { ListToolbar } from "../components/ListToolbar";
import {
  TokenPrecheckNotice,
  type TokenPrecheckMode,
} from "../components/TokenPrecheckNotice";
import { useListQuery } from "../list-query";
import { asIngestMode } from "../types";
import type { DatasourceItem, TokenPrecheckResult } from "../types";

/// 四步指引（设计 §5.5 的四步原文）：只写「去哪做、填什么」，不写接口路径与字段名。
const FOUR_STEPS: ReadonlyArray<{ title: string; detail: string }> = [
  {
    title: "在飞书审批后台配置控件",
    detail:
      "把单选或多选控件的取值方式改成「使用外部选项」，自定义一个 Token 并记住它——服务端只保存它的摘要，之后无法回显，请先复制到安全的地方。",
  },
  {
    title: "在这里建一个数据源",
    detail: "数据源标识会进接口地址；把上一步的 Token 粘贴进来。",
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

/// 编辑对话框的目标。字段都是基本类型，便于安全地作为 `DatasourceFormDialog` 的 effect 依赖。
type RenameTarget = {
  sourceKey: string;
  title: string;
  /// 「加密返回 / 默认语言」的现值：**只有读得到真实记录时才给**。
  /// 给了对话框才渲染对应的控件（能改的前提是界面知道现状）；拿不到就不渲染——
  /// 拿一个猜的值当现状写回去，等于把用户没说过的设置改掉。
  encryptEnabled?: boolean;
  /// **原样**给，不在这里归一化：后端对 `default_locale` 零校验，库里可能是 `zh-CN`
  /// 这种取值域以外的值，而它正是「所有语言下控件都取不到文案」的元凶。归一化会变成
  /// 未经确认地改写用户数据；对话框那边会让它留空、由用户显式选一项修好。
  defaultLocale?: string;
  /// 坐标现值。与上面两项同一取舍：**只有读得到真实记录时才给**，给了才渲染坐标区。
  coordinates?: DatasourceCoordinatesFormValue;
};

type FormTarget = { mode: "create" } | ({ mode: "rename" } & RenameTarget);

/// 把一行记录折成编辑对话框的目标。
function renameTargetOf(item: DatasourceItem): RenameTarget {
  return {
    sourceKey: item.sourceKey,
    title: item.title,
    encryptEnabled: item.encryptEnabled,
    defaultLocale: item.defaultLocale,
    // 空值统一折成空串：对话框里空串表示「清空该坐标」，而 `null` 与「没这个键」
    // 在受控输入里都会退化成非受控，必须给一个确定的字符串。
    coordinates: {
      ingestMode: asIngestMode(item.ingestMode) ?? "push",
      bitableBaseToken: item.bitableBaseToken ?? "",
      bitableTableId: item.bitableTableId ?? "",
      bitableViewId: item.bitableViewId ?? "",
      bitableFieldName: item.bitableFieldName ?? "",
      linkageMapping: item.linkageMapping ?? "",
    },
  };
}

/// 停用/启用/删除的确认目标（三个动作都只需要标识 + 给回执用的名称）。
type ConfirmTarget = {
  kind: ConfirmKind;
  sourceKey: string;
  title: string;
};

/// 创建/轮换之后的预检状态。`result` 为 null 表示还在检——这一屏必须显示真实标识。
type PrecheckState = {
  sourceKey: string;
  title: string;
  mode: TokenPrecheckMode;
  result: TokenPrecheckResult | null;
};

function messageOf(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

export default function DatasourceListPage() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const actions = useFeishuActions();
  const controller = useListQuery();
  const listQuery = useDatasourceList(controller.query);

  const [formTarget, setFormTarget] = useState<FormTarget | null>(null);
  const [confirmTarget, setConfirmTarget] = useState<ConfirmTarget | null>(
    null,
  );
  const [precheck, setPrecheck] = useState<PrecheckState | null>(null);
  const [precheckPending, setPrecheckPending] = useState(false);
  const [createPending, setCreatePending] = useState(false);
  const [updatePending, setUpdatePending] = useState(false);
  const [deletePending, setDeletePending] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
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
  // 结果集收缩（删除 / 停用启用 / 重命名，或别处改了数据后回读）会让当前页越界：
  // items 空、total 却大于 0，而且页码排在最后一页之后。这与「真的没有匹配」是两回事，
  // 不能拿它说任何结论——把页码夹回最后一页（那一页必然有行）就够了。
  const pageOutOfRange = emptyPage && lastPage !== null && page > lastPage;

  useEffect(() => {
    if (pageOutOfRange && lastPage !== null) setPage(lastPage);
  }, [pageOutOfRange, lastPage, setPage]);

  /**
   * 预检回执说的是「刚刚建好 / 刚轮换过的那一个数据源」。它一旦已经不在了，
   * 就不只是过期难看：那句「数据源已创建」把人按在一个不存在的标识上，
   * 「重新填写 Token」更是直接送进死路（`update_datasource` 会回「数据源不存在」）。
   *
   * 判据只能是**这一页看到了整个结果集，而里面没有它**：有筛选、或这只是结果集的
   * 一页，那都只是「没看到」，不是「不存在」。自己删掉的另在删除成功处当场清
   * （见 `confirmAction`）——那条路径连回读都不用等。
   *
   * 这里不借用 `settled`：它描述的是「空态判定的证据强度」，跟本判据不是一回事。
   */
  const precheckOutdated =
    precheck !== null &&
    !listQuery.isPending &&
    !listQuery.isError &&
    !listQuery.isPlaceholderData &&
    !filtering &&
    total !== null &&
    items.length >= total &&
    !items.some((item) => item.sourceKey === precheck.sourceKey);

  const renameTarget = formTarget?.mode === "rename" ? formTarget : null;

  /**
   * 回读列表。
   *
   * `shrink: true` = 这次变更**可能让当前结果集变小**，必须同时把页码归位：不归位的
   * 后果不是报错，而是停在一个不存在的页码上——当前页为空，界面就会把「数据缩水了」
   * 说成「没有匹配」。三条路径都属于这一类，所以都走这一个入口：
   * 1. 删除：那一行直接没了；
   * 2. 停用 / 启用：在按状态筛选的结果集里，这一行会离开当前的那个集合；
   * 3. 重命名：改完可能不再命中当前搜索词。
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
    setFormError(null);
    setActionError(null);
    setFormTarget({ mode: "create" });
  }

  function openRename(target: RenameTarget) {
    setFormError(null);
    setActionError(null);
    setFormTarget({ mode: "rename", ...target });
  }

  /// 列表项上的「重命名」入口：那一行握着两项的现值，编辑态因此能改它们。
  function openRenameForItem(item: DatasourceItem) {
    openRename(renameTargetOf(item));
  }

  /// 回执里的「重新填写 Token」入口：列表里若还握着那一行，就把现值一并带上；
  /// 找不到那一行（被筛掉 / 不在这一页）就只给名称与 Token 两项。
  function openRenameFromPrecheck(sourceKey: string, title: string) {
    const item = items.find((candidate) => candidate.sourceKey === sourceKey);
    openRename(item ? renameTargetOf(item) : { sourceKey, title });
  }

  function closeForm() {
    setFormTarget(null);
    setFormError(null);
  }

  function openDetail(item: DatasourceItem) {
    void navigate(`/feishu/datasources/${item.sourceKey}`);
  }

  /// 「已停用」的数据源直接给「启用」：只多一个条目，却消掉了「唯一恢复路径
  /// 藏在看不见的菜单里」这个状态。
  function requestToggle(item: DatasourceItem) {
    setActionError(null);
    setConfirmTarget({
      kind: item.status === "active" ? "disable" : "enable",
      sourceKey: item.sourceKey,
      title: item.title,
    });
  }

  function requestDelete(item: DatasourceItem) {
    setActionError(null);
    setConfirmTarget({
      kind: "delete",
      sourceKey: item.sourceKey,
      title: item.title,
    });
  }

  /**
   * 用刚填的明文 Token 试拉一次选项。
   *
   * 必须排在创建/轮换**成功之后**：该端点按 `source_key` 查库再比对存储的哈希，
   * 数据源行得先存在。失败也不阻塞上面那次操作的结果。
   */
  async function runPrecheck(
    sourceKey: string,
    title: string,
    token: string,
    mode: TokenPrecheckMode,
  ) {
    setPrecheck({ sourceKey, title, mode, result: null });
    setPrecheckPending(true);
    try {
      const result = await actions.precheckToken(sourceKey, token);
      setPrecheck({ sourceKey, title, mode, result });
    } finally {
      setPrecheckPending(false);
    }
  }

  function submitForm(submission: DatasourceFormSubmission) {
    setFormError(null);
    setActionError(null);
    setActionNotice(null);

    if (submission.mode === "create") {
      setCreatePending(true);
      void (async () => {
        try {
          const created = await actions.createDatasource({
            sourceKey: submission.sourceKey,
            title: submission.title,
            token: submission.token,
            encryptEnabled: submission.encryptEnabled,
            defaultLocale: submission.defaultLocale,
            coordinates: submission.coordinates,
          });
          setFormTarget(null);
          await refreshList();
          await runPrecheck(
            created.sourceKey,
            submission.title,
            submission.token,
            "create",
          );
        } catch (cause) {
          // 服务端拒绝类错误直接回显后端原文，留在对话框里让人改。
          setFormError(messageOf(cause));
        } finally {
          setCreatePending(false);
        }
      })();
      return;
    }

    setUpdatePending(true);
    void (async () => {
      try {
        await actions.updateDatasource({
          sourceKey: submission.sourceKey,
          title: submission.title,
          // 重命名留空 Token 时整个键不出现（省略 = 不轮换），传空串会被后端拒绝。
          ...(submission.token === undefined
            ? {}
            : { token: submission.token }),
          // 编辑态新增的两项：对话框**渲染了才带上**。没渲染就是读不到现值，
          // 省略 = 保持原值，正好对上「不知道现状就别动它」。
          ...(submission.encryptEnabled === undefined
            ? {}
            : { encryptEnabled: submission.encryptEnabled }),
          ...(submission.defaultLocale === undefined
            ? {}
            : { defaultLocale: submission.defaultLocale }),
          // 坐标区没渲染就不带（同上）；渲染了就整组带上——**空串是要发的**，
          // 它表示「清空这个坐标」，而你刻意清空一个填错的值是合法操作。
          ...(submission.coordinates === undefined
            ? {}
            : { coordinates: submission.coordinates }),
        });
        setFormTarget(null);
        // 重命名可能让这一行不再命中当前搜索词：结果集可能收缩。
        await refreshList({ shrink: true });
        if (submission.token === undefined) {
          setActionNotice(`已更新「${submission.title}」`);
        } else {
          await runPrecheck(
            submission.sourceKey,
            submission.title,
            submission.token,
            "rotate",
          );
        }
      } catch (cause) {
        setFormError(messageOf(cause));
      } finally {
        setUpdatePending(false);
      }
    })();
  }

  function confirmAction() {
    if (!confirmTarget) return;
    const { kind, sourceKey, title } = confirmTarget;
    setActionError(null);
    setActionNotice(null);

    if (kind === "delete") {
      setDeletePending(true);
      void (async () => {
        try {
          await actions.deleteDatasource(sourceKey);
          setConfirmTarget(null);
          // 回执指着就是这一条：删掉之后它还写着「数据源已创建」，
          // 而「重新填写 Token」会把人送进一个已经不存在的标识。当场清掉它。
          setPrecheck((current) =>
            current?.sourceKey === sourceKey ? null : current,
          );
          // 后端会连带停用其下全部选项：详情页那份缓存一并作废。
          await queryClient.invalidateQueries({
            queryKey: feishuQueryKeys.options(sourceKey),
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
      return;
    }

    setUpdatePending(true);
    void (async () => {
      try {
        await actions.updateDatasource({
          sourceKey,
          status: kind === "disable" ? "disabled" : "active",
        });
        setConfirmTarget(null);
        // 停用/启用会让这一行离开（或回到）当前的状态筛选结果集：同样可能收缩。
        await refreshList({ shrink: true });
        setActionNotice(
          kind === "disable" ? `已停用「${title}」` : `已启用「${title}」`,
        );
      } catch (cause) {
        setConfirmTarget(null);
        setActionError(messageOf(cause));
      } finally {
        setUpdatePending(false);
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
          onRotate={() =>
            openRenameFromPrecheck(precheck.sourceKey, precheck.title)
          }
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
                  onRename={openRenameForItem}
                  onToggleStatus={requestToggle}
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
                  onRename={openRenameForItem}
                  onToggleStatus={requestToggle}
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

      <DatasourceFormDialog
        open={formTarget !== null}
        mode={formTarget?.mode ?? "create"}
        initialSourceKey={renameTarget?.sourceKey ?? ""}
        initialTitle={renameTarget?.title ?? ""}
        initialEncryptEnabled={renameTarget?.encryptEnabled}
        initialDefaultLocale={renameTarget?.defaultLocale}
        initialCoordinates={renameTarget?.coordinates}
        pending={createPending || updatePending}
        serverError={formError}
        onSubmit={submitForm}
        onCancel={closeForm}
      />

      <ConfirmDialog
        open={confirmTarget !== null}
        kind={confirmTarget?.kind ?? "delete"}
        sourceKey={confirmTarget?.sourceKey}
        pending={updatePending || deletePending}
        onConfirm={confirmAction}
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

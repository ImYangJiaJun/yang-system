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
 */

import { useState } from "react";
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
} from "../components/DatasourceFormDialog";
import { DatasourceLedger } from "../components/DatasourceLedger";
import { ListPagination } from "../components/ListPagination";
import { ListToolbar } from "../components/ListToolbar";
import {
  TokenPrecheckNotice,
  type TokenPrecheckMode,
} from "../components/TokenPrecheckNotice";
import { useListQuery } from "../list-query";
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

/// 新建/重命名对话框的目标。重命名只需要两个基本类型初值，便于作为 effect 依赖。
type FormTarget =
  { mode: "create" } | { mode: "rename"; sourceKey: string; title: string };

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
  // 判定用去抖后的查询快照：它才是产出这份结果集的那次请求。
  const filtering =
    controller.query.search.trim() !== "" || controller.query.status !== "all";
  const loaded = !listQuery.isPending && !listQuery.isError;
  // 一个数据源都没有：整屏归四步指引，不渲染空栅格/空表，工具栏也不渲染。
  const showGuide = loaded && !filtering && items.length === 0;
  // 搜索/筛选无结果：与上面区分开，保留工具栏并给「清除筛选」。
  const showNoResults = loaded && filtering && items.length === 0;

  const renameTarget = formTarget?.mode === "rename" ? formTarget : null;

  async function refreshList() {
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

  function openRename(target: { sourceKey: string; title: string }) {
    setFormError(null);
    setActionError(null);
    setFormTarget({ mode: "rename", ...target });
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
        });
        setFormTarget(null);
        await refreshList();
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
          // 后端会连带停用其下全部选项：详情页那份缓存一并作废。
          await queryClient.invalidateQueries({
            queryKey: feishuQueryKeys.options(sourceKey),
          });
          // 结果集变小了：回到第 1 页，否则会停在一个不存在的页码上。
          controller.setPage(1);
          await refreshList();
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
        await refreshList();
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

      {precheck ? (
        <TokenPrecheckNotice
          sourceKey={precheck.sourceKey}
          result={precheck.result}
          mode={precheck.mode}
          pending={precheckPending}
          onRotate={() =>
            openRename({
              sourceKey: precheck.sourceKey,
              title: precheck.title,
            })
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
                  onRename={openRename}
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
                  onRename={openRename}
                  onToggleStatus={requestToggle}
                  onDelete={requestDelete}
                />
              )}
              {items.length > 0 ? (
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

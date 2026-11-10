/**
 * 审批回写控制台：「规则」与「运行记录」两态。
 *
 * - 规则列表：名称 / 审批 / 触发 / 目标表 / 状态列 / 启用 / 更新时间；
 *   操作新建（六步向导）、编辑、删除、检查、查看运行。
 * - 运行记录：按规则下钻，展示 `write_status` / `mark_status` / 目标记录 /
 *   错误，支持人工重试与强制重建。
 *
 * 独立侧边栏页面（`/feishu/approval-export`）；规则与运行日志都按 operation id
 * 门控（目录里有没有那一粒 Action），不解析 JWT，也不把
 * `presentation.availability` 当权限。
 */

import { useEffect, useState } from "react";
import { useSearchParams } from "react-router";
import { Plus } from "lucide-react";
import { useQueryClient } from "@tanstack/react-query";
import { useUiCatalog } from "@/engine";
import { Button } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import { Input } from "@/shared/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/shared/ui/select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/shared/ui/table";
import { useToast } from "@/shared/lib/toast";
import {
  EXPORT_OPERATION_IDS,
  feishuQueryKeys,
  hasOperation,
  useApprovalExportActions,
  useApprovalExportRuleList,
  useApprovalExportRunList,
  useApprovalExportWizardClient,
} from "../api";
import { ApprovalExportRuleDialog } from "../components/ApprovalExportRuleDialog";
import { ListPagination } from "../components/ListPagination";
import { StatusBadge, type ToneName } from "../components/StatusBadge";
import { PAGE_SIZE_OPTIONS, useDebouncedValue } from "../list-query";
import {
  EXPORT_WRITE_STATUS_OPTIONS,
  exportMarkStatusLabel,
  exportRowFilterLabel,
  exportTriggerModeLabel,
  exportWriteStatusLabel,
  formatUnixSeconds,
  isExportRunForceRecreatable,
  isExportRunRetryable,
  type ApprovalExportCheckResult,
  type ApprovalExportRule,
  type ApprovalExportRun,
} from "../types";

function messageOf(cause: unknown) {
  return cause instanceof Error ? cause.message : String(cause);
}

function writeStatusTone(status: string): ToneName {
  if (status === "written") return "positive";
  if (status === "partial" || status === "retry" || status === "review")
    return "warning";
  if (status === "terminal" || status === "skipped") return "danger";
  if (status === "writing") return "info";
  return "neutral";
}

function markStatusTone(status: string): ToneName {
  if (status === "valid") return "positive";
  if (status === "rolled_back" || status === "marking") return "warning";
  if (
    status === "rejected" ||
    status === "canceled" ||
    status === "reverted" ||
    status === "deleted" ||
    status === "mark_failed"
  )
    return "danger";
  return "neutral";
}

export default function ApprovalExportPage() {
  const [params, setParams] = useSearchParams();
  const queryClient = useQueryClient();
  const toast = useToast();
  const actions = useApprovalExportActions();
  const wizardClient = useApprovalExportWizardClient();
  const { data: catalog } = useUiCatalog();
  // 写侧入口按各自那一粒 operation id 门控：保存 / 体检要打飞书（凭证缺失时
  // 服务端不注册）；删除 / 重试 / 重建只读写本库，独立注册。
  const canCreate = hasOperation(catalog, EXPORT_OPERATION_IDS.createRule);
  const canDelete = hasOperation(catalog, EXPORT_OPERATION_IDS.deleteRule);
  const canListRuns = hasOperation(catalog, EXPORT_OPERATION_IDS.listRuns);

  const rawRule = params.get("rule");
  const parsedRule = rawRule === null ? null : Number(rawRule);
  const ruleId =
    parsedRule !== null && Number.isSafeInteger(parsedRule) && parsedRule > 0
      ? parsedRule
      : null;
  // 独立页面的两个视图：规则 / 日志。`rule` 仍表示日志下钻到某条规则。
  const logsMode = params.get("view") === "logs" || ruleId !== null;
  const pageValue = Number(params.get("page") ?? 1);
  const page = Number.isSafeInteger(pageValue) && pageValue > 0 ? pageValue : 1;
  const sizeValue = Number(params.get("pageSize") ?? 10);
  const pageSize = PAGE_SIZE_OPTIONS.some((size) => size === sizeValue)
    ? sizeValue
    : 10;
  const search = params.get("search") ?? "";
  const debouncedSearch = useDebouncedValue(search, 300);
  const writeFilter = params.get("write") ?? "";
  const instanceFilter = params.get("instance") ?? "";

  const [wizardOpen, setWizardOpen] = useState(false);
  const [editTarget, setEditTarget] = useState<
    ApprovalExportRule | undefined
  >();
  const [deleteTarget, setDeleteTarget] = useState<ApprovalExportRule | null>(
    null,
  );
  const [pending, setPending] = useState(false);
  const [checking, setChecking] = useState(false);
  const [checkResult, setCheckResult] =
    useState<ApprovalExportCheckResult | null>(null);
  const [runAction, setRunAction] = useState<{
    run: ApprovalExportRun;
    kind: "retry" | "recreate";
  } | null>(null);

  const rulesQuery = useApprovalExportRuleList(
    { page, pageSize, search: debouncedSearch },
    { enabled: !logsMode },
  );
  const runsQuery = useApprovalExportRunList(
    {
      page,
      pageSize,
      ruleId: ruleId ?? undefined,
      instanceCode: instanceFilter,
      writeStatus: writeFilter,
    },
    { enabled: logsMode },
  );

  const items = rulesQuery.data?.items ?? [];
  const total = rulesQuery.data?.total ?? null;
  const lastPage =
    total === null ? null : Math.max(1, Math.ceil(total / pageSize));
  const runs = runsQuery.data?.items ?? [];
  const runsTotal = runsQuery.data?.total ?? null;
  const runsLastPage =
    runsTotal === null ? null : Math.max(1, Math.ceil(runsTotal / pageSize));

  function changeParams(patch: Record<string, string | null>, replace = false) {
    setParams(
      (current) => {
        const next = new URLSearchParams(current);
        for (const [key, value] of Object.entries(patch)) {
          if (value === null) next.delete(key);
          else next.set(key, value);
        }
        return next;
      },
      { replace },
    );
  }

  // 页码越界时回到最后一页（删除、筛选把结果集缩小后会出现）。
  useEffect(() => {
    const last = logsMode ? runsLastPage : lastPage;
    const busy = logsMode
      ? runsQuery.isPending || runsQuery.isError
      : rulesQuery.isPending || rulesQuery.isError;
    if (busy || last === null || page <= last) return;
    setParams(
      (current) => {
        const next = new URLSearchParams(current);
        next.set("page", String(last));
        return next;
      },
      { replace: true },
    );
  }, [
    logsMode,
    lastPage,
    runsLastPage,
    page,
    rulesQuery.isPending,
    rulesQuery.isError,
    runsQuery.isPending,
    runsQuery.isError,
    setParams,
  ]);

  async function refresh() {
    await queryClient.invalidateQueries({
      queryKey: feishuQueryKeys.approvalExportRules(),
    });
    await queryClient.invalidateQueries({
      queryKey: feishuQueryKeys.approvalExportRuns(),
    });
  }

  async function confirmDelete() {
    if (!deleteTarget) return;
    setPending(true);
    try {
      const result = await actions.deleteRule(deleteTarget.id);
      toast.success(
        `已删除「${deleteTarget.title}」${
          result.deletedMaps > 0
            ? `，同时删除 ${result.deletedMaps} 条字段映射`
            : ""
        }；运行记录保留`,
      );
      setDeleteTarget(null);
      if (ruleId === deleteTarget.id)
        changeParams({ view: null, rule: null, page: "1" });
      await refresh();
    } catch (cause) {
      toast.error(messageOf(cause));
    } finally {
      setPending(false);
    }
  }

  async function checkAll() {
    setChecking(true);
    try {
      const result = await actions.checkRules();
      if (result.problems.length === 0) {
        setCheckResult(null);
        toast.success(`全部 ${result.total} 条启用规则检查通过。`);
      } else {
        setCheckResult(result);
      }
    } catch (cause) {
      toast.error(messageOf(cause));
    } finally {
      setChecking(false);
    }
  }

  /// 向导内每保存成功一次就会回调一次（可能先创建再更新）：这里只回读列表，
  /// **不关对话框**——第 6 步还要留在原地展示试算结果。
  function submitWizard() {
    void refresh();
  }

  async function confirmRunAction() {
    if (!runAction) return;
    setPending(true);
    try {
      if (runAction.kind === "retry") {
        const result = await actions.retryRun(runAction.run.id);
        toast.success(
          `已重新排队（原状态 ${exportWriteStatusLabel(
            result.previousStatus || runAction.run.writeStatus,
          )}）`,
        );
      } else {
        const result = await actions.forceRecreateRun(runAction.run.id);
        toast.success(
          `已排入第 ${result.generation} 代次，worker 会用新幂等 Token 重新创建目标记录`,
        );
      }
      setRunAction(null);
      await runsQuery.refetch();
    } catch (cause) {
      toast.error(messageOf(cause));
    } finally {
      setPending(false);
    }
  }

  if (!actions.canRead) {
    return (
      <p className="p-6">当前身份没有查看审批回写规则的权限，请联系管理员。</p>
    );
  }

  return (
    <div className="mx-auto max-w-6xl space-y-6 p-4 sm:p-6 lg:p-8">
      {logsMode ? (
        <>
          <div className="flex flex-wrap items-start justify-between gap-4">
            <div>
              <Button
                variant="ghost"
                className="mb-2 h-auto p-0"
                onClick={() =>
                  changeParams({
                    view: null,
                    rule: null,
                    page: "1",
                    write: null,
                    instance: null,
                  })
                }
              >
                <span aria-hidden="true">←</span> 返回规则列表
              </Button>
              <h1 className="text-2xl font-semibold tracking-tight">
                {ruleId === null
                  ? "回写日志"
                  : `回写运行记录 · 规则 #${ruleId}`}
              </h1>
              <p className="mt-2 text-sm text-muted-foreground">
                规则级行（__rule__）记录触发与条件状态；行级行（row:N）记录每条明细的写入与标记。
              </p>
            </div>
          </div>
          <div className="flex flex-wrap items-center gap-3">
            <Select
              value={writeFilter === "" ? "__all__" : writeFilter}
              onValueChange={(value) =>
                changeParams(
                  { write: value === "__all__" ? null : value, page: "1" },
                  true,
                )
              }
            >
              <SelectTrigger aria-label="写入状态筛选" className="w-44">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="__all__">全部写入状态</SelectItem>
                {EXPORT_WRITE_STATUS_OPTIONS.map((option) => (
                  <SelectItem key={option.value} value={option.value}>
                    {option.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Input
              className="w-full sm:w-64"
              aria-label="按实例 Code 筛选"
              placeholder="按实例 Code 筛选"
              value={instanceFilter}
              onChange={(event) =>
                changeParams(
                  { instance: event.target.value || null, page: "1" },
                  true,
                )
              }
            />
          </div>
          {!canListRuns ? (
            <p className="p-6">
              当前身份没有查看回写运行记录的权限，请联系管理员。
            </p>
          ) : runsQuery.isPending ? (
            <p role="status">正在读取运行记录…</p>
          ) : runsQuery.isError ? (
            <div role="alert">
              <p>暂时无法读取运行记录：{messageOf(runsQuery.error)}</p>
              <Button
                variant="outline"
                onClick={() => void runsQuery.refetch()}
              >
                重新加载
              </Button>
            </div>
          ) : runs.length === 0 ? (
            <div className="rounded-lg border p-8 text-center">
              <p className="font-medium">还没有运行记录</p>
              <p className="mt-2 text-sm text-muted-foreground">
                规则处理过的审批实例会在这里出现；也可先在规则列表用「检查」确认配置。
              </p>
            </div>
          ) : (
            <div className="space-y-4">
              <div className="overflow-x-auto rounded-lg border">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>实例 / 行键</TableHead>
                      <TableHead>触发节点</TableHead>
                      <TableHead>写入状态</TableHead>
                      <TableHead>标记状态</TableHead>
                      <TableHead>目标记录</TableHead>
                      <TableHead>尝试</TableHead>
                      <TableHead>最后错误</TableHead>
                      <TableHead>更新时间</TableHead>
                      <TableHead className="text-right">操作</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {runs.map((run) => (
                      <TableRow key={run.id}>
                        <TableCell className="whitespace-normal">
                          <p className="font-mono text-xs">
                            {run.instanceCode}
                          </p>
                          <p className="mt-1 text-xs text-muted-foreground">
                            {run.rowKey}
                          </p>
                        </TableCell>
                        <TableCell className="font-mono text-xs">
                          {run.triggerNodeId}
                        </TableCell>
                        <TableCell>
                          <StatusBadge tone={writeStatusTone(run.writeStatus)}>
                            {exportWriteStatusLabel(run.writeStatus)}
                          </StatusBadge>
                        </TableCell>
                        <TableCell>
                          <StatusBadge tone={markStatusTone(run.markStatus)}>
                            {exportMarkStatusLabel(run.markStatus)}
                          </StatusBadge>
                          {run.appliedLabel ? (
                            <p className="mt-1 text-xs text-muted-foreground">
                              {run.appliedLabel}
                            </p>
                          ) : null}
                        </TableCell>
                        <TableCell className="font-mono text-xs">
                          {run.targetRecordId ?? "—"}
                        </TableCell>
                        <TableCell className="tabular-nums">
                          {run.attempts}
                        </TableCell>
                        <TableCell className="max-w-64 whitespace-normal">
                          {run.lastError ? (
                            <span
                              className="line-clamp-2 text-xs text-destructive"
                              title={run.lastError}
                            >
                              {run.lastError}
                            </span>
                          ) : (
                            <span className="text-xs text-muted-foreground">
                              —
                            </span>
                          )}
                        </TableCell>
                        <TableCell className="text-xs">
                          {formatUnixSeconds(run.updatedAt)}
                        </TableCell>
                        <TableCell className="text-right">
                          <div className="flex justify-end gap-1">
                            {actions.canRetry &&
                            isExportRunRetryable(run.writeStatus) ? (
                              <Button
                                size="sm"
                                variant="outline"
                                onClick={() =>
                                  setRunAction({ run, kind: "retry" })
                                }
                              >
                                重试
                              </Button>
                            ) : null}
                            {actions.canForceRecreate &&
                            isExportRunForceRecreatable(run) ? (
                              <Button
                                size="sm"
                                variant="outline"
                                onClick={() =>
                                  setRunAction({ run, kind: "recreate" })
                                }
                              >
                                强制重建
                              </Button>
                            ) : null}
                          </div>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>
              <ListPagination
                page={page}
                pageSize={pageSize}
                total={runsTotal}
                pending={runsQuery.isFetching}
                onPageChange={(value) => changeParams({ page: String(value) })}
                onPageSizeChange={(value) =>
                  changeParams({ pageSize: String(value), page: "1" })
                }
              />
            </div>
          )}
        </>
      ) : (
        <>
          <div className="flex flex-wrap items-start justify-between gap-4">
            <div>
              <h1 className="text-2xl font-semibold tracking-tight">
                审批回写
              </h1>
              <p className="mt-2 text-sm text-muted-foreground">
                审批通过后把付款明细逐行写入多维表格，实例被拒绝 / 撤回 /
                退回时标记目标行。
              </p>
            </div>
            <div className="flex flex-wrap gap-2">
              {canListRuns ? (
                <Button
                  variant="outline"
                  onClick={() =>
                    changeParams({ view: "logs", rule: null, page: "1" }, true)
                  }
                >
                  日志
                </Button>
              ) : null}
              {actions.canCheck ? (
                <Button
                  variant="outline"
                  disabled={checking}
                  onClick={() => void checkAll()}
                >
                  {checking ? "检查中…" : "检查全部规则"}
                </Button>
              ) : null}
              {canCreate ? (
                <Button
                  onClick={() => {
                    setEditTarget(undefined);
                    setWizardOpen(true);
                  }}
                >
                  <Plus aria-hidden="true" />
                  新建规则
                </Button>
              ) : null}
            </div>
          </div>
          <div className="flex flex-wrap items-center gap-3">
            <Input
              className="w-full sm:w-72"
              aria-label="搜索回写规则"
              placeholder="搜索规则或审批定义"
              value={search}
              onChange={(event) =>
                changeParams(
                  { search: event.target.value || null, page: "1" },
                  true,
                )
              }
            />
          </div>
          {rulesQuery.isPending ? (
            <p role="status">正在读取回写规则…</p>
          ) : rulesQuery.isError ? (
            <div role="alert">
              <p>暂时无法读取回写规则：{messageOf(rulesQuery.error)}</p>
              <Button
                variant="outline"
                onClick={() => void rulesQuery.refetch()}
              >
                重新加载
              </Button>
            </div>
          ) : items.length === 0 && total === 0 ? (
            <div className="rounded-lg border p-8 text-center">
              <p className="font-medium">
                {search.trim() !== "" ? "没有符合条件的规则" : "还没有回写规则"}
              </p>
              <p className="mt-2 text-sm text-muted-foreground">
                {search.trim() !== ""
                  ? "尝试更换关键词。"
                  : "新建规则：选择来源审批与付款明细控件，再配置触发节点、条件、目标表和字段映射。"}
              </p>
            </div>
          ) : (
            <div className="space-y-4">
              <div className="overflow-x-auto rounded-lg border">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>规则名称</TableHead>
                      <TableHead>来源审批</TableHead>
                      <TableHead>触发</TableHead>
                      <TableHead>目标表</TableHead>
                      <TableHead>状态列</TableHead>
                      <TableHead>状态</TableHead>
                      <TableHead>更新时间</TableHead>
                      <TableHead className="text-right">操作</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {items.map((rule) => (
                      <TableRow key={rule.id}>
                        <TableCell className="whitespace-normal">
                          <p className="font-medium">{rule.title}</p>
                          <p className="mt-1 text-xs text-muted-foreground">
                            {rule.maps.length} 条映射 ·{" "}
                            {exportRowFilterLabel(rule.rowFilter)}
                          </p>
                          {rule.conditionJson !== null &&
                          rule.condition === null ? (
                            <p className="mt-1 text-xs text-destructive">
                              条件 JSON 无法解析，请重新编辑保存
                            </p>
                          ) : null}
                        </TableCell>
                        <TableCell className="whitespace-normal">
                          <p>{rule.approvalName?.trim() || "审批名称待补充"}</p>
                          <p className="mt-1 font-mono text-xs text-muted-foreground">
                            {rule.approvalCode}
                          </p>
                        </TableCell>
                        <TableCell className="whitespace-normal">
                          <p>{exportTriggerModeLabel(rule.triggerMode)}</p>
                          <p className="mt-1 text-xs text-muted-foreground">
                            {rule.triggerMode === "final_approved"
                              ? "无需节点"
                              : `${rule.triggerNodeIds.length} 个节点`}
                            {rule.requireFinalApproval
                              ? " · 还要求最终通过"
                              : ""}
                          </p>
                        </TableCell>
                        <TableCell className="whitespace-normal">
                          <p>
                            {rule.targetTableName?.trim() || "数据表名称待补充"}
                          </p>
                          <p className="mt-1 font-mono text-xs text-muted-foreground">
                            {rule.targetTableName
                              ? rule.targetTableId
                              : rule.targetBaseToken}
                          </p>
                        </TableCell>
                        <TableCell className="whitespace-normal">
                          {rule.markEnabled ? (
                            rule.statusFieldName?.trim() ||
                            rule.statusField ||
                            "未选择"
                          ) : (
                            <span className="text-xs text-muted-foreground">
                              不标记
                            </span>
                          )}
                        </TableCell>
                        <TableCell>
                          <StatusBadge
                            tone={rule.enabled ? "positive" : "neutral"}
                          >
                            {rule.enabled ? "已启用" : "已停用"}
                          </StatusBadge>
                        </TableCell>
                        <TableCell className="text-xs">
                          {formatUnixSeconds(rule.updatedAt)}
                        </TableCell>
                        <TableCell className="text-right">
                          <div className="flex justify-end gap-1">
                            {canListRuns ? (
                              <Button
                                size="sm"
                                variant="ghost"
                                onClick={() =>
                                  changeParams({
                                    view: "logs",
                                    rule: String(rule.id),
                                    page: "1",
                                    write: null,
                                    instance: null,
                                  })
                                }
                              >
                                查看运行
                              </Button>
                            ) : null}
                            {canCreate ? (
                              <Button
                                size="sm"
                                variant="ghost"
                                onClick={() => {
                                  setEditTarget(rule);
                                  setWizardOpen(true);
                                }}
                              >
                                编辑
                              </Button>
                            ) : null}
                            {canDelete ? (
                              <Button
                                size="sm"
                                variant="ghost"
                                onClick={() => setDeleteTarget(rule)}
                              >
                                删除
                              </Button>
                            ) : null}
                          </div>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>
              <ListPagination
                page={page}
                pageSize={pageSize}
                total={total}
                pending={rulesQuery.isFetching}
                onPageChange={(value) => changeParams({ page: String(value) })}
                onPageSizeChange={(value) =>
                  changeParams({ pageSize: String(value), page: "1" })
                }
              />
            </div>
          )}
          <p className="text-xs text-muted-foreground">
            已启用仅表示 worker
            会处理该规则；触发节点与条件以真实实例的试算结果为准。
          </p>
        </>
      )}

      {wizardOpen ? (
        <ApprovalExportRuleDialog
          key={editTarget?.id ?? "new"}
          open
          client={wizardClient}
          initialRule={editTarget}
          onCancel={() => setWizardOpen(false)}
          onSubmitted={submitWizard}
        />
      ) : null}

      <Dialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open && !pending) setDeleteTarget(null);
        }}
      >
        <DialogContent showCloseButton={!pending}>
          <DialogHeader>
            <DialogTitle>删除回写规则</DialogTitle>
            <DialogDescription>
              <span className="block">
                删除后该规则的字段映射会一并清除，运行记录保留（已写入的目标记录不受影响）。删除不可恢复。
              </span>
              {deleteTarget ? (
                <span className="mt-2 block">
                  {deleteTarget.title}（#{deleteTarget.id}）
                </span>
              ) : null}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="ghost"
              disabled={pending}
              onClick={() => setDeleteTarget(null)}
            >
              取消
            </Button>
            <Button
              variant="destructive"
              disabled={pending}
              onClick={() => void confirmDelete()}
            >
              {pending ? "删除中…" : "确认删除"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog
        open={runAction !== null}
        onOpenChange={(open) => {
          if (!open && !pending) setRunAction(null);
        }}
      >
        <DialogContent showCloseButton={!pending}>
          <DialogHeader>
            <DialogTitle>
              {runAction?.kind === "retry"
                ? "重试运行记录"
                : "强制重建目标记录"}
            </DialogTitle>
            <DialogDescription>
              {runAction?.kind === "retry" ? (
                <span className="block">
                  把该行重新排入队列，并唤醒同一实例的规则级行。只适用于 pending
                  / retry / review。
                </span>
              ) : (
                <span className="block">
                  目标记录被手工删除后，代次 +1 并换用新的幂等 Token
                  重新创建。旧代次记录保留作追溯。
                </span>
              )}
              {runAction ? (
                <span className="mt-2 block font-mono text-xs">
                  {runAction.run.instanceCode} · {runAction.run.rowKey}
                </span>
              ) : null}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="ghost"
              disabled={pending}
              onClick={() => setRunAction(null)}
            >
              取消
            </Button>
            <Button disabled={pending} onClick={() => void confirmRunAction()}>
              {pending
                ? "提交中…"
                : runAction?.kind === "retry"
                  ? "确认重试"
                  : "确认重建"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog
        open={checkResult !== null}
        onOpenChange={(open) => {
          if (!open && !checking) setCheckResult(null);
        }}
      >
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle>回写规则检查结果</DialogTitle>
            <DialogDescription>
              {checkResult
                ? `共检查 ${checkResult.total} 条启用规则：${checkResult.okCount} 条正常，${checkResult.problemCount} 条需要处理（配置失效 ${checkResult.invalidCount} 条，取数失败 ${checkResult.errorCount} 条）。`
                : null}
            </DialogDescription>
          </DialogHeader>
          <div className="max-h-[60vh] space-y-3 overflow-auto">
            {checkResult?.problems.map((problem) => (
              <div key={problem.ruleId} className="rounded-md border p-3">
                <p className="text-sm font-medium">
                  {problem.title}（#{problem.ruleId}）
                  <span className="ml-2 text-xs font-normal text-muted-foreground">
                    {problem.status === "error" ? "本轮取数失败" : "配置失效"}
                  </span>
                </p>
                <ul className="mt-2 list-disc space-y-1 pl-5 text-sm">
                  {problem.reasons.map((reason, index) => (
                    <li key={index}>{reason}</li>
                  ))}
                </ul>
                {problem.status === "error" ? (
                  <p className="mt-2 text-xs text-muted-foreground">
                    这不是配置本身的问题：本轮没取到飞书数据。请确认审批定义与目标表仍存在、应用仍有权限后重试。
                  </p>
                ) : null}
              </div>
            ))}
          </div>
          <DialogFooter>
            <Button
              variant="ghost"
              disabled={checking}
              onClick={() => setCheckResult(null)}
            >
              关闭
            </Button>
            <Button
              variant="outline"
              disabled={checking}
              onClick={() => void checkAll()}
            >
              {checking ? "检查中…" : "重新检查"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

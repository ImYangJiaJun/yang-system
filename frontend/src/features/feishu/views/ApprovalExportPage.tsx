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
import { Label } from "@/shared/ui/label";
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
  useApprovalExportCommentRunList,
  useApprovalExportLatestCommentRuns,
  useApprovalExportRuleList,
  useApprovalExportRunList,
  useApprovalExportWizardClient,
} from "../api";
import { ApprovalExportRuleDialog } from "../components/ApprovalExportRuleDialog";
import { FeishuOAuthCard } from "../components/FeishuOAuthCard";
import { ListPagination } from "../components/ListPagination";
import { StatusBadge, type ToneName } from "../components/StatusBadge";
import { PAGE_SIZE_OPTIONS, useDebouncedValue } from "../list-query";
import {
  APPROVAL_COMMENT_RUN_STATE_OPTIONS,
  EXPORT_WRITE_STATUS_OPTIONS,
  approvalCommentRunStateLabel,
  exportMarkStatusLabel,
  exportRowFilterLabel,
  exportTriggerModeLabel,
  exportWriteStatusLabel,
  formatUnixSeconds,
  isApprovalCommentRunRetryable,
  isExportRunForceRecreatable,
  isExportRunRetryable,
  type ApprovalCommentCheckResult,
  type ApprovalCommentRun,
  type ApprovalCommentTestResult,
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

function commentStateTone(state: string): ToneName {
  if (state === "done" || state === "commented") return "positive";
  if (state === "commenting" || state === "pending" || state === "waiting")
    return "info";
  if (state === "retry") return "warning";
  if (state === "failed") return "danger";
  return "neutral";
}

function LatestCommentRunCell({
  run,
}: {
  run: ApprovalCommentRun | undefined;
}) {
  if (!run) {
    return <span className="text-xs text-muted-foreground">—</span>;
  }
  return (
    <>
      <StatusBadge tone={commentStateTone(run.state)}>
        {approvalCommentRunStateLabel(run.state)}
      </StatusBadge>
      <p className="mt-1 text-xs text-muted-foreground">
        {formatUnixSeconds(run.updatedAt)}
      </p>
      {run.lastError ? (
        <p
          className="mt-1 line-clamp-2 text-xs text-destructive"
          title={run.lastError}
        >
          {run.lastError}
        </p>
      ) : null}
    </>
  );
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
  const canListCommentRuns = hasOperation(
    catalog,
    EXPORT_OPERATION_IDS.listCommentRuns,
  );

  const rawRule = params.get("rule");
  const parsedRule = rawRule === null ? null : Number(rawRule);
  const ruleId =
    parsedRule !== null && Number.isSafeInteger(parsedRule) && parsedRule > 0
      ? parsedRule
      : null;
  // 独立页面的三个视图：规则 / 回写日志 / 评论台账。`rule` 仍表示日志下钻到某条规则。
  const commentRunsMode = params.get("view") === "comment-runs";
  const logsMode =
    !commentRunsMode && (params.get("view") === "logs" || ruleId !== null);
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
  const commentState = params.get("commentState") ?? "";
  const commentRecordId = params.get("commentRecord") ?? "";
  const commentRuleRaw = params.get("commentRule") ?? "";
  const parsedCommentRule = Number(commentRuleRaw);
  const commentRuleId =
    Number.isSafeInteger(parsedCommentRule) && parsedCommentRule > 0
      ? parsedCommentRule
      : undefined;

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

  // 评论台账 / 体检 / 测试评论
  const [retryingCommentRunId, setRetryingCommentRunId] = useState<
    number | null
  >(null);
  const [commentCheckRule, setCommentCheckRule] =
    useState<ApprovalExportRule | null>(null);
  const [commentCheckRecordId, setCommentCheckRecordId] = useState("");
  const [commentChecking, setCommentChecking] = useState(false);
  const [commentCheckResult, setCommentCheckResult] =
    useState<ApprovalCommentCheckResult | null>(null);
  const [commentCheckError, setCommentCheckError] = useState<string | null>(
    null,
  );
  const [commentTestRule, setCommentTestRule] =
    useState<ApprovalExportRule | null>(null);
  const [commentTestStep, setCommentTestStep] = useState<
    "form" | "confirm" | "result"
  >("form");
  const [commentTestInstance, setCommentTestInstance] = useState("");
  const [commentTestText, setCommentTestText] = useState("");
  const [commentTesting, setCommentTesting] = useState(false);
  const [commentTestError, setCommentTestError] = useState<string | null>(null);
  const [commentTestResult, setCommentTestResult] =
    useState<ApprovalCommentTestResult | null>(null);

  const rulesQuery = useApprovalExportRuleList(
    { page, pageSize, search: debouncedSearch },
    { enabled: !logsMode && !commentRunsMode },
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
  const commentRunsQuery = useApprovalExportCommentRunList(
    {
      page,
      pageSize,
      ruleId: commentRuleId,
      state: commentState || undefined,
      recordId: commentRecordId,
    },
    { enabled: commentRunsMode },
  );

  const items = rulesQuery.data?.items ?? [];
  const total = rulesQuery.data?.total ?? null;
  const lastPage =
    total === null ? null : Math.max(1, Math.ceil(total / pageSize));
  const runs = runsQuery.data?.items ?? [];
  const runsTotal = runsQuery.data?.total ?? null;
  const runsLastPage =
    runsTotal === null ? null : Math.max(1, Math.ceil(runsTotal / pageSize));
  const commentRuns = commentRunsQuery.data?.items ?? [];
  const commentRunsTotal = commentRunsQuery.data?.total ?? null;
  const commentRunsLastPage =
    commentRunsTotal === null
      ? null
      : Math.max(1, Math.ceil(commentRunsTotal / pageSize));
  // 规则列表的「最近评论」列：每条可见规则各取最新一条台账。
  const latestCommentRuns = useApprovalExportLatestCommentRuns(
    items.map((rule) => rule.id),
    { enabled: !logsMode && !commentRunsMode },
  );

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
  const activeLastPage = commentRunsMode
    ? commentRunsLastPage
    : logsMode
      ? runsLastPage
      : lastPage;
  const activeBusy = commentRunsMode
    ? commentRunsQuery.isPending || commentRunsQuery.isError
    : logsMode
      ? runsQuery.isPending || runsQuery.isError
      : rulesQuery.isPending || rulesQuery.isError;
  useEffect(() => {
    if (activeBusy || activeLastPage === null || page <= activeLastPage) return;
    setParams(
      (current) => {
        const next = new URLSearchParams(current);
        next.set("page", String(activeLastPage));
        return next;
      },
      { replace: true },
    );
  }, [activeBusy, activeLastPage, page, setParams]);

  async function refresh() {
    await queryClient.invalidateQueries({
      queryKey: feishuQueryKeys.approvalExportRules(),
    });
    await queryClient.invalidateQueries({
      queryKey: feishuQueryKeys.approvalExportRuns(),
    });
    await queryClient.invalidateQueries({
      queryKey: feishuQueryKeys.approvalExportCommentRuns(),
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

  async function retryCommentRun(run: ApprovalCommentRun) {
    setRetryingCommentRunId(run.id);
    try {
      const result = await actions.retryCommentRun(run.id);
      toast.success(
        `评论已重新排队（原状态 ${approvalCommentRunStateLabel(
          result.previousState || run.state,
        )}）`,
      );
    } catch (cause) {
      toast.error(messageOf(cause));
    } finally {
      setRetryingCommentRunId(null);
    }
  }

  async function runCommentCheck(rule: ApprovalExportRule, recordId: string) {
    setCommentChecking(true);
    setCommentCheckError(null);
    try {
      setCommentCheckResult(
        await actions.checkComment(rule.id, recordId.trim() || null),
      );
    } catch (cause) {
      setCommentCheckResult(null);
      setCommentCheckError(messageOf(cause));
    } finally {
      setCommentChecking(false);
    }
  }

  function openCommentCheck(rule: ApprovalExportRule) {
    setCommentCheckRule(rule);
    setCommentCheckRecordId("");
    setCommentCheckResult(null);
    setCommentCheckError(null);
    void runCommentCheck(rule, "");
  }

  function openCommentTest(rule: ApprovalExportRule) {
    setCommentTestRule(rule);
    setCommentTestStep("form");
    setCommentTestInstance("");
    setCommentTestText("");
    setCommentTestResult(null);
    setCommentTestError(null);
  }

  async function sendCommentTest() {
    if (!commentTestRule) return;
    setCommentTesting(true);
    setCommentTestError(null);
    try {
      const result = await actions.testComment({
        instanceCode: commentTestInstance,
        // 二次确认在对话框的 confirm 步骤完成；到这里才显式传 true。
        confirm: true,
        text: commentTestText.trim() || undefined,
        ruleId: commentTestRule.id,
      });
      setCommentTestResult(result);
      setCommentTestStep("result");
    } catch (cause) {
      setCommentTestError(messageOf(cause));
    } finally {
      setCommentTesting(false);
    }
  }

  if (!actions.canRead) {
    return (
      <p className="p-6">当前身份没有查看审批回写规则的权限，请联系管理员。</p>
    );
  }

  return (
    <div className="mx-auto max-w-6xl space-y-6 p-4 sm:p-6 lg:p-8">
      {commentRunsMode ? (
        <>
          <div className="flex flex-wrap items-start justify-between gap-4">
            <div>
              <Button
                variant="ghost"
                className="mb-2 h-auto p-0"
                onClick={() =>
                  changeParams(
                    {
                      view: null,
                      page: "1",
                      commentState: null,
                      commentRecord: null,
                      commentRule: null,
                    },
                    true,
                  )
                }
              >
                <span aria-hidden="true">←</span> 返回规则列表
              </Button>
              <h1 className="text-2xl font-semibold tracking-tight">
                评论运行台账
              </h1>
              <p className="mt-2 text-sm text-muted-foreground">
                支付评论从排队到完成的逐条状态；失败的记录可人工重试。
              </p>
            </div>
          </div>
          <div className="flex flex-wrap items-center gap-3">
            <Select
              value={commentState === "" ? "__all__" : commentState}
              onValueChange={(value) =>
                changeParams(
                  {
                    commentState: value === "__all__" ? null : value,
                    page: "1",
                  },
                  true,
                )
              }
            >
              <SelectTrigger aria-label="评论状态筛选" className="w-52">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="__all__">全部评论状态</SelectItem>
                {APPROVAL_COMMENT_RUN_STATE_OPTIONS.map((option) => (
                  <SelectItem key={option.value} value={option.value}>
                    {option.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Input
              className="w-full sm:w-40"
              aria-label="按规则 ID 筛选"
              placeholder="规则 ID"
              inputMode="numeric"
              value={commentRuleRaw}
              onChange={(event) =>
                changeParams(
                  { commentRule: event.target.value || null, page: "1" },
                  true,
                )
              }
            />
            <Input
              className="w-full sm:w-64"
              aria-label="按目标记录筛选"
              placeholder="按目标记录 ID 筛选"
              value={commentRecordId}
              onChange={(event) =>
                changeParams(
                  { commentRecord: event.target.value || null, page: "1" },
                  true,
                )
              }
            />
          </div>
          {!canListCommentRuns ? (
            <p className="p-6">
              当前身份没有查看评论运行记录的权限，请联系管理员。
            </p>
          ) : commentRunsQuery.isPending ? (
            <p role="status">正在读取评论运行记录…</p>
          ) : commentRunsQuery.isError ? (
            <div role="alert">
              <p>
                暂时无法读取评论运行记录：{messageOf(commentRunsQuery.error)}
              </p>
              <Button
                variant="outline"
                onClick={() => void commentRunsQuery.refetch()}
              >
                重新加载
              </Button>
            </div>
          ) : commentRuns.length === 0 ? (
            <div className="rounded-lg border p-8 text-center">
              <p className="font-medium">还没有评论运行记录</p>
              <p className="mt-2 text-sm text-muted-foreground">
                启用支付评论的规则在目标记录变化后会在这里出现。
              </p>
            </div>
          ) : (
            <div className="space-y-4">
              <div className="overflow-x-auto rounded-lg border">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>状态</TableHead>
                      <TableHead>规则 / 目标记录</TableHead>
                      <TableHead>审批实例</TableHead>
                      <TableHead>评论 ID</TableHead>
                      <TableHead>尝试</TableHead>
                      <TableHead>下次重试</TableHead>
                      <TableHead>最后错误</TableHead>
                      <TableHead>更新时间</TableHead>
                      <TableHead className="text-right">操作</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {commentRuns.map((run) => (
                      <TableRow key={run.id}>
                        <TableCell>
                          <StatusBadge tone={commentStateTone(run.state)}>
                            {approvalCommentRunStateLabel(run.state)}
                          </StatusBadge>
                        </TableCell>
                        <TableCell className="whitespace-normal">
                          <p className="text-xs text-muted-foreground">
                            规则 #{run.ruleId}
                          </p>
                          <p className="mt-1 font-mono text-xs">
                            {run.recordId}
                          </p>
                        </TableCell>
                        <TableCell className="font-mono text-xs">
                          {run.instanceCode ?? "—"}
                        </TableCell>
                        <TableCell className="font-mono text-xs">
                          {run.commentId ?? "—"}
                        </TableCell>
                        <TableCell className="tabular-nums">
                          {run.attempts}
                        </TableCell>
                        <TableCell className="text-xs">
                          {run.nextAttemptAt === null
                            ? "—"
                            : formatUnixSeconds(run.nextAttemptAt)}
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
                          {actions.canRetryComment &&
                          isApprovalCommentRunRetryable(run.state) ? (
                            <Button
                              size="sm"
                              variant="outline"
                              disabled={retryingCommentRunId === run.id}
                              onClick={() => void retryCommentRun(run)}
                            >
                              {retryingCommentRunId === run.id
                                ? "重试中…"
                                : "重试"}
                            </Button>
                          ) : null}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>
              <ListPagination
                page={page}
                pageSize={pageSize}
                total={commentRunsTotal}
                pending={commentRunsQuery.isFetching}
                onPageChange={(value) => changeParams({ page: String(value) })}
                onPageSizeChange={(value) =>
                  changeParams({ pageSize: String(value), page: "1" })
                }
              />
            </div>
          )}
        </>
      ) : logsMode ? (
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
              {canListCommentRuns ? (
                <Button
                  variant="outline"
                  onClick={() =>
                    changeParams(
                      {
                        view: "comment-runs",
                        rule: null,
                        page: "1",
                        commentState: null,
                        commentRecord: null,
                        commentRule: null,
                      },
                      true,
                    )
                  }
                >
                  评论台账
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
          <FeishuOAuthCard />
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
                      <TableHead>支付评论</TableHead>
                      <TableHead>最近评论</TableHead>
                      <TableHead>状态</TableHead>
                      <TableHead>更新时间</TableHead>
                      <TableHead className="text-right">操作</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {items.map((rule, index) => (
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
                        <TableCell className="whitespace-normal">
                          {rule.commentConfig === null ? (
                            rule.commentConfigJson !== null ? (
                              <span className="text-xs text-destructive">
                                配置无法解析
                              </span>
                            ) : (
                              <span className="text-xs text-muted-foreground">
                                未配置
                              </span>
                            )
                          ) : (
                            <>
                              <StatusBadge
                                tone={
                                  rule.commentConfig.enabled
                                    ? "positive"
                                    : "neutral"
                                }
                              >
                                {rule.commentConfig.enabled
                                  ? "已启用"
                                  : "已停用"}
                              </StatusBadge>
                              <p className="mt-1 text-xs text-muted-foreground">
                                {rule.commentConfig.statusFieldName.trim() ||
                                  rule.commentConfig.statusFieldId ||
                                  "未选状态列"}
                              </p>
                            </>
                          )}
                        </TableCell>
                        <TableCell className="whitespace-normal">
                          <LatestCommentRunCell
                            run={latestCommentRuns[index]?.data?.items[0]}
                          />
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
                            {actions.canCheckComment ? (
                              <Button
                                size="sm"
                                variant="ghost"
                                onClick={() => openCommentCheck(rule)}
                              >
                                评论体检
                              </Button>
                            ) : null}
                            {actions.canTestComment ? (
                              <Button
                                size="sm"
                                variant="ghost"
                                onClick={() => openCommentTest(rule)}
                              >
                                测试评论
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

      <Dialog
        open={commentCheckRule !== null}
        onOpenChange={(open) => {
          if (!open && !commentChecking) setCommentCheckRule(null);
        }}
      >
        <DialogContent className="max-w-2xl" showCloseButton={!commentChecking}>
          <DialogHeader>
            <DialogTitle>支付评论体检</DialogTitle>
            <DialogDescription>
              {commentCheckRule
                ? `${commentCheckRule.title}（#${commentCheckRule.id}）逐项检查评论字段、状态列、事件订阅与实例映射。`
                : null}
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-3">
            <div className="flex flex-wrap items-end gap-2">
              <div className="min-w-48 flex-1 space-y-1.5">
                <Label htmlFor="export-comment-check-record">
                  目标记录 ID（可选）
                </Label>
                <Input
                  id="export-comment-check-record"
                  value={commentCheckRecordId}
                  onChange={(event) =>
                    setCommentCheckRecordId(event.target.value)
                  }
                  placeholder="留空取该规则最近一条已回写记录"
                  autoComplete="off"
                />
              </div>
              <Button
                variant="outline"
                disabled={commentChecking || commentCheckRule === null}
                onClick={() => {
                  if (commentCheckRule) {
                    void runCommentCheck(
                      commentCheckRule,
                      commentCheckRecordId,
                    );
                  }
                }}
              >
                {commentChecking ? "检查中…" : "重新体检"}
              </Button>
            </div>
            {commentCheckError !== null ? (
              <p role="alert" className="text-sm text-destructive">
                {messageOf(commentCheckError)}
              </p>
            ) : null}
            {commentChecking && commentCheckResult === null ? (
              <p role="status">正在体检…</p>
            ) : null}
            {commentCheckResult ? (
              <>
                <p
                  role="status"
                  className={
                    commentCheckResult.passed
                      ? "text-sm text-emerald-700"
                      : "text-sm text-destructive"
                  }
                >
                  {commentCheckResult.passed
                    ? `全部 ${commentCheckResult.items.length} 项检查通过。`
                    : `${commentCheckResult.failedCount} 项需要处理（共 ${commentCheckResult.items.length} 项）。`}
                </p>
                <div className="max-h-[55vh] space-y-2 overflow-auto">
                  {commentCheckResult.items.map((item) => (
                    <div key={item.id} className="rounded-md border p-3">
                      <div className="flex flex-wrap items-center gap-2">
                        <StatusBadge tone={item.passed ? "positive" : "danger"}>
                          {item.passed ? "通过" : "失败"}
                        </StatusBadge>
                        <p className="text-sm font-medium">{item.name}</p>
                        <span className="font-mono text-xs text-muted-foreground">
                          {item.id}
                        </span>
                      </div>
                      <p className="mt-1 whitespace-pre-wrap break-words text-xs text-muted-foreground">
                        {item.detail}
                      </p>
                    </div>
                  ))}
                </div>
              </>
            ) : null}
          </div>
          <DialogFooter>
            <Button
              variant="ghost"
              disabled={commentChecking}
              onClick={() => setCommentCheckRule(null)}
            >
              关闭
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog
        open={commentTestRule !== null}
        onOpenChange={(open) => {
          if (!open && !commentTesting) setCommentTestRule(null);
        }}
      >
        <DialogContent className="max-w-xl" showCloseButton={!commentTesting}>
          <DialogHeader>
            <DialogTitle>发送测试评论</DialogTitle>
            <DialogDescription>
              {commentTestRule
                ? `向真实审批实例发一条评论，验证评论链路。评论作者使用页面顶部绑定的飞书运维身份。规则：${commentTestRule.title}（#${commentTestRule.id}）。`
                : null}
            </DialogDescription>
          </DialogHeader>

          {commentTestStep === "form" ? (
            <div className="space-y-3">
              <div className="space-y-1.5">
                <Label htmlFor="export-comment-test-instance">
                  审批实例 Code
                </Label>
                <Input
                  id="export-comment-test-instance"
                  value={commentTestInstance}
                  onChange={(event) =>
                    setCommentTestInstance(event.target.value)
                  }
                  placeholder="例如 202610260136…"
                  autoComplete="off"
                />
              </div>
              <p className="text-xs text-muted-foreground">
                将使用页面顶部绑定的飞书运维身份作为评论作者。
              </p>
              <div className="space-y-1.5">
                <Label htmlFor="export-comment-test-text">
                  评论正文（可选）
                </Label>
                <textarea
                  id="export-comment-test-text"
                  className="min-h-16 w-full rounded-md border bg-transparent px-2 py-1 text-sm"
                  value={commentTestText}
                  onChange={(event) => setCommentTestText(event.target.value)}
                  placeholder="留空使用默认测试文案"
                />
              </div>
            </div>
          ) : null}

          {commentTestStep === "confirm" ? (
            <div className="space-y-3">
              <div className="space-y-1 rounded-md border p-3 text-sm">
                <p>
                  实例：
                  <span className="font-mono">{commentTestInstance}</span>
                </p>
                <p>作者：页面顶部绑定的飞书运维身份</p>
                <p>正文：{commentTestText.trim() || "默认测试文案"}</p>
              </div>
              <p role="alert" className="text-sm text-destructive">
                确认后会在真实审批实例上留下一条评论，无法撤回。
              </p>
              {commentTestError !== null ? (
                <p role="alert" className="text-sm text-destructive">
                  {commentTestError}
                </p>
              ) : null}
            </div>
          ) : null}

          {commentTestStep === "result" && commentTestResult ? (
            <div className="space-y-3">
              <p className="text-sm">
                飞书返回 code{" "}
                <span className="font-mono">{commentTestResult.code}</span>
                {commentTestResult.httpStatus !== null
                  ? `（HTTP ${commentTestResult.httpStatus}）`
                  : ""}
                ：{commentTestResult.msg || "（空 msg）"}
              </p>
              {commentTestResult.commentId ? (
                <p className="text-sm">
                  评论 ID：
                  <span className="font-mono">
                    {commentTestResult.commentId}
                  </span>
                </p>
              ) : null}
              <pre className="max-h-60 overflow-auto rounded-md border bg-muted/40 p-3 text-xs">
                {JSON.stringify(commentTestResult.data, null, 2)}
              </pre>
            </div>
          ) : null}

          <DialogFooter>
            {commentTestStep === "form" ? (
              <>
                <Button
                  variant="ghost"
                  onClick={() => setCommentTestRule(null)}
                >
                  取消
                </Button>
                <Button
                  disabled={commentTestInstance.trim() === ""}
                  onClick={() => {
                    setCommentTestError(null);
                    setCommentTestStep("confirm");
                  }}
                >
                  下一步：确认
                </Button>
              </>
            ) : commentTestStep === "confirm" ? (
              <>
                <Button
                  variant="ghost"
                  disabled={commentTesting}
                  onClick={() => setCommentTestStep("form")}
                >
                  返回修改
                </Button>
                <Button
                  disabled={commentTesting}
                  onClick={() => void sendCommentTest()}
                >
                  {commentTesting ? "发送中…" : "确认发送"}
                </Button>
              </>
            ) : (
              <>
                <Button
                  variant="ghost"
                  onClick={() => setCommentTestRule(null)}
                >
                  关闭
                </Button>
                <Button
                  variant="outline"
                  onClick={() => {
                    setCommentTestStep("form");
                    setCommentTestResult(null);
                    setCommentTestError(null);
                  }}
                >
                  再发一条
                </Button>
              </>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

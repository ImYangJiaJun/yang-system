import { useCallback, useEffect, useMemo, useState } from "react";
import { Link, useNavigate } from "react-router";
import { Inbox, RefreshCw } from "lucide-react";

import { useSessionController, useSessionCredentials } from "@/engine";
import { Button } from "@/shared/ui/button";
import { Checkbox } from "@/shared/ui/checkbox";
import { Skeleton } from "@/shared/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/shared/ui/table";
import { formatProductDateTime } from "@/shared/lib/product-locale";
import { useToast } from "@/shared/lib/toast";

import {
  BATCH_ACTION_LABELS,
  createBatchJob,
  definitionKey,
  describeBatchError,
  fetchBatchTasks,
  fetchCurrentUsername,
  groupBatchTasks,
  isFeishuReauthError,
  isFeishuScopeError,
  newIdempotencyKey,
  requestFeishuAuthorizationUrl,
  taskOperability,
  type BatchApprovalAction,
  type BatchJobItem,
  type BatchTask,
} from "../batch-approval-api";
import {
  BatchActionDialog,
  type BatchActionParams,
} from "../components/BatchActionDialog";
import { BatchJobDialog } from "../components/BatchJobDialog";
import { BatchTaskDetailDialog } from "../components/BatchTaskDetailDialog";
import { StatusBadge } from "../components/StatusBadge";

const TASK_STATUS_LABELS: Record<string, string> = {
  PENDING: "待审批",
  APPROVED: "已通过",
  REJECTED: "已拒绝",
  CANCELED: "已撤销",
  CANCELLED: "已撤销",
  DELETED: "已删除",
};

function taskStatusLabel(status: string): string {
  return TASK_STATUS_LABELS[status] ?? status;
}

function timeText(value: number | null): string {
  return value === null ? "—" : formatProductDateTime(value);
}

type SelectedMap = Record<string, BatchTask>;

type LastJobRequest = {
  action: BatchApprovalAction;
  params: BatchActionParams;
  items: BatchJobItem[];
};

/// 批量审批页：独立专注页（根层 RequireAuth，不套 AppLayout）。
///
/// 数据流：拉本人待办 → 按定义分组 → 勾选可操作任务 → 在确认对话框里选动作与参数
/// → 创建 job → 轮询逐条结果。所有写操作只创建 job，页面本身不直接调飞书。
export default function BatchApprovalPage() {
  const session = useSessionCredentials();
  const token = session.token;
  const controller = useSessionController();
  const navigate = useNavigate();
  const toast = useToast();

  const [tasks, setTasks] = useState<BatchTask[]>([]);
  const [truncated, setTruncated] = useState(false);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState("");
  const [reauthRequired, setReauthRequired] = useState(false);
  const [scopeError, setScopeError] = useState(false);
  const [username, setUsername] = useState<string | null>(null);
  const [loggingOut, setLoggingOut] = useState(false);
  const [authorizing, setAuthorizing] = useState(false);

  const [definition, setDefinition] = useState("all");
  const [selected, setSelected] = useState<SelectedMap>({});
  const [detailTask, setDetailTask] = useState<BatchTask | null>(null);
  const [action, setAction] = useState<BatchApprovalAction | null>(null);
  const [actionError, setActionError] = useState("");
  const [submittingAction, setSubmittingAction] = useState(false);
  const [job, setJob] = useState<{ id: string; label: string } | null>(null);
  const [lastRequest, setLastRequest] = useState<LastJobRequest | null>(null);
  const [retrying, setRetrying] = useState(false);

  const loadTasks = useCallback(
    async (signal?: AbortSignal) => {
      setLoading(true);
      setLoadError("");
      setReauthRequired(false);
      setScopeError(false);
      try {
        const result = await fetchBatchTasks(token, signal);
        if (signal?.aborted) return;
        setTasks(result.tasks);
        setTruncated(result.truncated);
        // 刷新后只保留仍然存在且可操作的选择，避免把已处理任务再次提交。
        setSelected((prev) => {
          const next: SelectedMap = {};
          for (const task of result.tasks) {
            if (prev[task.taskId] && taskOperability(task).operable) {
              next[task.taskId] = task;
            }
          }
          return next;
        });
      } catch (cause) {
        if (signal?.aborted) return;
        setTasks([]);
        setTruncated(false);
        if (isFeishuReauthError(cause)) {
          setReauthRequired(true);
        } else {
          if (isFeishuScopeError(cause)) setScopeError(true);
          setLoadError(describeBatchError(cause));
        }
      } finally {
        if (!signal?.aborted) setLoading(false);
      }
    },
    [token],
  );

  useEffect(() => {
    const abort = new AbortController();
    void loadTasks(abort.signal);
    void fetchCurrentUsername(token, abort.signal)
      .then((name) => {
        if (!abort.signal.aborted) setUsername(name);
      })
      .catch(() => {
        // 当前用户名只是页眉信息，取不到就显示占位，不阻塞任务列表。
      });
    return () => abort.abort();
  }, [loadTasks, token]);

  const groups = useMemo(() => groupBatchTasks(tasks), [tasks]);
  const filteredTasks = useMemo(
    () =>
      definition === "all"
        ? tasks
        : tasks.filter((task) => definitionKey(task) === definition),
    [tasks, definition],
  );
  const filteredOperable = useMemo(
    () => filteredTasks.filter((task) => taskOperability(task).operable),
    [filteredTasks],
  );
  const selectedTasks = useMemo(() => Object.values(selected), [selected]);
  const selectedVisibleCount = filteredOperable.filter(
    (task) => selected[task.taskId],
  ).length;
  const allVisibleSelected =
    filteredOperable.length > 0 &&
    selectedVisibleCount === filteredOperable.length;
  const someVisibleSelected = selectedVisibleCount > 0 && !allVisibleSelected;

  const toggleTask = (task: BatchTask) => {
    if (!taskOperability(task).operable) return;
    setSelected((prev) => {
      const next = { ...prev };
      if (next[task.taskId]) {
        delete next[task.taskId];
      } else {
        next[task.taskId] = task;
      }
      return next;
    });
  };

  const toggleAllVisible = () => {
    setSelected((prev) => {
      const next = { ...prev };
      if (allVisibleSelected) {
        for (const task of filteredOperable) delete next[task.taskId];
      } else {
        for (const task of filteredOperable) next[task.taskId] = task;
      }
      return next;
    });
  };

  const openAction = (nextAction: BatchApprovalAction) => {
    if (selectedTasks.length === 0) {
      toast.info("请先勾选可操作的任务");
      return;
    }
    setActionError("");
    setAction(nextAction);
  };

  const jobParams = (params: BatchActionParams) => ({
    comment: params.comment,
    transferOpenId: params.transferUser?.openId,
    addSignOpenIds: params.addSignUsers?.map((user) => user.openId),
    addSignType: params.addSignType,
    approvalMethod: params.approvalMethod,
    rollbackMode: params.rollbackMode,
  });

  const confirmAction = async (params: BatchActionParams) => {
    if (!action || selectedTasks.length === 0 || submittingAction) return;
    const items: BatchJobItem[] = selectedTasks.map((task) => ({
      taskId: task.taskId,
      instanceCode: task.instanceCode,
      title: task.title,
      status: "pending",
      resultCode: null,
      message: null,
      beforeStatus: null,
      afterStatus: null,
    }));
    setSubmittingAction(true);
    setActionError("");
    try {
      const jobId = await createBatchJob(
        {
          action,
          items,
          idempotencyKey: newIdempotencyKey(),
          ...jobParams(params),
        },
        token,
      );
      setLastRequest({ action, params, items });
      setAction(null);
      setSelected({});
      setJob({ id: jobId, label: BATCH_ACTION_LABELS[action] });
      toast.success("批量任务已创建，正在后台执行");
    } catch (cause) {
      if (isFeishuReauthError(cause)) {
        setAction(null);
        setReauthRequired(true);
        setLoadError(describeBatchError(cause));
      } else {
        setActionError(describeBatchError(cause));
      }
    } finally {
      setSubmittingAction(false);
    }
  };

  const retryFailed = async (failedItems: BatchJobItem[]) => {
    if (!lastRequest || retrying) return;
    setRetrying(true);
    try {
      const jobId = await createBatchJob(
        {
          action: lastRequest.action,
          items: failedItems,
          idempotencyKey: newIdempotencyKey(),
          ...jobParams(lastRequest.params),
        },
        token,
      );
      setJob({
        id: jobId,
        label: `重试${BATCH_ACTION_LABELS[lastRequest.action]}`,
      });
    } catch (cause) {
      toast.error(describeBatchError(cause));
    } finally {
      setRetrying(false);
    }
  };

  const closeJob = () => {
    setJob(null);
    void loadTasks();
  };

  const reauthorize = async () => {
    if (authorizing) return;
    setAuthorizing(true);
    try {
      const url = await requestFeishuAuthorizationUrl(token);
      window.location.assign(url);
    } catch (cause) {
      toast.error(describeBatchError(cause));
      setAuthorizing(false);
    }
  };

  const logout = async () => {
    if (loggingOut) return;
    setLoggingOut(true);
    try {
      await controller.endSession();
      navigate("/login", { replace: true });
    } finally {
      setLoggingOut(false);
    }
  };

  return (
    <div className="flex min-h-svh flex-col bg-background text-foreground">
      <header className="sticky top-0 z-10 flex flex-wrap items-center justify-between gap-3 border-b border-border bg-background/95 px-6 py-3 backdrop-blur">
        <div className="flex items-center gap-2 text-sm font-semibold">
          <span className="flex size-8 items-center justify-center rounded-lg bg-primary text-sm font-bold text-primary-foreground">
            Y
          </span>
          YANG System
          <span className="font-normal text-muted-foreground">· 批量审批</span>
        </div>
        <div className="flex flex-wrap items-center gap-3 text-sm">
          <span className="text-muted-foreground">
            当前用户：{username ?? "—"}
          </span>
          <Button variant="ghost" size="sm" asChild>
            <Link to="/">返回系统</Link>
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={loggingOut}
            onClick={() => void logout()}
          >
            {loggingOut ? "退出中…" : "退出登录"}
          </Button>
        </div>
      </header>

      <main className="mx-auto flex w-full max-w-7xl flex-1 flex-col gap-4 p-6">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div>
            <h1 className="text-xl font-semibold">批量审批</h1>
            <p className="text-sm text-muted-foreground">
              处理你自己的飞书待办。写操作由后台逐条执行，不自动重试。
            </p>
          </div>
          <Button
            variant="outline"
            size="sm"
            disabled={loading}
            onClick={() => void loadTasks()}
          >
            <RefreshCw className="size-4" aria-hidden="true" />
            {loading ? "刷新中…" : "刷新"}
          </Button>
        </div>

        {truncated && (
          <p className="rounded-md border border-tone-warning/40 bg-tone-warning/10 px-3 py-2 text-sm">
            待办超过 1000 条，仅显示前 1000 条。请按审批定义筛选或分批处理。
          </p>
        )}
        {reauthRequired && (
          <div
            role="alert"
            className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-tone-warning/40 bg-tone-warning/10 px-3 py-2 text-sm"
          >
            <span>
              飞书授权已过期或尚未绑定，无法读取待办。请重新授权飞书账号。
            </span>
            <Button
              size="sm"
              disabled={authorizing}
              onClick={() => void reauthorize()}
            >
              {authorizing ? "正在跳转…" : "重新授权飞书"}
            </Button>
          </div>
        )}
        {scopeError && (
          <p
            role="alert"
            className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
          >
            飞书应用未开通审批/通讯录权限，请联系管理员在开放平台开通并发布版本。
          </p>
        )}
        {loadError && (
          <div
            role="alert"
            className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
          >
            <span>{loadError}</span>
            <Button
              variant="outline"
              size="sm"
              disabled={loading}
              onClick={() => void loadTasks()}
            >
              重试
            </Button>
          </div>
        )}

        <div className="flex flex-1 flex-col gap-4 md:flex-row">
          <aside
            aria-label="审批定义"
            className="w-full shrink-0 space-y-1 md:w-64"
          >
            <p className="px-2 text-xs font-medium text-muted-foreground">
              按审批定义
            </p>
            <button
              type="button"
              aria-pressed={definition === "all"}
              className={
                definition === "all"
                  ? "flex w-full items-center justify-between rounded-md bg-primary/10 px-3 py-2 text-left text-sm text-primary"
                  : "flex w-full items-center justify-between rounded-md px-3 py-2 text-left text-sm text-muted-foreground hover:bg-muted"
              }
              onClick={() => setDefinition("all")}
            >
              <span>全部待办</span>
              <span className="tabular-nums">{tasks.length}</span>
            </button>
            {groups.map((group) => (
              <button
                key={group.key}
                type="button"
                aria-pressed={definition === group.key}
                className={
                  definition === group.key
                    ? "flex w-full items-center justify-between gap-2 rounded-md bg-primary/10 px-3 py-2 text-left text-sm text-primary"
                    : "flex w-full items-center justify-between gap-2 rounded-md px-3 py-2 text-left text-sm text-muted-foreground hover:bg-muted"
                }
                onClick={() => setDefinition(group.key)}
              >
                <span className="min-w-0">
                  <span className="block truncate">{group.name}</span>
                  {group.code && (
                    <span className="block truncate text-xs opacity-70">
                      {group.code}
                    </span>
                  )}
                </span>
                <span className="tabular-nums">{group.tasks.length}</span>
              </button>
            ))}
          </aside>

          <section className="min-w-0 flex-1 space-y-3">
            <div className="flex flex-wrap items-center gap-2 rounded-lg border border-border bg-card px-3 py-2">
              <span className="text-sm text-muted-foreground">
                已选 {selectedTasks.length} 条
              </span>
              <Button
                variant="ghost"
                size="sm"
                disabled={selectedTasks.length === 0}
                onClick={() => setSelected({})}
              >
                清空选择
              </Button>
              <div className="ml-auto flex flex-wrap gap-2">
                <Button
                  size="sm"
                  disabled={selectedTasks.length === 0}
                  onClick={() => openAction("pass")}
                >
                  同意
                </Button>
                <Button
                  size="sm"
                  variant="destructive"
                  disabled={selectedTasks.length === 0}
                  onClick={() => openAction("refuse")}
                >
                  拒绝
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={selectedTasks.length === 0}
                  onClick={() => openAction("forward")}
                >
                  转交
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={selectedTasks.length === 0}
                  onClick={() => openAction("rollback")}
                >
                  退回
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={selectedTasks.length === 0}
                  onClick={() => openAction("add_sign")}
                >
                  加签
                </Button>
              </div>
            </div>

            {loading && tasks.length === 0 ? (
              <div className="space-y-2" aria-label="待办加载中">
                <Skeleton className="h-10 w-full" />
                <Skeleton className="h-10 w-full" />
                <Skeleton className="h-10 w-full" />
              </div>
            ) : tasks.length === 0 ? (
              reauthRequired || loadError || scopeError ? null : (
                <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border py-16 text-center">
                  <Inbox className="size-10 text-muted-foreground" />
                  <p className="font-medium">暂无待办</p>
                  <p className="text-sm text-muted-foreground">
                    当前飞书账号没有待你处理的审批；刷新可重新拉取。
                  </p>
                </div>
              )
            ) : filteredTasks.length === 0 ? (
              <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border py-16 text-center">
                <p className="font-medium">该审批定义下没有待办</p>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setDefinition("all")}
                >
                  查看全部待办
                </Button>
              </div>
            ) : (
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead className="w-10">
                      <Checkbox
                        aria-label="全选当前可操作任务"
                        checked={
                          allVisibleSelected
                            ? true
                            : someVisibleSelected
                              ? "indeterminate"
                              : false
                        }
                        disabled={filteredOperable.length === 0}
                        onCheckedChange={toggleAllVisible}
                      />
                    </TableHead>
                    <TableHead>标题</TableHead>
                    <TableHead>发起人</TableHead>
                    <TableHead>当前节点 / 摘要</TableHead>
                    <TableHead>时间</TableHead>
                    <TableHead>状态</TableHead>
                    <TableHead>API 操作</TableHead>
                    <TableHead className="w-20">详情</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {filteredTasks.map((task) => {
                    const operability = taskOperability(task);
                    const checked = Boolean(selected[task.taskId]);
                    const statusUpper = task.status.toUpperCase();
                    return (
                      <TableRow
                        key={task.taskId}
                        data-state={checked ? "selected" : undefined}
                      >
                        <TableCell>
                          <Checkbox
                            aria-label={`选择 ${task.title}`}
                            checked={checked}
                            disabled={!operability.operable}
                            onCheckedChange={() => toggleTask(task)}
                          />
                        </TableCell>
                        <TableCell
                          className="max-w-64 truncate font-medium"
                          title={task.title}
                        >
                          {task.title}
                        </TableCell>
                        <TableCell>{task.initiatorName ?? "—"}</TableCell>
                        <TableCell className="max-w-64">
                          <span
                            className="block truncate"
                            title={task.summaries.join("；")}
                          >
                            {task.summaries.join("；") || "—"}
                          </span>
                          {!operability.operable && (
                            <span className="block text-xs text-muted-foreground">
                              {operability.reason}
                            </span>
                          )}
                        </TableCell>
                        <TableCell className="text-xs tabular-nums">
                          {timeText(task.createdAt)}
                        </TableCell>
                        <TableCell>
                          <span className="inline-flex flex-wrap gap-1">
                            <StatusBadge
                              tone={
                                statusUpper === "PENDING" ? "info" : "neutral"
                              }
                            >
                              {taskStatusLabel(statusUpper)}
                            </StatusBadge>
                            {task.external && (
                              <StatusBadge tone="warning">外部审批</StatusBadge>
                            )}
                          </span>
                        </TableCell>
                        <TableCell>
                          <StatusBadge
                            tone={
                              task.supportApiOperate ? "positive" : "warning"
                            }
                          >
                            {task.supportApiOperate ? "支持" : "不支持"}
                          </StatusBadge>
                        </TableCell>
                        <TableCell>
                          <Button
                            variant="ghost"
                            size="sm"
                            onClick={() => setDetailTask(task)}
                          >
                            查看
                          </Button>
                        </TableCell>
                      </TableRow>
                    );
                  })}
                </TableBody>
              </Table>
            )}
          </section>
        </div>
      </main>

      <BatchTaskDetailDialog
        open={detailTask !== null}
        task={detailTask}
        accessToken={token}
        onClose={() => setDetailTask(null)}
      />
      <BatchActionDialog
        open={action !== null}
        action={action}
        count={selectedTasks.length}
        accessToken={token}
        submitting={submittingAction}
        errorMessage={actionError}
        onCancel={() => {
          if (submittingAction) return;
          setAction(null);
          setActionError("");
        }}
        onConfirm={(params) => void confirmAction(params)}
      />
      <BatchJobDialog
        open={job !== null}
        jobId={job?.id ?? null}
        actionLabel={job?.label ?? "批量审批"}
        accessToken={token}
        retrying={retrying}
        onClose={closeJob}
        onRetryFailed={(items) => void retryFailed(items)}
      />
    </div>
  );
}

import { Fragment, useCallback, useEffect, useMemo, useState } from "react";
import { Link, useNavigate } from "react-router";
import {
  ChevronDown,
  ChevronRight,
  Inbox,
  RefreshCw,
  RotateCcw,
} from "lucide-react";

import { useSessionController, useSessionCredentials } from "@/engine";
import { Button } from "@/shared/ui/button";
import { Checkbox } from "@/shared/ui/checkbox";
import { Input } from "@/shared/ui/input";
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
  FEISHU_STATUS_FILTER_OPTIONS,
  createBatchJob,
  definitionKey,
  describeBatchError,
  feishuStatusLabel,
  fetchBatchTaskDetail,
  fetchBatchTasks,
  fetchCurrentUsername,
  groupBatchTasks,
  isFeishuReauthError,
  isFeishuScopeError,
  newIdempotencyKey,
  requestFeishuAuthorizationUrl,
  taskOperability,
  taskStatusLabel,
  type BatchApprovalAction,
  type BatchJobItem,
  type BatchTask,
  type BatchTaskDetail,
} from "../batch-approval-api";
import {
  BatchActionDialog,
  type BatchActionParams,
} from "../components/BatchActionDialog";
import { BatchJobDialog } from "../components/BatchJobDialog";
import { BatchTaskDetailBody } from "../components/BatchTaskDetailDialog";
import { StatusBadge } from "../components/StatusBadge";

function timeText(value: number | null): string {
  return value === null ? "—" : formatProductDateTime(value);
}

function dateKey(value: number | null): string {
  if (value === null) return "";
  const date = new Date(value);
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${date.getFullYear()}-${month}-${day}`;
}

function statusTone(label: string): "info" | "positive" | "danger" | "neutral" {
  if (label === "待办" || label === "处理中" || label === "审批中") {
    return "info";
  }
  if (
    label === "已办" ||
    label === "已通过" ||
    label === "已完成" ||
    label === "已读"
  ) {
    return "positive";
  }
  if (label === "已拒绝" || label === "撤回" || label === "已撤销") {
    return "danger";
  }
  return "neutral";
}

type SelectedMap = Record<string, BatchTask>;

type LastJobRequest = {
  action: BatchApprovalAction;
  params: BatchActionParams;
  items: BatchJobItem[];
};

type Filters = {
  title: string;
  definition: string;
  initiator: string;
  summary: string;
  from: string;
  to: string;
  status: string;
  api: "all" | "supported" | "unsupported";
  external: "all" | "feishu" | "external";
};

const EMPTY_FILTERS: Filters = {
  title: "",
  definition: "",
  initiator: "",
  summary: "",
  from: "",
  to: "",
  status: "all",
  api: "all",
  external: "all",
};

function includesText(
  value: string | null | undefined,
  needle: string,
): boolean {
  return (value ?? "").toLocaleLowerCase().includes(needle.toLocaleLowerCase());
}

/// 批量审批页：独立专注页（根层 RequireAuth，不套 AppLayout）。
///
/// 数据流：拉本人待办 → 表格展示与逐列筛选 → 勾选可操作任务 → 在确认对话框里选动作
/// → 创建 job → 轮询逐条结果。详情在行内展开，不再需要逐条打开弹窗。
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

  const [definitionGroup, setDefinitionGroup] = useState("all");
  const [filters, setFilters] = useState<Filters>(EMPTY_FILTERS);
  const [selected, setSelected] = useState<SelectedMap>({});
  const [expandedTaskId, setExpandedTaskId] = useState<string | null>(null);
  const [details, setDetails] = useState<Record<string, BatchTaskDetail>>({});
  const [detailErrors, setDetailErrors] = useState<Record<string, string>>({});
  const [detailLoadingId, setDetailLoadingId] = useState<string | null>(null);
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

  const filteredTasks = useMemo(() => {
    const needleTitle = filters.title.trim();
    const needleDefinition = filters.definition.trim();
    const needleInitiator = filters.initiator.trim();
    const needleSummary = filters.summary.trim();
    return tasks.filter((task) => {
      if (
        definitionGroup !== "all" &&
        definitionKey(task) !== definitionGroup
      ) {
        return false;
      }
      if (needleTitle && !includesText(task.title, needleTitle)) return false;
      if (
        needleDefinition &&
        !includesText(task.definitionName, needleDefinition) &&
        !includesText(task.definitionCode, needleDefinition)
      ) {
        return false;
      }
      if (
        needleInitiator &&
        !includesText(task.initiatorName, needleInitiator)
      ) {
        return false;
      }
      if (
        needleSummary &&
        !task.summaries.some((summary) => includesText(summary, needleSummary))
      ) {
        return false;
      }
      const date = dateKey(task.createdAt);
      if ((filters.from || filters.to) && !date) return false;
      if (filters.from && date < filters.from) return false;
      if (filters.to && date > filters.to) return false;
      if (
        filters.status !== "all" &&
        taskStatusLabel(task) !== filters.status
      ) {
        return false;
      }
      if (filters.api === "supported" && !task.supportApiOperate) return false;
      if (filters.api === "unsupported" && task.supportApiOperate) return false;
      if (filters.external === "external" && !task.external) return false;
      if (filters.external === "feishu" && task.external) return false;
      return true;
    });
  }, [tasks, definitionGroup, filters]);

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

  const updateFilter = <K extends keyof Filters>(key: K, value: Filters[K]) => {
    setFilters((prev) => ({ ...prev, [key]: value }));
  };

  const resetFilters = () => {
    setFilters(EMPTY_FILTERS);
    setDefinitionGroup("all");
  };

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

  const loadDetail = useCallback(
    (task: BatchTask) => {
      const abort = new AbortController();
      setDetailLoadingId(task.taskId);
      setDetailErrors((prev) => {
        const next = { ...prev };
        delete next[task.taskId];
        return next;
      });
      void fetchBatchTaskDetail(
        task.taskId,
        task.instanceCode,
        token,
        abort.signal,
      )
        .then((detail) => {
          if (abort.signal.aborted) return;
          setDetails((prev) => ({ ...prev, [task.taskId]: detail }));
        })
        .catch((cause: unknown) => {
          if (abort.signal.aborted) return;
          setDetailErrors((prev) => ({
            ...prev,
            [task.taskId]:
              cause instanceof Error ? cause.message : "审批详情加载失败",
          }));
        })
        .finally(() => {
          if (!abort.signal.aborted) setDetailLoadingId(null);
        });
      return () => abort.abort();
    },
    [token],
  );

  const toggleDetail = (task: BatchTask) => {
    if (expandedTaskId === task.taskId) {
      setExpandedTaskId(null);
      return;
    }
    setExpandedTaskId(task.taskId);
    if (!details[task.taskId] && detailLoadingId !== task.taskId) {
      loadDetail(task);
    }
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

      <main className="mx-auto flex w-full max-w-[1600px] flex-1 flex-col gap-4 p-6">
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
            className="w-full shrink-0 space-y-1 md:w-60"
          >
            <p className="px-2 text-xs font-medium text-muted-foreground">
              按审批定义
            </p>
            <button
              type="button"
              aria-pressed={definitionGroup === "all"}
              className={
                definitionGroup === "all"
                  ? "flex w-full items-center justify-between rounded-md bg-primary/10 px-3 py-2 text-left text-sm text-primary"
                  : "flex w-full items-center justify-between rounded-md px-3 py-2 text-left text-sm text-muted-foreground hover:bg-muted"
              }
              onClick={() => setDefinitionGroup("all")}
            >
              <span>全部待办</span>
              <span className="tabular-nums">{tasks.length}</span>
            </button>
            {groups.map((group) => (
              <button
                key={group.key}
                type="button"
                aria-pressed={definitionGroup === group.key}
                className={
                  definitionGroup === group.key
                    ? "flex w-full items-center justify-between gap-2 rounded-md bg-primary/10 px-3 py-2 text-left text-sm text-primary"
                    : "flex w-full items-center justify-between gap-2 rounded-md px-3 py-2 text-left text-sm text-muted-foreground hover:bg-muted"
                }
                onClick={() => setDefinitionGroup(group.key)}
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
                已选 {selectedTasks.length} 条 · 当前筛选 {filteredTasks.length}{" "}
                条
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
                <p className="font-medium">没有符合筛选条件的待办</p>
                <Button variant="outline" size="sm" onClick={resetFilters}>
                  重置筛选
                </Button>
              </div>
            ) : (
              <div className="overflow-x-auto rounded-lg border border-border">
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
                      <TableHead className="min-w-44">标题</TableHead>
                      <TableHead className="min-w-44">审批定义</TableHead>
                      <TableHead className="min-w-28">发起人</TableHead>
                      <TableHead className="min-w-52">摘要</TableHead>
                      <TableHead className="min-w-44">提交时间</TableHead>
                      <TableHead className="min-w-28">实例状态</TableHead>
                      <TableHead className="min-w-24">API 操作</TableHead>
                      <TableHead className="min-w-24">来源</TableHead>
                      <TableHead className="w-24">详情</TableHead>
                    </TableRow>
                    <TableRow className="bg-muted/30 hover:bg-muted/30">
                      <TableHead>
                        <Button
                          variant="ghost"
                          size="sm"
                          className="h-8 px-1 text-xs"
                          onClick={resetFilters}
                          title="重置全部筛选"
                        >
                          <RotateCcw className="size-3.5" />
                        </Button>
                      </TableHead>
                      <TableHead>
                        <Input
                          aria-label="按标题筛选"
                          placeholder="标题"
                          className="h-8 text-xs"
                          value={filters.title}
                          onChange={(event) =>
                            updateFilter("title", event.target.value)
                          }
                        />
                      </TableHead>
                      <TableHead>
                        <Input
                          aria-label="按审批定义筛选"
                          placeholder="定义名称/Code"
                          className="h-8 text-xs"
                          value={filters.definition}
                          onChange={(event) =>
                            updateFilter("definition", event.target.value)
                          }
                        />
                      </TableHead>
                      <TableHead>
                        <Input
                          aria-label="按发起人筛选"
                          placeholder="发起人"
                          className="h-8 text-xs"
                          value={filters.initiator}
                          onChange={(event) =>
                            updateFilter("initiator", event.target.value)
                          }
                        />
                      </TableHead>
                      <TableHead>
                        <Input
                          aria-label="按摘要筛选"
                          placeholder="摘要关键字"
                          className="h-8 text-xs"
                          value={filters.summary}
                          onChange={(event) =>
                            updateFilter("summary", event.target.value)
                          }
                        />
                      </TableHead>
                      <TableHead>
                        <div className="flex gap-1">
                          <Input
                            aria-label="开始日期"
                            type="date"
                            className="h-8 w-32 text-xs"
                            value={filters.from}
                            onChange={(event) =>
                              updateFilter("from", event.target.value)
                            }
                          />
                          <Input
                            aria-label="结束日期"
                            type="date"
                            className="h-8 w-32 text-xs"
                            value={filters.to}
                            onChange={(event) =>
                              updateFilter("to", event.target.value)
                            }
                          />
                        </div>
                      </TableHead>
                      <TableHead>
                        <select
                          aria-label="按实例状态筛选"
                          className="h-8 w-full rounded-md border border-input bg-transparent px-2 text-xs"
                          value={filters.status}
                          onChange={(event) =>
                            updateFilter("status", event.target.value)
                          }
                        >
                          {FEISHU_STATUS_FILTER_OPTIONS.map((option) => (
                            <option key={option.value} value={option.value}>
                              {option.label}
                            </option>
                          ))}
                        </select>
                      </TableHead>
                      <TableHead>
                        <select
                          aria-label="按 API 操作能力筛选"
                          className="h-8 w-full rounded-md border border-input bg-transparent px-2 text-xs"
                          value={filters.api}
                          onChange={(event) =>
                            updateFilter(
                              "api",
                              event.target.value as Filters["api"],
                            )
                          }
                        >
                          <option value="all">全部</option>
                          <option value="supported">支持</option>
                          <option value="unsupported">不支持</option>
                        </select>
                      </TableHead>
                      <TableHead>
                        <select
                          aria-label="按来源筛选"
                          className="h-8 w-full rounded-md border border-input bg-transparent px-2 text-xs"
                          value={filters.external}
                          onChange={(event) =>
                            updateFilter(
                              "external",
                              event.target.value as Filters["external"],
                            )
                          }
                        >
                          <option value="all">全部</option>
                          <option value="feishu">飞书</option>
                          <option value="external">外部</option>
                        </select>
                      </TableHead>
                      <TableHead />
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {filteredTasks.map((task) => {
                      const operability = taskOperability(task);
                      const checked = Boolean(selected[task.taskId]);
                      const expanded = expandedTaskId === task.taskId;
                      const statusLabel = taskStatusLabel(task);
                      return (
                        <Fragment key={task.taskId}>
                          <TableRow
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
                              className="max-w-64 font-medium"
                              title={task.title}
                            >
                              <span className="block truncate">
                                {task.title}
                              </span>
                              {!operability.operable && (
                                <span className="block text-xs font-normal text-muted-foreground">
                                  {operability.reason}
                                </span>
                              )}
                            </TableCell>
                            <TableCell
                              className="max-w-56"
                              title={task.definitionName}
                            >
                              <span className="block truncate">
                                {task.definitionName}
                              </span>
                              <span className="block truncate text-xs text-muted-foreground">
                                {task.definitionCode || "—"}
                              </span>
                            </TableCell>
                            <TableCell>{task.initiatorName ?? "—"}</TableCell>
                            <TableCell
                              className="max-w-72"
                              title={task.summaries.join("；")}
                            >
                              <span className="block truncate">
                                {task.summaries.join("；") || "—"}
                              </span>
                            </TableCell>
                            <TableCell className="text-xs tabular-nums">
                              {timeText(task.createdAt)}
                            </TableCell>
                            <TableCell>
                              <StatusBadge tone={statusTone(statusLabel)}>
                                {statusLabel}
                              </StatusBadge>
                            </TableCell>
                            <TableCell>
                              <StatusBadge
                                tone={
                                  task.supportApiOperate
                                    ? "positive"
                                    : "warning"
                                }
                              >
                                {task.supportApiOperate ? "支持" : "不支持"}
                              </StatusBadge>
                            </TableCell>
                            <TableCell>
                              <StatusBadge
                                tone={task.external ? "warning" : "neutral"}
                              >
                                {task.external ? "外部" : "飞书"}
                              </StatusBadge>
                            </TableCell>
                            <TableCell>
                              <Button
                                variant="ghost"
                                size="sm"
                                aria-expanded={expanded}
                                onClick={() => toggleDetail(task)}
                              >
                                {expanded ? (
                                  <ChevronDown className="size-4" />
                                ) : (
                                  <ChevronRight className="size-4" />
                                )}
                                {expanded ? "收起" : "展开"}
                              </Button>
                            </TableCell>
                          </TableRow>
                          {expanded && (
                            <TableRow>
                              <TableCell
                                colSpan={10}
                                className="bg-muted/20 p-4"
                              >
                                <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
                                  <span className="text-xs text-muted-foreground">
                                    实例 {task.instanceCode} ·{" "}
                                    {feishuStatusLabel(
                                      task.instanceStatus ?? task.status,
                                    )}
                                  </span>
                                  <Button
                                    variant="outline"
                                    size="sm"
                                    onClick={() => loadDetail(task)}
                                  >
                                    <RefreshCw className="size-3.5" />
                                    刷新详情
                                  </Button>
                                </div>
                                <BatchTaskDetailBody
                                  loading={detailLoadingId === task.taskId}
                                  errorMessage={detailErrors[task.taskId] ?? ""}
                                  detail={details[task.taskId] ?? null}
                                />
                              </TableCell>
                            </TableRow>
                          )}
                        </Fragment>
                      );
                    })}
                  </TableBody>
                </Table>
              </div>
            )}
          </section>
        </div>
      </main>

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

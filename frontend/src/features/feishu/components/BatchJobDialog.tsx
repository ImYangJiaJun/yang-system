import { useEffect, useState } from "react";

import { Button } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/shared/ui/table";

import {
  cancelBatchJob,
  fetchBatchJob,
  type BatchJob,
  type BatchJobItem,
} from "../batch-approval-api";
import { StatusBadge, type ToneName } from "./StatusBadge";

const TERMINAL_JOB_STATUSES = new Set([
  "succeeded",
  "partial",
  "failed",
  "cancelled",
]);

const JOB_STATUS_LABELS: Record<string, string> = {
  pending: "排队中",
  running: "执行中",
  succeeded: "全部成功",
  partial: "部分成功",
  failed: "失败",
  cancelled: "已取消",
};

const ITEM_STATUS_LABELS: Record<string, string> = {
  pending: "排队中",
  running: "执行中",
  succeeded: "成功",
  failed: "失败",
  unknown: "结果未知",
  skipped: "已跳过",
};

function statusTone(status: string): ToneName {
  switch (status) {
    case "succeeded":
      return "positive";
    case "partial":
    case "unknown":
      return "warning";
    case "failed":
      return "danger";
    case "running":
      return "info";
    default:
      return "neutral";
  }
}

function statusLabel(labels: Record<string, string>, status: string): string {
  // 未知状态原样显示，不把它画成一个看起来正常的中文标签。
  return labels[status] ?? status;
}

/// 批量 job 的进度与逐条结果。
///
/// 轮询只读 `GET /jobs/{id}`；取消只停止未开始项，`running` 项允许完成。失败项可以
/// 重试，但会**新建一个 job**（新幂等键），不会在原 job 上重放。`unknown` 不提供
/// 一键重试：飞书写接口没有幂等保证，重复提交会重复审批。
export function BatchJobDialog({
  open,
  jobId,
  actionLabel,
  accessToken,
  retrying,
  onClose,
  onRetryFailed,
}: {
  open: boolean;
  jobId: string | null;
  actionLabel: string;
  accessToken: string | undefined;
  retrying: boolean;
  onClose: () => void;
  onRetryFailed: (items: BatchJobItem[]) => void;
}) {
  const [job, setJob] = useState<BatchJob | null>(null);
  const [errorMessage, setErrorMessage] = useState("");
  const [cancelling, setCancelling] = useState(false);

  useEffect(() => {
    if (!open || !jobId) return;
    const controller = new AbortController();
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    setJob(null);
    setErrorMessage("");

    const tick = async () => {
      try {
        const next = await fetchBatchJob(jobId, accessToken, controller.signal);
        if (stopped) return;
        setJob(next);
        setErrorMessage("");
        if (!TERMINAL_JOB_STATUSES.has(next.status)) {
          timer = setTimeout(() => void tick(), 1500);
        }
      } catch (cause) {
        if (stopped) return;
        setErrorMessage(
          cause instanceof Error ? cause.message : "任务进度加载失败",
        );
        // 网络抖动静默重试；明确失败（如 404）也会继续，直到用户关闭。
        timer = setTimeout(() => void tick(), 3000);
      }
    };
    void tick();

    return () => {
      stopped = true;
      controller.abort();
      if (timer) clearTimeout(timer);
    };
  }, [open, jobId, accessToken]);

  const cancel = async () => {
    if (!jobId || cancelling) return;
    if (
      !window.confirm(
        "确定取消该批量任务吗？排队中的任务会跳过，正在执行的任务允许完成。",
      )
    ) {
      return;
    }
    setCancelling(true);
    setErrorMessage("");
    try {
      await cancelBatchJob(jobId, accessToken);
      const next = await fetchBatchJob(jobId, accessToken);
      setJob(next);
    } catch (cause) {
      setErrorMessage(
        cause instanceof Error ? cause.message : "取消失败，请稍后重试",
      );
    } finally {
      setCancelling(false);
    }
  };

  const terminal = job !== null && TERMINAL_JOB_STATUSES.has(job.status);
  const failedItems =
    job?.items.filter((item) => item.status === "failed") ?? [];
  const finished = job ? job.succeeded + job.failed + job.unknown : 0;
  const total = job?.total ?? 0;
  const progress = total > 0 ? Math.min(100, (finished / total) * 100) : 0;

  return (
    <Dialog
      open={open}
      onOpenChange={(nextOpen) => {
        if (!nextOpen && !cancelling && !retrying) onClose();
      }}
    >
      <DialogContent
        aria-label="批量任务进度"
        className="sm:max-w-2xl"
        showCloseButton={!cancelling && !retrying}
      >
        <DialogHeader>
          <DialogTitle>{actionLabel}进度</DialogTitle>
          <DialogDescription>
            后台逐条执行，不会自动重试写请求；结果未知的任务请到飞书审批中核对。
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          {errorMessage && (
            <p
              role="alert"
              className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
            >
              {errorMessage}（系统会继续尝试刷新）
            </p>
          )}

          {job === null ? (
            <p role="status" className="text-sm text-muted-foreground">
              任务已提交，等待执行…
            </p>
          ) : (
            <>
              <div className="flex flex-wrap items-center gap-3 text-sm">
                <StatusBadge tone={statusTone(job.status)}>
                  {statusLabel(JOB_STATUS_LABELS, job.status)}
                </StatusBadge>
                <span>
                  共 {job.total} 条：成功 {job.succeeded} · 失败 {job.failed} ·
                  未知 {job.unknown}
                </span>
              </div>
              <div
                className="h-2 w-full overflow-hidden rounded-full bg-muted"
                role="progressbar"
                aria-valuemin={0}
                aria-valuemax={Math.max(total, 1)}
                aria-valuenow={finished}
                aria-label="批量任务进度"
              >
                <div
                  className="h-full bg-primary transition-all"
                  style={{ width: `${progress}%` }}
                />
              </div>

              {job.status === "cancelled" && (
                <p className="rounded-md border border-border bg-muted/50 px-3 py-2 text-sm">
                  任务已取消；排队中的条目已跳过，正在执行的条目允许完成。
                </p>
              )}

              {job.items.length === 0 ? (
                <p className="text-sm text-muted-foreground">
                  暂无逐条结果，等待 worker 领取任务…
                </p>
              ) : (
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>任务</TableHead>
                      <TableHead>结果</TableHead>
                      <TableHead>说明</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {job.items.map((item) => (
                      <TableRow key={`${item.taskId}-${item.instanceCode}`}>
                        <TableCell className="max-w-48 truncate">
                          <span title={item.title ?? item.taskId}>
                            {item.title ?? item.taskId}
                          </span>
                        </TableCell>
                        <TableCell>
                          <StatusBadge tone={statusTone(item.status)}>
                            {statusLabel(ITEM_STATUS_LABELS, item.status)}
                          </StatusBadge>
                          {item.resultCode !== null && (
                            <span className="ml-2 text-xs text-muted-foreground">
                              {item.resultCode}
                            </span>
                          )}
                        </TableCell>
                        <TableCell
                          className="max-w-64 truncate text-xs text-muted-foreground"
                          title={item.message ?? undefined}
                        >
                          {item.message ??
                            (item.status === "unknown"
                              ? "结果未知，不自动重发；请到飞书审批中确认"
                              : "—")}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              )}
            </>
          )}
        </div>

        <DialogFooter>
          {!terminal && job !== null && (
            <Button
              type="button"
              variant="outline"
              disabled={cancelling || retrying}
              onClick={() => void cancel()}
            >
              {cancelling ? "取消中…" : "取消任务"}
            </Button>
          )}
          {terminal && failedItems.length > 0 && (
            <Button
              type="button"
              variant="outline"
              disabled={retrying}
              onClick={() => onRetryFailed(failedItems)}
            >
              {retrying
                ? "正在创建重试任务…"
                : `重试失败项（${failedItems.length}）`}
            </Button>
          )}
          <Button
            type="button"
            variant="ghost"
            disabled={cancelling || retrying}
            onClick={onClose}
          >
            关闭
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

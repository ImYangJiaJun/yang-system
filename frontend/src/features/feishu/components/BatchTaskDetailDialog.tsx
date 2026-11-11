import { useEffect, useState } from "react";

import { formatProductDateTime } from "@/shared/lib/product-locale";
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
  feishuStatusLabel,
  fetchBatchTaskDetail,
  type BatchTask,
  type BatchTaskDetail,
} from "../batch-approval-api";

/// 表单值 → 纯文本。对象/嵌套结构直接 JSON 串，避免把 React 无法渲染的对象
/// 塞进文本节点（也会把飞书返回的结构藏起来）。
function valueText(value: unknown): string {
  if (value === null || value === undefined || value === "") return "—";
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") {
    return String(value);
  }
  if (Array.isArray(value)) {
    const allPrimitive = value.every(
      (entry) =>
        entry === null ||
        ["string", "number", "boolean"].includes(typeof entry),
    );
    return allPrimitive
      ? value.map((entry) => String(entry ?? "—")).join("、")
      : JSON.stringify(value);
  }
  return JSON.stringify(value);
}

function timeText(value: number | null): string {
  return value === null ? "—" : formatProductDateTime(value);
}

/// 审批详情正文：表单 + 当前节点 + 时间线。Dialog 与表格内联展开共用。
export function BatchTaskDetailBody({
  loading,
  errorMessage,
  detail,
}: {
  loading: boolean;
  errorMessage: string;
  detail: BatchTaskDetail | null;
}) {
  if (loading) {
    return (
      <p role="status" className="text-sm text-muted-foreground">
        正在加载审批详情…
      </p>
    );
  }
  if (errorMessage) {
    return (
      <p role="alert" className="text-sm text-destructive">
        {errorMessage}
      </p>
    );
  }
  if (!detail) return null;
  return (
    <div className="grid gap-5 md:grid-cols-2">
      <section aria-label="表单">
        <h3 className="text-sm font-medium">表单</h3>
        {detail.form.length === 0 ? (
          <p className="mt-2 text-sm text-muted-foreground">
            该审批没有可展示的表单字段。
          </p>
        ) : (
          <dl className="mt-2 grid grid-cols-[minmax(6rem,auto)_1fr] gap-x-4 gap-y-2 text-sm">
            {detail.form.map((field, index) => (
              <div key={`${field.name}-${index}`} className="contents">
                <dt className="text-muted-foreground">{field.name}</dt>
                <dd className="break-words">{valueText(field.value)}</dd>
              </div>
            ))}
          </dl>
        )}
      </section>

      <section aria-label="当前节点">
        <h3 className="text-sm font-medium">当前节点</h3>
        <p className="mt-2 text-sm text-muted-foreground">
          {detail.currentNodeNames.length > 0
            ? detail.currentNodeNames.join(" → ")
            : "—"}
          {detail.status
            ? ` · 实例状态：${feishuStatusLabel(detail.status)}`
            : ""}
        </p>
      </section>

      <section aria-label="时间线" className="md:col-span-2">
        <h3 className="text-sm font-medium">时间线</h3>
        {detail.timeline.length === 0 ? (
          <p className="mt-2 text-sm text-muted-foreground">暂无时间线数据。</p>
        ) : (
          <ol className="mt-2 space-y-2">
            {detail.timeline.map((entry, index) => (
              <li
                key={`${entry.title}-${index}`}
                className="rounded-md border border-border px-3 py-2 text-sm"
              >
                <p className="font-medium">
                  {entry.title}
                  {entry.status && (
                    <span className="ml-2 text-xs text-muted-foreground">
                      {feishuStatusLabel(entry.status)}
                    </span>
                  )}
                </p>
                <p className="text-xs text-muted-foreground">
                  {entry.userName ?? "—"} · {timeText(entry.time)}
                </p>
                {entry.comment && (
                  <p className="mt-1 text-sm">{entry.comment}</p>
                )}
              </li>
            ))}
          </ol>
        )}
      </section>
    </div>
  );
}

/// 审批详情 Dialog：表单 + 当前节点 + 时间线。只读，不在此处执行写操作。
export function BatchTaskDetailDialog({
  open,
  task,
  accessToken,
  onClose,
}: {
  open: boolean;
  task: BatchTask | null;
  accessToken: string | undefined;
  onClose: () => void;
}) {
  const [detail, setDetail] = useState<BatchTaskDetail | null>(null);
  const [loading, setLoading] = useState(false);
  const [errorMessage, setErrorMessage] = useState("");

  useEffect(() => {
    if (!open || !task) return;
    const controller = new AbortController();
    setDetail(null);
    setErrorMessage("");
    setLoading(true);
    void fetchBatchTaskDetail(
      task.taskId,
      task.instanceCode,
      accessToken,
      controller.signal,
    )
      .then((next) => {
        if (!controller.signal.aborted) setDetail(next);
      })
      .catch((cause: unknown) => {
        if (!controller.signal.aborted) {
          setErrorMessage(
            cause instanceof Error ? cause.message : "审批详情加载失败",
          );
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
  }, [open, task, accessToken]);

  return (
    <Dialog
      open={open}
      onOpenChange={(nextOpen) => {
        if (!nextOpen) onClose();
      }}
    >
      <DialogContent
        aria-label="审批详情"
        className="sm:max-w-2xl"
        showCloseButton={!loading}
      >
        <DialogHeader>
          <DialogTitle>{task?.title ?? "审批详情"}</DialogTitle>
          <DialogDescription>
            {task?.definitionName ?? ""}
            {task?.initiatorName ? ` · 发起人：${task.initiatorName}` : ""}
          </DialogDescription>
        </DialogHeader>

        <BatchTaskDetailBody
          loading={loading}
          errorMessage={errorMessage}
          detail={detail}
        />

        <DialogFooter>
          <Button type="button" variant="outline" onClick={onClose}>
            关闭
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

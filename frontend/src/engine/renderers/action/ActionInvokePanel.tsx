import { useId, useRef, useState } from "react";

import {
  ApiError,
  invokeAction,
  type InvocationResult,
} from "@/engine/http/client";
import { useSessionCredentials } from "@/engine/session/use-session";
import { Badge } from "@/shared/ui/badge";
import { Button } from "@/shared/ui/button";
import { ContractError } from "@/engine/contracts/ui-catalog";
import type { ActionDemoSchema } from "@/engine/contracts/ui-catalog";
import { initialObject } from "@/engine/contracts/json-schema";
import { JsonSchemaForm } from "@/engine/renderers/form/JsonSchemaForm";
import { useToast } from "@/shared/lib/toast";

/**
 * Action 调试面板（旧 ActionDemo.vue 语义平移）：
 * 动态表单 + 真实调用 + 结果/错误展示（含 requestId 与 details）。
 */
export function ActionInvokePanel({
  action,
  initialValues,
}: {
  action: ActionDemoSchema;
  initialValues?: Record<string, unknown>;
}) {
  const session = useSessionCredentials();
  const formId = useId();
  const toast = useToast();
  const [loading, setLoading] = useState(false);
  const [result, setResult] = useState<InvocationResult>();
  const abortRef = useRef<AbortController | undefined>(undefined);

  const submit = async (values: Record<string, unknown>) => {
    if (loading) return;
    if (result?.blobUrl) URL.revokeObjectURL(result.blobUrl);
    setResult(undefined);
    const controller = new AbortController();
    abortRef.current = controller;
    setLoading(true);
    try {
      const invocation = await invokeAction(
        action,
        values,
        session,
        controller.signal,
      );
      setResult(invocation);
      toast.success(`调用成功（HTTP ${invocation.status}）`);
    } catch (cause) {
      if (cause instanceof ApiError) {
        toast.error(cause.message, { duration: 8000 });
      } else if (cause instanceof ContractError) {
        toast.error(cause.message, { duration: 8000 });
      } else if (cause instanceof Error && cause.name !== "AbortError") {
        toast.error(cause.message);
      }
    } finally {
      setLoading(false);
    }
  };

  return (
    <section data-testid="action-demo" className="space-y-4">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="space-y-1">
          <div className="flex gap-1.5">
            <Badge>{action.method}</Badge>
            {action.requires_auth && <Badge variant="outline">需要认证</Badge>}
            <Badge variant="outline">{action.response_kind}</Badge>
          </div>
          <h2 className="text-lg font-semibold">
            {action.title || action.operation_id}
          </h2>
          <p className="text-sm text-muted-foreground">
            {action.description || "该 Action 未提供说明。"}
          </p>
        </div>
        <code className="rounded bg-muted px-2 py-1 text-xs">
          {action.operation_id}
        </code>
      </div>

      {action.request_media_type === "multipart" && (
        <p className="rounded-md border border-border bg-muted/50 px-3 py-2 text-sm">
          受限 multipart：最多 {action.multipart?.max_files ?? 0} 个文件，单文件{" "}
          {action.multipart?.max_file_bytes ?? 0} bytes
        </p>
      )}

      <p className="text-sm">
        <span className="mr-2 font-medium">{action.method}</span>
        <code className="text-muted-foreground">{action.path}</code>
      </p>

      <JsonSchemaForm
        key={action.operation_id}
        formId={formId}
        schema={action.input_schema}
        params={action.params}
        defaultValues={{
          ...initialObject(action.input_schema),
          ...(initialValues ?? {}),
        }}
        multipart={action.multipart}
        onSubmit={(values) => void submit(values)}
      />

      {action.params.length > 0 && (
        <div className="flex flex-wrap gap-2 text-sm">
          {action.params.map((parameter) => (
            <span key={parameter.name} className="flex items-center gap-1">
              <Badge variant="outline">{parameter.source}</Badge>
              {parameter.title || parameter.name}
            </span>
          ))}
        </div>
      )}

      <div className="flex gap-2">
        <Button type="submit" form={formId} disabled={loading}>
          {loading ? "调用中…" : "发起真实调用"}
        </Button>
        {loading && (
          <Button variant="ghost" onClick={() => abortRef.current?.abort()}>
            取消
          </Button>
        )}
      </div>

      {result && (
        <div data-testid="action-result" className="space-y-2">
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <Badge>
              {result.kind === "redirect" && result.status === 0
                ? "Redirect 已拦截"
                : `HTTP ${result.status}`}
            </Badge>
            <span>{result.durationMs} ms</span>
            {result.requestId && <span>request-id: {result.requestId}</span>}
          </div>
          {result.kind === "json" && (
            <pre className="overflow-x-auto rounded-md bg-muted p-3 text-xs">
              {JSON.stringify(result.data, null, 2)}
            </pre>
          )}
          {result.kind === "redirect" && (
            <p className="rounded-md border border-border p-3 text-sm">
              服务端请求重定向：
              {result.location || "浏览器安全策略隐藏 Location，页面未自动跳转"}
            </p>
          )}
          {(result.kind === "download" || result.kind === "preview") && (
            <p className="text-sm">
              <a
                className="text-primary underline"
                href={result.blobUrl}
                download={
                  result.kind === "download" ? result.filename || "" : undefined
                }
                target="_blank"
                rel="noreferrer"
              >
                {result.kind === "download" ? "下载文件" : "打开预览"}
              </a>
              {result.filename && (
                <span className="ml-2 text-muted-foreground">
                  {result.filename}
                </span>
              )}
            </p>
          )}
        </div>
      )}
    </section>
  );
}

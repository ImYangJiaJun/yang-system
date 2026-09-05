import { useCallback, useEffect, useRef, useState } from "react";
import { z } from "zod";

import { invokeAction } from "@/engine/http/client";
import { useSessionCredentials } from "@/engine/session/use-session";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import type { CustomViewProps } from "../registry";

const insightSchema = z.object({
  total: z.number().int().nonnegative(),
  active: z.number().int().nonnegative(),
  draft: z.number().int().nonnegative(),
});

/// 项目运行洞察（旧 DemoItemInsight.vue 移植）：数据仍通过声明的 Action 获取。
export default function DemoItemInsight({
  presentation,
  actions,
  onClose,
}: CustomViewProps) {
  const session = useSessionCredentials();
  const [insight, setInsight] = useState<z.infer<typeof insightSchema>>();
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const abortRef = useRef<AbortController | undefined>(undefined);

  const action = actions.find(
    (candidate) => candidate.operation_id === presentation.operation_id,
  );

  const load = useCallback(async () => {
    if (!action) {
      setError(`目录缺少 ${presentation.operation_id}`);
      return;
    }
    abortRef.current?.abort();
    const controller = new AbortController();
    abortRef.current = controller;
    setLoading(true);
    setError("");
    try {
      const result = await invokeAction(action, {}, session, controller.signal);
      if (result.kind !== "json") throw new Error("洞察 Action 必须返回 JSON");
      setInsight(insightSchema.parse(result.data));
    } catch (cause) {
      if (cause instanceof Error && cause.name === "AbortError") return;
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      if (abortRef.current === controller) setLoading(false);
    }
  }, [action, presentation.operation_id, session]);

  useEffect(() => {
    void load();
    return () => abortRef.current?.abort();
  }, [load]);

  return (
    <section aria-label="项目运行洞察">
      <header className="mb-4 flex items-start justify-between gap-3">
        <div className="space-y-1">
          <Badge variant="secondary">自定义 View</Badge>
          <h2 className="text-lg font-semibold">项目运行洞察</h2>
          <p className="text-sm text-muted-foreground">
            由静态 view_id 注册表加载，数据仍通过声明的 Action 获取。
          </p>
        </div>
        <Button variant="outline" size="sm" onClick={onClose}>
          返回通用表格
        </Button>
      </header>
      {error ? (
        <p
          role="alert"
          className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
        >
          {error}
        </p>
      ) : loading && !insight ? (
        <div className="grid grid-cols-3 gap-3">
          <Skeleton className="h-24" />
          <Skeleton className="h-24" />
          <Skeleton className="h-24" />
        </div>
      ) : insight ? (
        <div className="grid grid-cols-3 gap-3">
          <article className="rounded-lg border border-border p-4">
            <span className="text-sm text-muted-foreground">项目总数</span>
            <p className="mt-1 text-2xl font-semibold">{insight.total}</p>
          </article>
          <article className="rounded-lg border border-border p-4">
            <span className="text-sm text-muted-foreground">运行中</span>
            <p className="mt-1 text-2xl font-semibold">{insight.active}</p>
          </article>
          <article className="rounded-lg border border-border p-4">
            <span className="text-sm text-muted-foreground">草稿</span>
            <p className="mt-1 text-2xl font-semibold">{insight.draft}</p>
          </article>
        </div>
      ) : null}
    </section>
  );
}

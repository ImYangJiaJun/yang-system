import { Suspense, useState } from "react";
import { Link, useOutletContext, useSearchParams } from "react-router";

import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import type { ActionPresentationSchema } from "@/contracts/ui-catalog";
import { CustomViewBoundary } from "@/custom/custom-view-boundary";
import { resolveCustomView } from "@/custom/registry";
import type { ShellContext } from "@/layout/AppLayout";
import { TableView } from "@/renderers/table/TableView";

/**
 * 未分配视图承载页（旧 BusinessPage.vue 语义）：?view=<viewId> 指定，
 * 缺省取第一个；custom interaction 经静态注册表解析，未注册/失败回退通用表格。
 */
export default function BusinessPage() {
  const { catalog } = useOutletContext<ShellContext>();
  const [searchParams] = useSearchParams();
  const [custom, setCustom] = useState<ActionPresentationSchema | null>(null);
  const [notice, setNotice] = useState("");

  const viewId = searchParams.get("view");
  const view =
    catalog.table_views.find((candidate) => candidate.view_id === viewId) ??
    catalog.table_views[0];

  if (!view) {
    return (
      <div className="flex flex-col items-center gap-3 p-12 text-center">
        <h2 className="text-lg font-medium">暂无可访问的业务页面</h2>
        <p className="text-sm text-muted-foreground">
          请确认当前身份拥有页面权限，或稍后刷新后端目录。
        </p>
      </div>
    );
  }

  const openCustom = (presentation: ActionPresentationSchema) => {
    if (!resolveCustomView(presentation.view_id)) {
      setNotice(
        `自定义页面 ${presentation.view_id ?? "未声明"} 未注册，已保留通用业务页`,
      );
      return;
    }
    setNotice("");
    setCustom(presentation);
  };

  const CustomComponent = custom
    ? resolveCustomView(custom.view_id)
    : undefined;

  return (
    <div className="p-6">
      {notice && (
        <p
          role="status"
          className="mb-3 rounded-md border border-border bg-muted/50 px-3 py-2 text-sm"
        >
          {notice}
        </p>
      )}
      {custom && CustomComponent ? (
        <CustomViewBoundary
          resetKey={custom.operation_id}
          onError={(message) => {
            setCustom(null);
            setNotice(`自定义页面加载失败，已回退通用业务页：${message}`);
          }}
        >
          <Suspense
            fallback={
              <div className="space-y-3" aria-label="自定义页面加载中">
                <Skeleton className="h-8 w-64" />
                <Skeleton className="h-40 w-full" />
              </div>
            }
          >
            <CustomComponent
              presentation={custom}
              actions={catalog.actions}
              onClose={() => setCustom(null)}
            />
          </Suspense>
        </CustomViewBoundary>
      ) : (
        <TableView
          key={view.view_id}
          view={view}
          actions={catalog.actions}
          onCustom={openCustom}
        />
      )}
      {catalog.table_views.length > 1 && (
        <div className="mt-4 flex gap-2">
          {catalog.table_views.map((candidate) => (
            <Button
              key={candidate.view_id}
              variant={
                candidate.view_id === view.view_id ? "default" : "outline"
              }
              size="sm"
              asChild
            >
              <Link
                to={`/business?view=${encodeURIComponent(candidate.view_id)}`}
              >
                {candidate.title || candidate.table}
              </Link>
            </Button>
          ))}
        </div>
      )}
    </div>
  );
}

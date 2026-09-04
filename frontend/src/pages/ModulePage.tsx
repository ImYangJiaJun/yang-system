import { Suspense, useEffect, useState } from "react";
import { Link, useNavigate, useOutletContext, useParams } from "react-router";

import { moduleView } from "@/catalog/module-pages";
import { buildNavigationPages, WORKSPACE_IDENTITY } from "@/app/navigation";
import { useIdentity } from "@/app/use-identity";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { CustomViewBoundary } from "@/custom/custom-view-boundary";
import { resolveCustomView } from "@/custom/registry";
import type { ActionPresentationSchema } from "@/contracts/ui-catalog";
import type { ShellContext } from "@/layout/AppLayout";
import { PrimaryActionPanel } from "@/renderers/module/primary-action-panel";
import { TableView } from "@/renderers/table/TableView";

export default function ModulePage() {
  const { catalog } = useOutletContext<ShellContext>();
  const { moduleId = "", viewId } = useParams();
  const { identity } = useIdentity();
  const navigate = useNavigate();

  const page = buildNavigationPages(catalog).find(
    (candidate) => candidate.id === moduleId,
  );
  const effectiveView = page ? moduleView(page, viewId) : undefined;
  const [custom, setCustom] = useState<ActionPresentationSchema | null>(null);
  const [customNotice, setCustomNotice] = useState("");

  // 切换模块/视图时退出自定义视图。
  useEffect(() => {
    setCustom(null);
    setCustomNotice("");
  }, [moduleId, viewId]);

  // 身份守卫：模块页只对声明身份可见（工作台兜底视图不受身份约束）。
  // 延迟一个宏任务再跳转：身份切换是「select + navigate」两步，守卫渲染可能短暂
  // 落在旧路由+新身份的不一致帧上，延迟后不一致已被后续提交清除（定时器被 cleanup 取消）。
  const identityMismatch = Boolean(
    page &&
    page.identity !== WORKSPACE_IDENTITY &&
    identity &&
    page.identity !== identity,
  );
  useEffect(() => {
    if (!identityMismatch) return;
    const timer = window.setTimeout(() => navigate("/", { replace: true }), 0);
    return () => window.clearTimeout(timer);
  }, [identityMismatch, navigate]);

  if (!page) {
    return (
      <div className="flex flex-col items-center gap-3 p-12 text-center">
        <h2 className="text-lg font-medium">当前身份无法访问该模块</h2>
        <p className="text-sm text-muted-foreground">
          页面只会为服务端已授权的 Module 生成。
        </p>
        <Button variant="outline" asChild>
          <Link to="/">返回应用中心</Link>
        </Button>
      </div>
    );
  }

  // 身份不一致：渲染空并等待上一 effect 的延迟跳转（见上）。
  if (identityMismatch) {
    return null;
  }

  const openCustom = (presentation: ActionPresentationSchema) => {
    if (!resolveCustomView(presentation.view_id)) {
      setCustomNotice(
        `自定义页面 ${presentation.view_id ?? "未声明"} 未注册，已保留通用模块页`,
      );
      return;
    }
    setCustomNotice("");
    setCustom(presentation);
  };

  const CustomComponent = custom
    ? resolveCustomView(custom.view_id)
    : undefined;

  return (
    <div className="p-6">
      <header className="mb-4 flex items-center gap-3">
        <div>
          <h1 className="text-xl font-semibold tracking-tight">{page.title}</h1>
          {page.description && (
            <p className="text-sm text-muted-foreground">{page.description}</p>
          )}
        </div>
      </header>
      {page.views.length > 1 && (
        <nav className="mb-4 flex gap-1 border-b border-border">
          {page.views.map((view) => (
            <Link
              key={view.view_id}
              to={`/m/${page.id}/v/${view.view_id}`}
              className={
                view.view_id === effectiveView?.view_id
                  ? "border-b-2 border-primary px-3 py-1.5 text-sm font-medium"
                  : "px-3 py-1.5 text-sm text-muted-foreground hover:text-foreground"
              }
            >
              {view.title || view.table}
            </Link>
          ))}
        </nav>
      )}
      {customNotice && (
        <p
          role="status"
          className="mb-3 rounded-md border border-border bg-muted/50 px-3 py-2 text-sm"
        >
          {customNotice}
        </p>
      )}
      {custom && CustomComponent ? (
        <CustomViewBoundary
          resetKey={custom.operation_id}
          onError={(message) => {
            setCustom(null);
            setCustomNotice(`自定义页面加载失败，已回退通用模块页：${message}`);
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
      ) : effectiveView ? (
        <TableView
          key={`${page.id}:${effectiveView.view_id}`}
          view={effectiveView}
          actions={catalog.actions}
          onCustom={openCustom}
        />
      ) : (
        // 无视图模块：回退为 primaryAction 数据卡片（旧 ModulePage.vue 语义）。
        <PrimaryActionPanel page={page} actions={catalog.actions} />
      )}
    </div>
  );
}

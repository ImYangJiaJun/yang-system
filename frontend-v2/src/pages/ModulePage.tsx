import { Link, useOutletContext, useParams } from "react-router";

import { moduleView } from "@/catalog/module-pages";
import { buildNavigationPages } from "@/app/navigation";
import type { ShellContext } from "@/layout/AppLayout";
import { Button } from "@/components/ui/button";
import { TableView } from "@/renderers/table/TableView";

export default function ModulePage() {
  const { catalog } = useOutletContext<ShellContext>();
  const { moduleId = "", viewId } = useParams();

  const page = buildNavigationPages(catalog).find(
    (candidate) => candidate.id === moduleId,
  );
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

  const effectiveView = moduleView(page, viewId);

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
      {effectiveView ? (
        <TableView
          key={`${page.id}:${effectiveView.view_id}`}
          view={effectiveView}
          actions={catalog.actions}
        />
      ) : (
        <p className="text-sm text-muted-foreground">
          该模块未声明任何 TableView，通用模块页无法渲染。
        </p>
      )}
    </div>
  );
}

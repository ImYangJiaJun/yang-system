import { useMemo, useState } from "react";
import { useOutletContext } from "react-router";

import { Input } from "@/components/ui/input";
import type { ShellContext } from "@/layout/AppLayout";
import { productLowerCase } from "@/lib/product-locale";
import { ActionInvokePanel } from "@/renderers/action/ActionInvokePanel";
import { TableView } from "@/renderers/table/TableView";
import { cn } from "@/lib/utils";

type NavigationMode = "views" | "actions";

/**
 * 契约驱动开发工作台（旧 WorkbenchPage.vue 语义）：
 * 全量 Action/TableView 浏览与调试。仅开发构建注册路由（见 app/routes.tsx
 * 的 import.meta.env.DEV 门控），生产构建不含此页。
 */
export default function WorkbenchPage() {
  const { catalog } = useOutletContext<ShellContext>();
  const [mode, setMode] = useState<NavigationMode>(
    catalog.table_views.length ? "views" : "actions",
  );
  const [query, setQuery] = useState("");
  const [selectedViewId, setSelectedViewId] = useState("");
  const [selectedOperationId, setSelectedOperationId] = useState("");

  const keyword = productLowerCase(query.trim());
  const views = useMemo(
    () =>
      catalog.table_views.filter((view) =>
        keyword
          ? [view.view_id, view.title, view.table, view.data_action]
              .map(productLowerCase)
              .join(" ")
              .includes(keyword)
          : true,
      ),
    [catalog.table_views, keyword],
  );
  const actions = useMemo(
    () =>
      catalog.actions.filter((action) =>
        keyword
          ? [action.operation_id, action.title, action.description, action.path]
              .map(productLowerCase)
              .join(" ")
              .includes(keyword)
          : true,
      ),
    [catalog.actions, keyword],
  );
  const selectedView =
    views.find((view) => view.view_id === selectedViewId) ?? views[0];
  const selectedAction =
    actions.find((action) => action.operation_id === selectedOperationId) ??
    actions[0];

  return (
    <div className="flex h-full">
      <aside className="flex w-72 shrink-0 flex-col border-r border-border">
        <div className="space-y-2 border-b border-border p-3">
          <div className="flex gap-1" role="tablist">
            {(
              [
                ["views", "业务页面"],
                ["actions", "接口演示"],
              ] as const
            ).map(([value, label]) => (
              <button
                key={value}
                role="tab"
                aria-selected={mode === value}
                className={cn(
                  "flex-1 rounded-md px-2 py-1.5 text-sm",
                  mode === value
                    ? "bg-accent font-medium"
                    : "text-muted-foreground hover:text-foreground",
                )}
                onClick={() => setMode(value)}
              >
                {label}
              </button>
            ))}
          </div>
          <Input
            placeholder="搜索目录"
            aria-label="搜索目录"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
        </div>
        <div className="flex-1 overflow-y-auto p-2">
          {mode === "views"
            ? views.map((view) => (
                <button
                  key={view.view_id}
                  className={cn(
                    "w-full rounded-md px-2 py-1.5 text-left text-sm hover:bg-accent",
                    selectedView?.view_id === view.view_id && "bg-accent",
                  )}
                  onClick={() => setSelectedViewId(view.view_id)}
                >
                  {view.title || view.table}
                </button>
              ))
            : actions.map((action) => (
                <button
                  key={action.operation_id}
                  className={cn(
                    "w-full rounded-md px-2 py-1.5 text-left text-sm hover:bg-accent",
                    selectedAction?.operation_id === action.operation_id &&
                      "bg-accent",
                  )}
                  onClick={() => setSelectedOperationId(action.operation_id)}
                >
                  {action.title || action.operation_id}
                </button>
              ))}
        </div>
      </aside>

      <div className="min-w-0 flex-1 overflow-y-auto p-6">
        {mode === "views" && selectedView ? (
          <TableView
            key={selectedView.view_id}
            view={selectedView}
            actions={catalog.actions}
          />
        ) : mode === "actions" && selectedAction ? (
          <ActionInvokePanel action={selectedAction} />
        ) : (
          <p className="py-12 text-center text-sm text-muted-foreground">
            后端目录中没有当前身份可访问的 Action
          </p>
        )}
      </div>
    </div>
  );
}

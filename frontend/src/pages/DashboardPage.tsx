import { useEffect, useState } from "react";
import { Search } from "lucide-react";
import { Link, Navigate, useOutletContext } from "react-router";

import { useIdentity } from "@/app/use-identity";
import { Input } from "@/components/ui/input";
import {
  buildAccountModulePages,
  modulesForIdentity,
  unassignedViews,
  visibleAccountIdentities,
} from "@/catalog/module-pages";
import type { ShellContext } from "@/layout/AppLayout";
import { productLowerCase } from "@/lib/product-locale";

/// 应用中心（旧 DashboardPage.vue 语义）：身份过滤后的模块卡片 + 未分配视图入口。
export default function DashboardPage() {
  const { catalog } = useOutletContext<ShellContext>();
  const { identity, select } = useIdentity();
  const [query, setQuery] = useState("");

  const modulePages = buildAccountModulePages(catalog);
  const identities = visibleAccountIdentities(modulePages, catalog);

  // 单身份账号直接进入（旧语义：/roles 手动选择；M3 起单身份免选）。
  useEffect(() => {
    if (!identity && identities.length === 1 && identities[0]) {
      select(identities[0].id);
    }
  }, [identity, identities, select]);

  // 多身份且未选择（或所选身份已不可见）→ 选择页。
  if (
    identities.length > 1 &&
    (!identity || !identities.some((candidate) => candidate.id === identity))
  ) {
    return <Navigate to="/select-identity" replace />;
  }

  const keyword = productLowerCase(query.trim());
  const modules = modulesForIdentity(modulePages, identity).filter((module) =>
    keyword
      ? [module.id, module.title, module.description]
          .map(productLowerCase)
          .join(" ")
          .includes(keyword)
      : true,
  );
  const businessViews = unassignedViews(catalog).filter((view) =>
    keyword
      ? [view.title, view.table, view.view_id]
          .filter(Boolean)
          .some((value) => productLowerCase(value).includes(keyword))
      : true,
  );

  return (
    <div className="p-6">
      <div className="mb-6 flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="text-xl font-semibold tracking-tight">应用中心</h1>
          <p className="text-sm text-muted-foreground">
            选择当前账号可访问的业务模块
          </p>
        </div>
        <span className="rounded-md border border-border px-2 py-1 text-xs text-muted-foreground">
          服务已连接
        </span>
      </div>

      {modules.length + businessViews.length > 1 && (
        <div className="relative mb-6 max-w-md">
          <Search className="absolute top-2.5 left-3 size-4 text-muted-foreground" />
          <Input
            className="pl-9"
            placeholder="搜索功能"
            aria-label="搜索功能"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
        </div>
      )}

      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
        {modules.map((module) => (
          <Link
            key={module.id}
            to={`/m/${module.id}`}
            data-testid={`module-card-${module.id}`}
            className="rounded-lg border border-border bg-card p-5 transition-colors hover:border-primary/50"
          >
            <h2 className="font-semibold">{module.title}</h2>
            <p className="mt-1 text-xs text-muted-foreground">{module.id}</p>
            <p className="mt-2 line-clamp-2 text-sm text-muted-foreground">
              {module.description}
            </p>
          </Link>
        ))}
        {businessViews.map((view) => (
          <Link
            key={view.view_id}
            to={`/business?view=${encodeURIComponent(view.view_id)}`}
            data-testid={`view-card-${view.view_id}`}
            className="rounded-lg border border-border bg-card p-5 transition-colors hover:border-primary/50"
          >
            <h2 className="font-semibold">{view.title || view.table}</h2>
            <p className="mt-1 text-xs text-muted-foreground">{view.table}</p>
            <p className="mt-2 text-sm text-muted-foreground">
              {view.columns.length} 个字段 · 契约驱动页面
            </p>
          </Link>
        ))}
      </div>

      {modules.length + businessViews.length === 0 && (
        <p className="py-12 text-center text-sm text-muted-foreground">
          当前身份没有可访问的业务模块。
        </p>
      )}
    </div>
  );
}

import { useEffect, useState, type ComponentType } from "react";
import {
  Building2,
  CircleUser,
  LogOut,
  Moon,
  Puzzle,
  ShieldCheck,
  Sun,
  Table2,
  Users,
} from "lucide-react";
import { NavLink, Outlet } from "react-router";

import { useUiCatalog } from "@/api/use-catalog";
import { useSessionController } from "@/api/use-session";
import { buildNavigationPages, groupNavigationPages } from "@/app/navigation";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import type { UiCatalog } from "@/contracts/ui-catalog";
import { cn } from "@/lib/utils";

export type ShellContext = { catalog: UiCatalog };

/// 旧前端 Material Symbols 图标 token 到 lucide 组件的映射；未知 token 回退 Puzzle。
const ICONS: Record<string, ComponentType<{ className?: string }>> = {
  account: CircleUser,
  account_circle: CircleUser,
  admin_panel_settings: ShieldCheck,
  apartment: Building2,
  domain: Building2,
  extension: Puzzle,
  groups: Users,
  manage_accounts: Users,
  organization: Building2,
  organizations: Building2,
  person: CircleUser,
  table: Table2,
};

function ModuleIcon({ token }: { token: string }) {
  const Icon = ICONS[token] ?? Puzzle;
  return <Icon className="size-4 shrink-0" />;
}

/// 侧边栏底部用户区：登出入口（endSession 清空会话后由认证门控自动跳 /login）。
function SidebarSessionFooter() {
  const controller = useSessionController();
  const [loggingOut, setLoggingOut] = useState(false);
  const [error, setError] = useState("");

  const onLogout = async () => {
    if (loggingOut) return;
    setLoggingOut(true);
    setError("");
    try {
      await controller.endSession();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      setLoggingOut(false);
    }
  };

  return (
    <div className="border-t border-border p-3">
      <Button
        variant="ghost"
        size="sm"
        className="w-full justify-start gap-2"
        disabled={loggingOut}
        onClick={() => void onLogout()}
      >
        <LogOut className="size-4" />
        {loggingOut ? "正在退出…" : "退出登录"}
      </Button>
      {error && (
        <p role="alert" className="mt-1 px-2 text-xs text-destructive">
          {error}
        </p>
      )}
    </div>
  );
}

export default function AppLayout() {
  const [dark, setDark] = useState(false);
  const catalogQuery = useUiCatalog();
  const catalog = catalogQuery.data;

  useEffect(() => {
    document.documentElement.classList.toggle("dark", dark);
  }, [dark]);

  const pages = buildNavigationPages(catalog);
  const groups = groupNavigationPages(pages, catalog);

  return (
    <div className="flex min-h-svh bg-background text-foreground">
      <aside className="flex w-60 shrink-0 flex-col border-r border-border">
        <div className="flex items-center gap-2 border-b border-border px-4 py-3">
          <span className="text-sm font-semibold tracking-tight">
            YANG System 控制台
          </span>
        </div>
        <nav className="flex-1 space-y-4 overflow-y-auto p-3">
          {groups.map((group) => (
            <div key={group.identity}>
              <p className="px-2 pb-1 text-xs font-medium text-muted-foreground">
                {group.title}
              </p>
              <ul className="space-y-0.5">
                {group.pages.map((page) => (
                  <li key={page.id}>
                    <NavLink
                      to={`/m/${page.id}`}
                      className={({ isActive }) =>
                        cn(
                          "flex items-center gap-2 rounded-md px-2 py-1.5 text-sm transition-colors hover:bg-accent hover:text-accent-foreground",
                          isActive &&
                            "bg-accent font-medium text-accent-foreground",
                        )
                      }
                    >
                      <ModuleIcon token={page.icon} />
                      {page.title}
                    </NavLink>
                  </li>
                ))}
              </ul>
            </div>
          ))}
          {catalog && pages.length === 0 && (
            <p className="px-2 text-sm text-muted-foreground">暂无可用模块</p>
          )}
        </nav>
        <SidebarSessionFooter />
      </aside>

      <div className="flex min-w-0 flex-1 flex-col">
        <header className="flex items-center justify-end gap-2 border-b border-border px-4 py-2">
          <Button
            variant="outline"
            size="icon"
            aria-label="切换明暗主题"
            onClick={() => setDark((prev) => !prev)}
          >
            {dark ? <Sun /> : <Moon />}
          </Button>
        </header>
        <main className="min-w-0 flex-1 overflow-y-auto">
          {catalogQuery.isPending ? (
            <div className="space-y-3 p-8" aria-label="目录加载中">
              <Skeleton className="h-8 w-64" />
              <Skeleton className="h-4 w-96" />
              <Skeleton className="h-64 w-full" />
            </div>
          ) : catalogQuery.isError || !catalog ? (
            <div className="p-8">
              <div className="rounded-lg border border-destructive/40 bg-destructive/10 p-4">
                <h2 className="font-medium text-destructive">目录加载失败</h2>
                <p className="mt-1 text-sm text-muted-foreground">
                  {catalogQuery.error instanceof Error
                    ? catalogQuery.error.message
                    : "未知错误"}
                </p>
                <Button
                  variant="outline"
                  className="mt-3"
                  onClick={() => void catalogQuery.refetch()}
                >
                  重试
                </Button>
              </div>
            </div>
          ) : (
            <Outlet context={{ catalog } satisfies ShellContext} />
          )}
        </main>
      </div>
    </div>
  );
}

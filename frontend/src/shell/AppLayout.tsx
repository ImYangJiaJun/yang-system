import { useEffect, useState, type ComponentType } from "react";
import {
  Building2,
  Check,
  ChevronsUpDown,
  CircleUser,
  LogOut,
  Moon,
  Puzzle,
  ShieldCheck,
  Sun,
  Table2,
  Users,
} from "lucide-react";
import { NavLink, Outlet, useNavigate } from "react-router";

import { useUiCatalog } from "@/engine/catalog/use-catalog";
import { useSessionController } from "@/engine/session/use-session";
import {
  applyDensity,
  DENSITY_OPTIONS,
  loadDensity,
  persistDensity,
  type Density,
} from "@/shell/density";
import { useIdentity } from "@/features/auth/use-identity";
import {
  buildNavigationPages,
  groupNavigationPages,
  WORKSPACE_IDENTITY,
} from "@/shell/navigation";
import { Button } from "@/shared/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/shared/ui/dropdown-menu";
import { Skeleton } from "@/shared/ui/skeleton";
import {
  buildAccountModulePages,
  visibleAccountIdentities,
} from "@/engine/catalog/module-pages";
import type { UiCatalog } from "@/engine/contracts/ui-catalog";
import { cn } from "@/shared/lib/utils";

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

/// 身份切换器（旧 AccountSwitcher.vue 语义）：当前身份 + 切换列表。
function AccountSwitcher({ catalog }: { catalog: UiCatalog | undefined }) {
  const { identity, select } = useIdentity();
  const navigate = useNavigate();
  const modulePages = buildAccountModulePages(catalog);
  const identities = visibleAccountIdentities(modulePages, catalog);
  const active = identities.find((candidate) => candidate.id === identity);

  const switchIdentity = (next: string) => {
    const first = modulePages.find((module) => module.identity === next);
    if (!first) return;
    select(next);
    navigate(`/m/${first.id}`);
  };

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-sm hover:bg-accent"
          aria-label="账号菜单"
        >
          <span className="flex size-6 items-center justify-center rounded-full bg-primary text-xs font-bold text-primary-foreground">
            Y
          </span>
          <span className="min-w-0 flex-1 truncate text-left">
            {active?.title ?? "未选择角色"}
          </span>
          <ChevronsUpDown className="size-3.5 text-muted-foreground" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-56">
        <DropdownMenuLabel>切换角色</DropdownMenuLabel>
        {identities.map((candidate) => (
          <DropdownMenuItem
            key={candidate.id}
            onClick={() => switchIdentity(candidate.id)}
          >
            <span className="flex-1">{candidate.title}</span>
            {candidate.id === identity && <Check className="size-4" />}
          </DropdownMenuItem>
        ))}
        {identities.length > 1 && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuItem onClick={() => navigate("/select-identity")}>
              查看全部角色
            </DropdownMenuItem>
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/// 密度三档切换（ADR-5 §2.1）：localStorage 持久化 + 文档根 data-density。
function DensityMenu() {
  const [density, setDensity] = useState<Density>(() => loadDensity());
  const apply = (next: Density) => {
    setDensity(next);
    persistDensity(next);
    applyDensity(next);
  };
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="outline" size="sm" aria-label="密度设置">
          密度
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuLabel>表格密度</DropdownMenuLabel>
        {DENSITY_OPTIONS.map((option) => (
          <DropdownMenuItem
            key={option.value}
            onClick={() => apply(option.value)}
          >
            <span className="flex-1">{option.label}</span>
            {density === option.value && <Check className="size-4" />}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
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
  const { identity } = useIdentity();

  useEffect(() => {
    document.documentElement.classList.toggle("dark", dark);
  }, [dark]);

  // 身份过滤：工作台兜底视图（未分配视图）不受身份约束，始终可见。
  const pages = buildNavigationPages(catalog).filter(
    (page) =>
      page.identity === WORKSPACE_IDENTITY ||
      !identity ||
      page.identity === identity,
  );
  const groups = groupNavigationPages(pages, catalog);

  return (
    <div className="flex min-h-svh bg-background text-foreground">
      <aside className="flex w-60 shrink-0 flex-col border-r border-border">
        <div className="flex items-center gap-2 border-b border-border px-4 py-3">
          <span className="text-sm font-semibold tracking-tight">
            YANG System 控制台
          </span>
        </div>
        <div className="border-b border-border p-2">
          <AccountSwitcher catalog={catalog} />
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
          <DensityMenu />
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

import { useState } from "react";
import { ArrowRight, LogOut, UserRoundX } from "lucide-react";
import { useNavigate } from "react-router";

import { useSessionController } from "@/api/use-session";
import { useUiCatalog } from "@/api/use-catalog";
import { useIdentity } from "@/app/use-identity";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import {
  buildAccountModulePages,
  visibleAccountIdentities,
} from "@/catalog/module-pages";

/// 工作身份选择页（旧 RoleSelectionPage.vue 语义）：多身份账号登录后的入口。
export default function SelectIdentityPage() {
  const controller = useSessionController();
  const navigate = useNavigate();
  const { select } = useIdentity();
  const catalogQuery = useUiCatalog();
  const [loggingOut, setLoggingOut] = useState(false);

  const catalog = catalogQuery.data;
  const modulePages = buildAccountModulePages(catalog);
  const identities = visibleAccountIdentities(modulePages, catalog);

  const selectIdentity = (identity: string) => {
    const first = modulePages.find((module) => module.identity === identity);
    if (!first) return;
    select(identity);
    navigate(`/m/${first.id}`, { replace: true });
  };

  const logout = async () => {
    if (loggingOut) return;
    setLoggingOut(true);
    try {
      await controller.endSession();
    } finally {
      setLoggingOut(false);
    }
  };

  return (
    <main className="min-h-svh bg-background text-foreground">
      <header className="flex items-center justify-between border-b border-border px-6 py-3">
        <span className="flex items-center gap-2 text-sm font-semibold">
          <span className="flex size-8 items-center justify-center rounded-lg bg-primary text-sm font-bold text-primary-foreground">
            Y
          </span>
          YANG System
        </span>
        <Button
          variant="ghost"
          size="sm"
          disabled={loggingOut}
          onClick={() => void logout()}
        >
          <LogOut className="size-4" />
          退出全部设备
        </Button>
      </header>

      <section className="mx-auto max-w-3xl px-6 py-12">
        <div className="mb-8 space-y-2">
          <p className="text-sm text-muted-foreground">工作身份</p>
          <h1 className="text-2xl font-bold tracking-tight">
            选择本次使用的角色
          </h1>
          <p className="text-sm text-muted-foreground">
            角色决定本次会话可进入的业务模块，之后仍可从账号菜单切换。
          </p>
        </div>

        {catalogQuery.isPending ? (
          <div className="grid gap-4 sm:grid-cols-2" aria-label="角色加载中">
            <Skeleton className="h-36" />
            <Skeleton className="h-36" />
          </div>
        ) : catalogQuery.isError ? (
          <div className="rounded-lg border border-destructive/40 bg-destructive/10 p-4">
            <p className="font-medium text-destructive">角色目录加载失败</p>
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
        ) : identities.length > 0 ? (
          <div className="grid gap-4 sm:grid-cols-2">
            {identities.map((identity) => (
              <div
                key={identity.id}
                className="rounded-lg border border-border bg-card p-5"
                data-testid={`identity-option-${identity.id}`}
              >
                <h2 className="text-lg font-semibold">{identity.title}</h2>
                <p className="mt-1 text-sm text-muted-foreground">
                  可使用{" "}
                  {
                    modulePages.filter(
                      (module) => module.identity === identity.id,
                    ).length
                  }{" "}
                  个模块
                </p>
                <Button
                  className="mt-4 w-full"
                  aria-label={`选择${identity.title}角色`}
                  onClick={() => selectIdentity(identity.id)}
                >
                  以{identity.title}进入
                  <ArrowRight className="size-4" />
                </Button>
              </div>
            ))}
          </div>
        ) : (
          <div className="flex flex-col items-center gap-3 py-12 text-center">
            <UserRoundX className="size-10 text-muted-foreground" />
            <p className="font-medium">当前账号没有可用角色</p>
            <p className="text-sm text-muted-foreground">
              请联系管理员配置角色与模块权限。
            </p>
          </div>
        )}
      </section>
    </main>
  );
}

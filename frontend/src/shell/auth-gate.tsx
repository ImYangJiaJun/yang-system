import type { ReactNode } from "react";
import { Navigate, useSearchParams } from "react-router";

import { safeRedirectTarget } from "@/features/auth/redirect";
import { useRestoredSession } from "@/engine/session/use-session";
import { Skeleton } from "@/shared/ui/skeleton";

/// 会话恢复进行中的全屏骨架（restoreState === "pending"）。
export function SessionPendingScreen() {
  return (
    <div
      className="flex min-h-svh flex-col items-center justify-center gap-4 bg-background"
      aria-label="会话恢复中"
    >
      <Skeleton className="h-10 w-40" />
      <Skeleton className="h-4 w-64" />
      <p className="text-sm text-muted-foreground">正在恢复会话…</p>
    </div>
  );
}

/// 受保护区域门控：pending → 骨架；anonymous → /login；authenticated → children。
export function RequireAuth({ children }: { children: ReactNode }) {
  const snapshot = useRestoredSession();
  if (snapshot.restoreState === "pending") return <SessionPendingScreen />;
  if (!snapshot.loggedIn) return <Navigate to="/login" replace />;
  return children;
}

/// 登录页门控：已认证访问登录页重定向回首页。
/// 带 `?redirect=` 时回跳原目标——登录提交那一跳与这里的重定向会同时发生，
/// 两边都必须认这个参数，否则本门控的 `<Navigate to="/">` 会把回跳覆盖掉。
export function RedirectIfAuthed({ children }: { children: ReactNode }) {
  const snapshot = useRestoredSession();
  const [searchParams] = useSearchParams();
  if (snapshot.restoreState === "pending") return <SessionPendingScreen />;
  if (snapshot.loggedIn) {
    return (
      <Navigate to={safeRedirectTarget(searchParams.get("redirect"))} replace />
    );
  }
  return children;
}

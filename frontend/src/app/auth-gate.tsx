import type { ReactNode } from "react";
import { Navigate } from "react-router";

import { useRestoredSession } from "@/api/use-session";
import { Skeleton } from "@/components/ui/skeleton";

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
export function RedirectIfAuthed({ children }: { children: ReactNode }) {
  const snapshot = useRestoredSession();
  if (snapshot.restoreState === "pending") return <SessionPendingScreen />;
  if (snapshot.loggedIn) return <Navigate to="/" replace />;
  return children;
}

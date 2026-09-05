import { useEffect } from "react";
import { Outlet, useNavigate } from "react-router";

import {
  SESSION_EXPIRED_EVENT,
  SESSION_RELOGIN_REQUIRED_EVENT,
} from "@/engine/session/auth-session";
import { useSessionController } from "@/engine/session/use-session";

/**
 * 会话失效传播桥（能力 11）：
 * - yang:session-expired（刷新被拒/重试仍 401）→ clearSession("session-expired") 并跳 /login
 * - yang:session-relogin-required（凭据变更类 Action 成功）→ clearSession("credentials-changed") 并跳 /login
 * 结束原因经 SessionController 快照传递（而非 URL query），规避与门控 Navigate 的竞态；
 * LoginPage 优先读快照 reason，同时保留 ?reason= 的外部链接兼容。
 */
export default function SessionBridge() {
  const controller = useSessionController();
  const navigate = useNavigate();

  useEffect(() => {
    const onExpired = () => {
      controller.clearSession("session-expired");
      navigate("/login", { replace: true });
    };
    const onReloginRequired = () => {
      controller.clearSession("credentials-changed");
      navigate("/login", { replace: true });
    };
    window.addEventListener(SESSION_EXPIRED_EVENT, onExpired);
    window.addEventListener(SESSION_RELOGIN_REQUIRED_EVENT, onReloginRequired);
    return () => {
      window.removeEventListener(SESSION_EXPIRED_EVENT, onExpired);
      window.removeEventListener(
        SESSION_RELOGIN_REQUIRED_EVENT,
        onReloginRequired,
      );
    };
  }, [controller, navigate]);

  return <Outlet />;
}

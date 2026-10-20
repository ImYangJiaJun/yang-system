import { useEffect } from "react";
import { Outlet, useNavigate } from "react-router";

import {
  SESSION_EXPIRED_EVENT,
  SESSION_REFRESHED_EVENT,
  SESSION_RELOGIN_REQUIRED_EVENT,
} from "@/engine/session/auth-session";
import type { LoginResult } from "@/engine/session/lifecycle";
import { subscribeSessionEnd } from "@/engine/session/session-coordination";
import { useSessionController } from "@/engine/session/use-session";

/**
 * 会话失效传播桥（能力 11）：
 * - yang:session-expired（刷新被拒）→ clearSession("session-expired") 并跳 /login
 * - yang:session-relogin-required（凭据变更类 Action 成功）→ clearSession("credentials-changed") 并跳 /login
 * - yang:session-refreshed（Access Token 自动轮换成功）→ 回写 SessionController 快照，
 *   否则请求 Bearer 会永远停在登录/恢复时的 T0，第二个 TTL 到期即被强制登出。
 * 结束原因经 SessionController 快照传递（而非 URL query），规避与门控 Navigate 的竞态；
 * LoginPage 优先读快照 reason，同时保留 ?reason= 的外部链接兼容。
 */
export default function SessionBridge() {
  const controller = useSessionController();
  const navigate = useNavigate();

  useEffect(() => {
    const onRefreshed = (event: Event) => {
      const tokens = (event as CustomEvent<LoginResult | undefined>).detail;
      if (tokens?.accessToken) controller.acceptRefreshedTokenPair(tokens);
    };
    const onExpired = () => {
      controller.clearSession("session-expired");
      navigate("/login", { replace: true });
    };
    const onReloginRequired = () => {
      controller.clearSession("credentials-changed");
      navigate("/login", { replace: true });
    };
    window.addEventListener(SESSION_REFRESHED_EVENT, onRefreshed);
    window.addEventListener(SESSION_EXPIRED_EVENT, onExpired);
    window.addEventListener(SESSION_RELOGIN_REQUIRED_EVENT, onReloginRequired);
    return () => {
      window.removeEventListener(SESSION_REFRESHED_EVENT, onRefreshed);
      window.removeEventListener(SESSION_EXPIRED_EVENT, onExpired);
      window.removeEventListener(
        SESSION_RELOGIN_REQUIRED_EVENT,
        onReloginRequired,
      );
    };
  }, [controller, navigate]);

  // 跨标签页会话结束广播（登出/停用/改密后其它标签即时收敛）：
  // subscribeSessionEnd 此前仅被定义与单测引用、未挂载到组件树，导致
  // 跨标签收敛退化为被动依赖 authz_version 刷新失败。
  useEffect(() => {
    const unsubscribe = subscribeSessionEnd((reason) => {
      controller.clearSession(
        reason === "credentials-changed"
          ? "credentials-changed"
          : "session-expired",
      );
      navigate("/login", { replace: true });
    });
    return unsubscribe;
  }, [controller, navigate]);

  return <Outlet />;
}

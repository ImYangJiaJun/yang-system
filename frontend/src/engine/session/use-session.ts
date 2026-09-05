import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useSyncExternalStore,
} from "react";

import type { SessionController, SessionSnapshot } from "./session-controller";
import type { SessionContext } from "../http/types";

/**
 * SessionController 的 React 薄订阅层：全仓库唯一允许 import react 的会话协议文件。
 * 所有协议逻辑在 session-controller.ts（纯 TS），这里只做 context 与
 * useSyncExternalStore 桥接。本文件不导出组件，避免破坏 fast refresh 约定。
 */

export const SessionControllerContext = createContext<SessionController | null>(
  null,
);

export function useSessionController(): SessionController {
  const controller = useContext(SessionControllerContext);
  if (!controller) {
    throw new Error("SessionControllerContext.Provider 缺失");
  }
  return controller;
}

export function useSessionSnapshot(): SessionSnapshot {
  const controller = useSessionController();
  return useSyncExternalStore(controller.subscribe, controller.getSnapshot);
}

/// 供 API 客户端使用的会话凭据视图；匿名会话 token 为 undefined。
/// 按 token memo：对象引用稳定，供 effect 依赖使用（不 memo 会导致请求 effect 无限重发）。
export function useSessionCredentials(): SessionContext {
  const snapshot = useSessionSnapshot();
  return useMemo(
    () => ({ token: snapshot.token || undefined }),
    [snapshot.token],
  );
}

/**
 * 应用启动时触发一次 Cookie 会话恢复（并发去重由 SessionController 保证），
 * 返回实时快照；调用方按 restoreState 分支渲染（pending/anonymous/authenticated）。
 */
export function useRestoredSession(): SessionSnapshot {
  const controller = useSessionController();
  const snapshot = useSessionSnapshot();
  useEffect(() => {
    void controller.restoreFromCookie();
  }, [controller]);
  return snapshot;
}

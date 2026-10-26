import {
  disableAccount as requestDisableAccount,
  logout as requestLogout,
  type LoginResult,
} from "./lifecycle";
import {
  clearStoredSession,
  discardLegacyStoredCredentials,
  persistTokenPair,
  requestWithTokenRefresh,
  restoreSessionFromCookie,
} from "./auth-session";
import { publishSessionEnd } from "./session-coordination";
import { withRequestDeadline } from "../http/deadline";

/// 登出/停用的服务端收敛上限：挂起请求不能让本地清理无限等待。
const TERMINATION_TIMEOUT_MS = 10_000;

/**
 * 会话协议控制器：纯 TS、框架无关（禁止 import react/vue）。
 *
 * 收编旧 Pinia `stores/session.ts` 与 `composables/useApplicationSession.ts` 的对外语义：
 * 内存 access token、Cookie 恢复状态机、并发恢复去重、logout/disable 会话变更与
 * 多标签页结束广播。React 侧只通过 useSyncExternalStore 薄壳（api/use-session.ts）订阅。
 */

export type SessionRestoreState =
  "pending" | "authenticated" | "anonymous" | "failed";

/// 会话结束原因：失效传播到登录页时展示对应提示（避免 URL reason 的导航竞态）。
export type SessionEndReason = "session-expired" | "credentials-changed";

export interface SessionSnapshot {
  readonly token: string;
  readonly restoreState: SessionRestoreState;
  readonly loggedIn: boolean;
  readonly sessionEndReason?: SessionEndReason;
}

export interface SessionControllerOptions {
  /// 会话建立/清空时级联重置其他 owner（身份、Catalog、导航等），由应用层注入。
  onSessionReset?: () => void;
}

export class SessionController {
  private token = "";
  private restoreState: SessionRestoreState = "pending";
  private activeRestore: Promise<boolean> | undefined;
  private generation = 0;
  private snapshot: SessionSnapshot = {
    token: "",
    restoreState: "pending",
    loggedIn: false,
  };
  private readonly listeners = new Set<() => void>();

  constructor(private readonly options: SessionControllerOptions = {}) {
    discardLegacyStoredCredentials();
  }

  /// 箭头函数属性保证 subscribe/getSnapshot 引用稳定，可直接供 useSyncExternalStore。
  readonly getSnapshot = (): SessionSnapshot => this.snapshot;

  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  /// 登录建立会话：先级联清空旧 owner，再写入新凭据。
  beginSession(tokens: LoginResult): void {
    this.generation += 1;
    this.activeRestore = undefined;
    this.options.onSessionReset?.();
    this.setTokenPair(tokens, true);
  }

  /// 自动刷新轮换：只换 token，保留当前身份上下文，不触发 owner 级联。
  acceptRefreshedTokenPair(tokens: LoginResult): void {
    this.setTokenPair(tokens);
  }

  clearSession(reason?: SessionEndReason): void {
    this.generation += 1;
    this.activeRestore = undefined;
    clearStoredSession();
    this.options.onSessionReset?.();
    this.commit("", "anonymous", reason);
  }

  async restoreFromCookie(): Promise<boolean> {
    if (this.snapshot.loggedIn) return true;
    if (this.restoreState === "anonymous") return false;
    if (!this.activeRestore) {
      const generation = this.generation;
      this.commit("", "pending");
      const pending = restoreSessionFromCookie()
        .then((tokens) => {
          if (generation !== this.generation) return false;
          if (!tokens) {
            this.commit("", "anonymous");
            return false;
          }
          this.setTokenPair(tokens);
          return true;
        })
        .catch(() => {
          if (generation === this.generation) this.commit("", "failed");
          return false;
        })
        .finally(() => {
          if (this.activeRestore === pending) this.activeRestore = undefined;
        });
      this.activeRestore = pending;
    }
    return this.activeRestore;
  }

  async endSession(): Promise<boolean> {
    const generation = this.generation;
    try {
      await withRequestDeadline(
        (signal) =>
          requestWithTokenRefresh(
            this.token || undefined,
            (token) => requestLogout(token, signal),
            signal,
          ),
        undefined,
        TERMINATION_TIMEOUT_MS,
      );
    } finally {
      // 服务端不可达或 Access Token 已过期也必须清本地：否则用户被卡在
      // 「已登出但界面仍是登录态」的半状态。10s 超时保证挂起请求不会让
      // finally 永不执行；保留已有结束原因，避免覆盖 session-expired。
      if (generation === this.generation) {
        this.clearSession(this.snapshot.sessionEndReason);
        publishSessionEnd("logout");
      }
    }
    return true;
  }

  async disableAccount(): Promise<boolean> {
    const generation = this.generation;
    try {
      await withRequestDeadline(
        (signal) =>
          requestWithTokenRefresh(
            this.token || undefined,
            (token) => requestDisableAccount(token, signal),
            signal,
          ),
        undefined,
        TERMINATION_TIMEOUT_MS,
      );
    } finally {
      if (generation === this.generation) {
        this.clearSession(this.snapshot.sessionEndReason);
        publishSessionEnd("logout");
      }
    }
    return true;
  }

  private setTokenPair(tokens: LoginResult, newSession = false): void {
    persistTokenPair(tokens, newSession);
    this.commit(tokens.accessToken, "authenticated");
  }

  private commit(
    token: string,
    restoreState: SessionRestoreState,
    sessionEndReason?: SessionEndReason,
  ): void {
    this.token = token;
    this.restoreState = restoreState;
    this.snapshot = {
      token,
      restoreState,
      loggedIn: Boolean(token.trim()),
      sessionEndReason,
    };
    for (const listener of this.listeners) listener();
  }
}

export function createSessionController(
  options: SessionControllerOptions = {},
): SessionController {
  return new SessionController(options);
}

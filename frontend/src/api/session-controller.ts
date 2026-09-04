import {
  disableAccount as requestDisableAccount,
  logout as requestLogout,
  type LoginResult,
} from "./auth";
import {
  clearStoredSession,
  discardLegacyStoredCredentials,
  persistTokenPair,
  restoreSessionFromCookie,
} from "./auth-session";
import { StepUpRequiredError } from "./errors";
import { publishSessionEnd } from "./session-coordination";
import type { SessionContext } from "./types";

/**
 * 会话协议控制器：纯 TS、框架无关（禁止 import react/vue）。
 *
 * 收编旧 Pinia `stores/session.ts` 与 `composables/useApplicationSession.ts` 的对外语义：
 * 内存 access token、Cookie 恢复状态机、并发恢复去重、logout/disable 的 Step-up 保护与
 * 多标签页结束广播。React 侧只通过 useSyncExternalStore 薄壳（api/use-session.ts）订阅。
 */

export type SessionRestoreState = "pending" | "authenticated" | "anonymous";

/// 会话结束原因：失效传播到登录页时展示对应提示（避免 URL reason 的导航竞态）。
export type SessionEndReason = "session-expired" | "credentials-changed";

export interface SessionSnapshot {
  readonly token: string;
  readonly restoreState: SessionRestoreState;
  readonly loggedIn: boolean;
  readonly sessionEndReason?: SessionEndReason;
}

/// Step-up proof 获取是 UI 交互（旧实现为 Quasar Dialog），以回调注入保持 core 纯净；
/// 返回 undefined 表示用户取消。
export type StepUpProofRequest = (
  challenge: string,
  context: SessionContext,
) => Promise<string | undefined>;

export interface SessionControllerOptions {
  requestStepUpProof?: StepUpProofRequest;
  /// 会话建立/清空时级联重置其他 owner（身份、Catalog、导航等），由应用层注入。
  onSessionReset?: () => void;
}

type SessionMutation = (
  accessToken: string | undefined,
  signal?: AbortSignal,
  stepUpProof?: string,
) => Promise<unknown>;

export class SessionController {
  private token = "";
  private restoreState: SessionRestoreState = "pending";
  private activeRestore: Promise<boolean> | undefined;
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
    this.options.onSessionReset?.();
    this.setTokenPair(tokens);
  }

  /// 自动刷新轮换：只换 token，保留当前身份上下文，不触发 owner 级联。
  acceptRefreshedTokenPair(tokens: LoginResult): void {
    this.setTokenPair(tokens);
  }

  clearSession(reason?: SessionEndReason): void {
    clearStoredSession();
    this.options.onSessionReset?.();
    this.commit("", "anonymous", reason);
  }

  async restoreFromCookie(): Promise<boolean> {
    if (this.snapshot.loggedIn) return true;
    if (this.restoreState === "anonymous") return false;
    if (!this.activeRestore) {
      this.activeRestore = restoreSessionFromCookie()
        .then((tokens) => {
          if (!tokens) {
            this.commit("", "anonymous");
            return false;
          }
          this.setTokenPair(tokens);
          return true;
        })
        .catch(() => {
          this.commit("", "anonymous");
          return false;
        })
        .finally(() => {
          this.activeRestore = undefined;
        });
    }
    return this.activeRestore;
  }

  /// 供 Action 执行层在 428 challenge 时索取 proof；未配置回调时 fail-loud。
  async requestStepUpProof(challenge: string): Promise<string | undefined> {
    if (!this.options.requestStepUpProof) {
      throw new Error("未配置 Step-up 交互回调（requestStepUpProof）");
    }
    return this.options.requestStepUpProof(challenge, {
      token: this.token || undefined,
    });
  }

  async endSession(): Promise<boolean> {
    const completed = await this.runStepUpProtectedMutation(requestLogout);
    if (!completed) return false;
    this.clearSession();
    publishSessionEnd("logout");
    return true;
  }

  async disableAccount(): Promise<boolean> {
    const completed = await this.runStepUpProtectedMutation(
      requestDisableAccount,
    );
    if (!completed) return false;
    this.clearSession();
    publishSessionEnd("logout");
    return true;
  }

  private async runStepUpProtectedMutation(
    request: SessionMutation,
  ): Promise<boolean> {
    try {
      await request(this.token || undefined);
    } catch (error: unknown) {
      if (!(error instanceof StepUpRequiredError)) throw error;
      // 未注入 Step-up 交互回调时 fail-loud，而不是静默跳过受保护的会话变更。
      if (!this.options.requestStepUpProof) throw error;
      const proof = await this.options.requestStepUpProof(error.challenge, {
        token: this.token || undefined,
      });
      if (!proof) return false;
      await request(this.token || undefined, undefined, proof);
    }
    return true;
  }

  private setTokenPair(tokens: LoginResult): void {
    persistTokenPair(tokens);
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

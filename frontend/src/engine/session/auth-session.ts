import { refreshSession, type LoginResult } from "./lifecycle";
import { withRefreshLock } from "./refresh-lock";
import { ApiError } from "../http/errors";

export const SESSION_EXPIRED_EVENT = "yang:session-expired";
export const SESSION_REFRESHED_EVENT = "yang:session-refreshed";
export const SESSION_RELOGIN_REQUIRED_EVENT = "yang:session-relogin-required";

const SESSION_KEYS = [
  "yang.token",
  "yang.refresh-token",
  "yang.account-identity",
] as const;
const CREDENTIAL_KEYS = ["yang.token", "yang.refresh-token"] as const;

let activeRefresh: Promise<LoginResult> | undefined;
let currentAccessToken: string | undefined;
let expiredAccessToken: string | undefined;

/// 刷新请求的最长等待时间。超过即中止并按可重试错误交给调用方，
/// 避免刷新端点挂起时启动骨架永久停在 pending。
const REFRESH_TIMEOUT_MS = 10_000;

export class SessionExpiredError extends Error {
  constructor(options?: { cause?: unknown }) {
    super("登录状态已过期，请重新登录", options);
    this.name = "SessionExpiredError";
  }
}

export function activeAccessToken(): string | undefined {
  return currentAccessToken;
}

function storage(): Storage | undefined {
  return typeof sessionStorage === "undefined" ? undefined : sessionStorage;
}

function dispatchSessionEvent(name: string, detail?: LoginResult) {
  if (typeof window === "undefined") return;
  window.dispatchEvent(new CustomEvent(name, { detail }));
}

export function discardLegacyStoredCredentials() {
  const target = storage();
  for (const key of CREDENTIAL_KEYS) target?.removeItem(key);
}

export function persistTokenPair(tokens: LoginResult) {
  currentAccessToken = tokens.accessToken;
  discardLegacyStoredCredentials();
  expiredAccessToken = undefined;
}

export function clearStoredSession() {
  currentAccessToken = undefined;
  const target = storage();
  for (const key of SESSION_KEYS) target?.removeItem(key);
}

export function requireCredentialRelogin() {
  clearStoredSession();
  dispatchSessionEvent(SESSION_RELOGIN_REQUIRED_EVENT);
}

function expireSession(accessToken: string, cause?: unknown): never {
  if (expiredAccessToken !== accessToken) {
    expiredAccessToken = accessToken;
    clearStoredSession();
    dispatchSessionEvent(SESSION_EXPIRED_EVENT);
  }
  throw new SessionExpiredError({ cause });
}

function terminalRefreshFailure(cause: unknown): boolean {
  if (!(cause instanceof ApiError)) return false;
  return (
    (cause.status >= 200 && cause.status < 300) ||
    cause.status === 400 ||
    cause.status === 401 ||
    cause.status === 403 ||
    cause.status === 422
  );
}

async function refreshTokenPair(): Promise<LoginResult> {
  if (!activeRefresh) {
    activeRefresh = withRefreshLock(() =>
      refreshSession(
        typeof AbortSignal !== "undefined" &&
          typeof AbortSignal.timeout === "function"
          ? AbortSignal.timeout(REFRESH_TIMEOUT_MS)
          : undefined,
      ),
    )
      .then((tokens) => {
        persistTokenPair(tokens);
        dispatchSessionEvent(SESSION_REFRESHED_EVENT, tokens);
        return tokens;
      })
      .finally(() => {
        activeRefresh = undefined;
      });
  }
  return activeRefresh;
}

export async function restoreSessionFromCookie(): Promise<
  LoginResult | undefined
> {
  if (currentAccessToken) return { accessToken: currentAccessToken };
  try {
    return await refreshTokenPair();
  } catch (cause: unknown) {
    if (terminalRefreshFailure(cause)) {
      clearStoredSession();
      return undefined;
    }
    throw cause;
  }
}

async function refreshAccessToken(failedAccessToken: string): Promise<string> {
  if (currentAccessToken && currentAccessToken !== failedAccessToken) {
    return currentAccessToken;
  }
  try {
    return (await refreshTokenPair()).accessToken;
  } catch (cause: unknown) {
    if (terminalRefreshFailure(cause)) {
      expireSession(failedAccessToken, cause);
    }
    throw cause;
  }
}

export async function requestWithTokenRefresh<T>(
  accessToken: string | undefined,
  request: (accessToken: string | undefined) => Promise<T>,
): Promise<T> {
  const normalizedAccessToken = accessToken?.trim() || undefined;
  if (!normalizedAccessToken) return request(undefined);

  let result: T;
  try {
    result = await request(normalizedAccessToken);
  } catch (cause) {
    // 生命周期端点（登出/停用）以抛 ApiError 表示 401，与返回 Response 的
    // 请求共用同一套续期逻辑。
    if (!(cause instanceof ApiError) || cause.status !== 401) throw cause;
    return request(await refreshAccessToken(normalizedAccessToken));
  }
  if (!(result instanceof Response) || result.status !== 401) return result;

  // 刷新成功后的重试若仍 401，那是业务拒绝而非会话失效：原样交给调用方，
  // 只有刷新端点本身的失败才允许 expireSession。
  return request(await refreshAccessToken(normalizedAccessToken));
}

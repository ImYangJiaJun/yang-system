import { refreshSession, type LoginResult } from "./lifecycle";
import { withRefreshLock } from "./refresh-lock";
import { ApiError } from "../http/errors";
import { waitForSignal, withRequestDeadline } from "../http/deadline";

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
let sessionGeneration = 0;

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

export function persistTokenPair(tokens: LoginResult, newSession = true) {
  if (newSession) {
    sessionGeneration += 1;
    activeRefresh = undefined;
  }
  currentAccessToken = tokens.accessToken;
  discardLegacyStoredCredentials();
  expiredAccessToken = undefined;
}

export function clearStoredSession() {
  sessionGeneration += 1;
  activeRefresh = undefined;
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
    const generation = sessionGeneration;
    const pending = withRequestDeadline(
      (signal) => withRefreshLock(() => refreshSession(signal), signal),
      undefined,
      REFRESH_TIMEOUT_MS,
    )
      .then((tokens) => {
        assertGeneration(generation);
        persistTokenPair(tokens, false);
        dispatchSessionEvent(SESSION_REFRESHED_EVENT, tokens);
        return tokens;
      })
      .finally(() => {
        if (activeRefresh === pending) activeRefresh = undefined;
      });
    activeRefresh = pending;
  }
  return activeRefresh;
}

export async function restoreSessionFromCookie(): Promise<
  LoginResult | undefined
> {
  if (currentAccessToken) return { accessToken: currentAccessToken };
  const generation = sessionGeneration;
  try {
    return await refreshTokenPair();
  } catch (cause: unknown) {
    assertGeneration(generation);
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
  const generation = sessionGeneration;
  try {
    return (await refreshTokenPair()).accessToken;
  } catch (cause: unknown) {
    assertGeneration(generation);
    if (terminalRefreshFailure(cause)) {
      expireSession(failedAccessToken, cause);
    }
    throw cause;
  }
}

export async function requestWithTokenRefresh<T>(
  accessToken: string | undefined,
  request: (accessToken: string | undefined) => Promise<T>,
  signal?: AbortSignal,
): Promise<T> {
  signal?.throwIfAborted();
  const generation = sessionGeneration;
  const normalizedAccessToken = accessToken?.trim() || undefined;
  if (!normalizedAccessToken) return request(undefined);

  const execute = async (token: string | undefined): Promise<T> => {
    assertGeneration(generation);
    signal?.throwIfAborted();
    const response = await waitForSignal(request(token), signal);
    assertGeneration(generation);
    return response;
  };
  const refresh = () =>
    waitForSignal(refreshAccessToken(normalizedAccessToken), signal);
  let result: T;
  try {
    result = await execute(normalizedAccessToken);
  } catch (cause) {
    assertGeneration(generation);
    if (!(cause instanceof ApiError) || cause.status !== 401) throw cause;
    return execute(await refresh());
  }
  if (!(result instanceof Response) || result.status !== 401) return result;

  // 只重放一次鉴权前被拒的请求；第二次业务401不能清会话。
  return execute(await refresh());
}

function assertGeneration(generation: number): void {
  if (generation !== sessionGeneration) {
    throw new DOMException("会话已变更，请重新发起操作", "AbortError");
  }
}

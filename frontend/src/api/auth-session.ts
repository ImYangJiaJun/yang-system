import { refreshSession, type LoginResult } from "./auth";
import { ApiError } from "./errors";

export const SESSION_EXPIRED_EVENT = "yang:session-expired";
export const SESSION_REFRESHED_EVENT = "yang:session-refreshed";
export const SESSION_RELOGIN_REQUIRED_EVENT = "yang:session-relogin-required";

const SESSION_KEYS = [
  "yang.token",
  "yang.refresh-token",
  "yang.account-identity",
] as const;
const CREDENTIAL_KEYS = ["yang.token", "yang.refresh-token"] as const;
const REFRESH_LOCK = "yang.session.refresh";

let activeRefresh: Promise<LoginResult> | undefined;
let currentAccessToken: string | undefined;
let expiredAccessToken: string | undefined;

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

async function withRefreshLock<T>(task: () => Promise<T>): Promise<T> {
  if (typeof navigator === "undefined" || !navigator.locks) return task();
  return navigator.locks.request(REFRESH_LOCK, { mode: "exclusive" }, () =>
    task(),
  );
}

async function refreshTokenPair(): Promise<LoginResult> {
  if (!activeRefresh) {
    activeRefresh = withRefreshLock(refreshSession)
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

export async function requestWithTokenRefresh(
  accessToken: string | undefined,
  request: (accessToken: string | undefined) => Promise<Response>,
): Promise<Response> {
  const normalizedAccessToken = accessToken?.trim() || undefined;
  const response = await request(normalizedAccessToken);
  if (response.status !== 401 || !normalizedAccessToken) return response;

  const renewedAccessToken = await refreshAccessToken(normalizedAccessToken);
  const retried = await request(renewedAccessToken);
  if (retried.status === 401) expireSession(renewedAccessToken);
  return retried;
}

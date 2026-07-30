import { refreshSession, type LoginResult } from "./auth";
import { ApiError } from "./errors";

export const SESSION_EXPIRED_EVENT = "yang:session-expired";
export const SESSION_REFRESHED_EVENT = "yang:session-refreshed";

const ACCESS_TOKEN_KEY = "yang.token";
const SESSION_KEYS = [
  ACCESS_TOKEN_KEY,
  "yang.refresh-token",
  "yang.tenant-id",
  "yang.account-identity",
] as const;

let activeRefresh: Promise<LoginResult> | undefined;
let expiredAccessToken: string | undefined;

export class SessionExpiredError extends Error {
  constructor(options?: { cause?: unknown }) {
    super("登录状态已过期，请重新登录", options);
    this.name = "SessionExpiredError";
  }
}

function storage(): Storage | undefined {
  return typeof sessionStorage === "undefined" ? undefined : sessionStorage;
}

function storedValue(key: string): string {
  return storage()?.getItem(key)?.trim() ?? "";
}

function dispatchSessionEvent(name: string, detail?: LoginResult) {
  if (typeof window === "undefined") return;
  window.dispatchEvent(new CustomEvent(name, { detail }));
}

export function persistTokenPair(tokens: LoginResult) {
  const target = storage();
  target?.setItem(ACCESS_TOKEN_KEY, tokens.accessToken);
  target?.removeItem("yang.refresh-token");
  expiredAccessToken = undefined;
}

export function clearStoredSession() {
  const target = storage();
  for (const key of SESSION_KEYS) target?.removeItem(key);
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

async function refreshAccessToken(failedAccessToken: string): Promise<string> {
  const currentAccessToken = storedValue(ACCESS_TOKEN_KEY);
  if (currentAccessToken && currentAccessToken !== failedAccessToken) {
    return currentAccessToken;
  }
  if (!activeRefresh) {
    activeRefresh = refreshSession()
      .then((tokens) => {
        persistTokenPair(tokens);
        dispatchSessionEvent(SESSION_REFRESHED_EVENT, tokens);
        return tokens;
      })
      .catch((cause: unknown) => {
        if (terminalRefreshFailure(cause)) {
          expireSession(failedAccessToken, cause);
        }
        throw cause;
      })
      .finally(() => {
        activeRefresh = undefined;
      });
  }
  return (await activeRefresh).accessToken;
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

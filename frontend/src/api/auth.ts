import { ApiError } from "./errors";
import { apiBase, parseJson } from "./http";

export type LoginResult = {
  accessToken: string;
  refreshToken: string;
};

type ApiEnvelope = {
  code?: number;
  message?: string;
  data?: unknown;
};

function tokenPair(data: unknown): LoginResult | undefined {
  if (!data || typeof data !== "object") return undefined;
  const value = data as Record<string, unknown>;
  if (
    typeof value.access_token !== "string" ||
    !value.access_token ||
    typeof value.refresh_token !== "string" ||
    !value.refresh_token
  )
    return undefined;
  return {
    accessToken: value.access_token,
    refreshToken: value.refresh_token,
  };
}

async function requestTokenPair(
  path: string,
  body: Record<string, string>,
  missingTokenMessage: string,
  signal?: AbortSignal,
): Promise<LoginResult> {
  const response = await fetch(`${apiBase}${path}`, {
    method: "POST",
    headers: {
      Accept: "application/json",
      "Content-Type": "application/json",
    },
    body: JSON.stringify(body),
    signal,
  });
  const requestId = response.headers.get("x-request-id") ?? undefined;
  const payload = (await parseJson(response)) as ApiEnvelope | undefined;
  if (!response.ok || payload?.code !== 0) {
    throw new ApiError(payload?.message ?? `HTTP ${response.status}`, {
      status: response.status,
      code: payload?.code,
      requestId,
      details: payload,
    });
  }
  const result = tokenPair(payload.data);
  if (!result) {
    throw new ApiError(missingTokenMessage, {
      status: response.status,
      code: payload.code,
      requestId,
      details: payload,
    });
  }
  return result;
}

export async function login(
  username: string,
  password: string,
  signal?: AbortSignal,
): Promise<LoginResult> {
  return requestTokenPair(
    "/api/v1/users/login",
    { username, password },
    "登录响应缺少有效 Token",
    signal,
  );
}

export async function refreshSession(
  refreshToken: string,
  signal?: AbortSignal,
): Promise<LoginResult> {
  return requestTokenPair(
    "/api/v1/users/refresh",
    { refresh_token: refreshToken },
    "刷新响应缺少有效 Token",
    signal,
  );
}

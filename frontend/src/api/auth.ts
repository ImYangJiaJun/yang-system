import { ApiError } from "./errors";
import { apiBase, parseJson } from "./http";

export type LoginResult = {
  accessToken: string;
};

type ApiEnvelope = {
  code?: number;
  message?: string;
  data?: unknown;
};

function accessToken(data: unknown): LoginResult | undefined {
  if (!data || typeof data !== "object") return undefined;
  const value = data as Record<string, unknown>;
  if (typeof value.access_token !== "string" || !value.access_token)
    return undefined;
  return {
    accessToken: value.access_token,
  };
}

async function requestAccessToken(
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
    credentials: "include",
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
  const result = accessToken(payload.data);
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
  return requestAccessToken(
    "/api/v1/users/login",
    { username, password },
    "登录响应缺少有效 Token",
    signal,
  );
}

export async function refreshSession(
  signal?: AbortSignal,
): Promise<LoginResult> {
  return requestAccessToken(
    "/api/v1/users/refresh",
    {},
    "刷新响应缺少有效 Token",
    signal,
  );
}

export async function logout(
  accessToken: string | undefined,
  signal?: AbortSignal,
): Promise<void> {
  const response = await fetch(`${apiBase}/api/v1/users/logout`, {
    method: "POST",
    headers: {
      Accept: "application/json",
      "Content-Type": "application/json",
      ...(accessToken ? { Authorization: `Bearer ${accessToken}` } : {}),
    },
    body: "{}",
    credentials: "include",
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
}

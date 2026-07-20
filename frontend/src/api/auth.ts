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

export async function login(
  username: string,
  password: string,
  signal?: AbortSignal,
): Promise<LoginResult> {
  const response = await fetch(`${apiBase}/api/v1/users/login`, {
    method: "POST",
    headers: {
      Accept: "application/json",
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ username, password }),
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
    throw new ApiError("登录响应缺少有效 Token", {
      status: response.status,
      code: payload.code,
      requestId,
      details: payload,
    });
  }
  return result;
}

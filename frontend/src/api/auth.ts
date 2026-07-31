import { ApiError } from "./errors";
import { apiBase, parseJson } from "./http";
import { stepUpRequiredError } from "./step-up-response";

export type LoginResult = {
  accessToken: string;
};

export type LogoutResult = {
  immediateConvergence: boolean;
};

export type DisableAccountResult = {
  immediateConvergence: boolean;
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
  stepUpProof?: string,
): Promise<LogoutResult> {
  return requestAccountTermination(
    "/api/v1/users/logout",
    "revoked_all_sessions",
    accessToken,
    signal,
    stepUpProof,
  );
}

export async function disableAccount(
  accessToken: string | undefined,
  signal?: AbortSignal,
  stepUpProof?: string,
): Promise<DisableAccountResult> {
  return requestAccountTermination(
    "/api/v1/users/disable",
    "account_disabled",
    accessToken,
    signal,
    stepUpProof,
  );
}

async function requestAccountTermination(
  path: string,
  confirmationField: "revoked_all_sessions" | "account_disabled",
  accessToken: string | undefined,
  signal?: AbortSignal,
  stepUpProof?: string,
): Promise<{ immediateConvergence: boolean }> {
  const response = await fetch(`${apiBase}${path}`, {
    method: "POST",
    headers: {
      Accept: "application/json",
      "Content-Type": "application/json",
      ...(accessToken ? { Authorization: `Bearer ${accessToken}` } : {}),
      ...(stepUpProof ? { "x-step-up-proof": stepUpProof } : {}),
    },
    body: "{}",
    credentials: "include",
    signal,
  });
  const requestId = response.headers.get("x-request-id") ?? undefined;
  const payload = (await parseJson(response)) as ApiEnvelope | undefined;
  const stepUpRequired = stepUpRequiredError(response, payload);
  if (stepUpRequired) throw stepUpRequired;
  const data =
    payload?.data !== null &&
    typeof payload?.data === "object" &&
    !Array.isArray(payload.data)
      ? (payload.data as Record<string, unknown>)
      : undefined;
  const validResult =
    data?.[confirmationField] === true &&
    typeof data.immediate_convergence === "boolean" &&
    data.relogin_required === true;
  if (!response.ok || payload?.code !== 0 || !validResult) {
    throw new ApiError(payload?.message ?? `HTTP ${response.status}`, {
      status: response.status,
      code: payload?.code,
      requestId,
      details: payload,
    });
  }
  return { immediateConvergence: data.immediate_convergence as boolean };
}

export async function resetPassword(
  resetToken: string,
  newPassword: string,
  signal?: AbortSignal,
): Promise<void> {
  const response = await fetch(`${apiBase}/api/v1/users/reset-password`, {
    method: "POST",
    headers: {
      Accept: "application/json",
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      reset_token: resetToken,
      new_password: newPassword,
    }),
    credentials: "include",
    signal,
  });
  const requestId = response.headers.get("x-request-id") ?? undefined;
  const payload = (await parseJson(response)) as ApiEnvelope | undefined;
  const reloginRequired =
    payload?.data !== null &&
    typeof payload?.data === "object" &&
    (payload.data as Record<string, unknown>).relogin_required === true;
  if (!response.ok || payload?.code !== 0 || !reloginRequired) {
    const message =
      response.ok && payload?.code === 0 && !reloginRequired
        ? "密码重置响应缺少重新登录确认"
        : (payload?.message ?? `HTTP ${response.status}`);
    throw new ApiError(message, {
      status: response.status,
      code: payload?.code,
      requestId,
      details: payload,
    });
  }
}

import { ApiError } from "@/engine/http/errors";
import { apiBase, parseJson } from "@/engine/http/http";

/**
 * account 域业务流程请求：注册（邮箱验证码）、密码重置。
 * 会话生命周期（login/refresh/logout/disable）属引擎会话协议，见 engine/session/lifecycle.ts。
 */

export type RegistrationEmailChallenge = {
  expiresIn: number;
  resendAfter: number;
};

export type RegisteredUser = {
  id: number;
  username: string;
  email: string;
  emailVerifiedAt: number;
};

type ApiEnvelope = {
  code?: number;
  message?: string;
  data?: unknown;
};

async function requestPublicAction(
  path: string,
  body: Record<string, string>,
  signal?: AbortSignal,
): Promise<{ payload: ApiEnvelope; status: number; requestId?: string }> {
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
  return { payload, status: response.status, requestId };
}

function recordData(data: unknown): Record<string, unknown> | undefined {
  return data !== null && typeof data === "object" && !Array.isArray(data)
    ? (data as Record<string, unknown>)
    : undefined;
}

export async function requestRegistrationEmail(
  email: string,
  signal?: AbortSignal,
): Promise<RegistrationEmailChallenge> {
  const result = await requestPublicAction(
    "/api/v1/users/registration-email-verifications",
    { email },
    signal,
  );
  const { payload } = result;
  const data = recordData(payload.data);
  if (
    data?.accepted !== true ||
    typeof data.expires_in !== "number" ||
    !Number.isSafeInteger(data.expires_in) ||
    data.expires_in <= 0 ||
    typeof data.resend_after !== "number" ||
    !Number.isSafeInteger(data.resend_after) ||
    data.resend_after <= 0
  ) {
    throw new ApiError("验证码响应缺少有效时限", {
      status: result.status,
      code: payload.code,
      requestId: result.requestId,
      details: payload,
    });
  }
  return {
    expiresIn: data.expires_in,
    resendAfter: data.resend_after,
  };
}

export async function register(
  username: string,
  password: string,
  email: string,
  emailCode: string,
  signal?: AbortSignal,
): Promise<RegisteredUser> {
  const result = await requestPublicAction(
    "/api/v1/users/register",
    { username, password, email, email_code: emailCode },
    signal,
  );
  const { payload } = result;
  const data = recordData(payload.data);
  if (
    typeof data?.id !== "number" ||
    !Number.isSafeInteger(data.id) ||
    data.id <= 0 ||
    typeof data.username !== "string" ||
    !data.username ||
    typeof data.email !== "string" ||
    !data.email ||
    typeof data.email_verified_at !== "number" ||
    !Number.isSafeInteger(data.email_verified_at) ||
    data.email_verified_at <= 0
  ) {
    throw new ApiError("注册响应缺少已验证账户", {
      status: result.status,
      code: payload.code,
      requestId: result.requestId,
      details: payload,
    });
  }
  return {
    id: data.id,
    username: data.username,
    email: data.email,
    emailVerifiedAt: data.email_verified_at,
  };
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

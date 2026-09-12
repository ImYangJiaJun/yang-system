import { useQuery, type UseQueryResult } from "@tanstack/react-query";

import { ApiError } from "@/engine/http/errors";
import { apiBase, parseJson } from "@/engine/http/http";
import { stepUpRequiredError } from "@/engine/session/step-up-response";
import { useSessionCredentials, useSessionSnapshot } from "@/engine";

/**
 * account 账号中心业务流程请求：当前用户资料、头像、修改密码、修改用户名、停用账号。
 *
 * 会话生命周期（login/refresh/logout/disable）属引擎会话协议，见 engine/session/lifecycle.ts；
 * 本文件只负责账号中心页面的受保护写操作。修改密码/用户名/停用均要求 Step-up
 * proof：后端返回 428 challenge 时抛 StepUpRequiredError，由页面层经
 * SessionController.requestStepUpProof 弹对话框换 proof 后重放。
 */

export type CurrentUser = {
  id: number;
  username: string;
  email: string | null;
  emailVerifiedAt: number | null;
  status: "active" | "disabled";
  totpActivated: boolean;
  avatarVersion: string | null;
  createdAt: number;
  updatedAt: number;
};

export type CredentialMutationResult = {
  reloginRequired: boolean;
  immediateConvergence: boolean;
};

type ApiEnvelope = {
  code?: number;
  message?: string;
  data?: unknown;
};

function recordData(data: unknown): Record<string, unknown> | undefined {
  return data !== null && typeof data === "object" && !Array.isArray(data)
    ? (data as Record<string, unknown>)
    : undefined;
}

async function postAuthenticated(
  path: string,
  body: unknown,
  accessToken: string | undefined,
  signal?: AbortSignal,
  stepUpProof?: string,
): Promise<{ payload: ApiEnvelope; status: number; requestId?: string }> {
  const response = await fetch(`${apiBase}${path}`, {
    method: "POST",
    headers: {
      Accept: "application/json",
      "Content-Type": "application/json",
      ...(accessToken ? { Authorization: `Bearer ${accessToken}` } : {}),
      ...(stepUpProof ? { "x-step-up-proof": stepUpProof } : {}),
    },
    body: JSON.stringify(body),
    credentials: "include",
    signal,
  });
  const requestId = response.headers.get("x-request-id") ?? undefined;
  const payload = (await parseJson(response)) as ApiEnvelope | undefined;
  if (!response.ok || payload?.code !== 0) {
    // 428 携带 challenge，交由 SessionController 弹 Step-up 对话框。
    const stepUpRequired = stepUpRequiredError(response, payload);
    if (stepUpRequired) throw stepUpRequired;
    throw new ApiError(payload?.message ?? `HTTP ${response.status}`, {
      status: response.status,
      code: payload?.code,
      requestId,
      details: payload,
    });
  }
  return { payload, status: response.status, requestId };
}

export async function fetchCurrentUser(
  accessToken: string | undefined,
  signal?: AbortSignal,
): Promise<CurrentUser> {
  const response = await fetch(`${apiBase}/api/v1/users/me`, {
    method: "GET",
    headers: {
      Accept: "application/json",
      ...(accessToken ? { Authorization: `Bearer ${accessToken}` } : {}),
    },
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
  const data = recordData(payload.data);
  if (
    typeof data?.id !== "number" ||
    typeof data.username !== "string" ||
    !data.username
  ) {
    throw new ApiError("当前用户响应缺少有效资料", {
      status: response.status,
      code: payload?.code,
      requestId,
      details: payload,
    });
  }
  return {
    id: data.id,
    username: data.username,
    email: typeof data.email === "string" ? data.email : null,
    emailVerifiedAt:
      typeof data.email_verified_at === "number"
        ? data.email_verified_at
        : null,
    status: data.status === "disabled" ? "disabled" : "active",
    totpActivated: data.totp_activated === true,
    avatarVersion:
      typeof data.avatar_version === "string" && data.avatar_version
        ? data.avatar_version
        : null,
    createdAt: data.created_at as number,
    updatedAt: data.updated_at as number,
  };
}

/// 当前用户资料的 TanStack Query 入口：认证会话就绪后拉取，token 变化自动重拉。
/// 头像上传等写操作后由调用方 invalidate 该查询驱动各消费方刷新。
export function useMe(): UseQueryResult<CurrentUser> {
  const session = useSessionCredentials();
  const snapshot = useSessionSnapshot();
  return useQuery({
    enabled: snapshot.loggedIn,
    queryKey: ["me", session.token ?? "anonymous"],
    queryFn: ({ signal }) => fetchCurrentUser(session.token, signal),
    staleTime: 30_000,
  });
}

export type AvatarContent = {
  etag: string | null;
  dataUrl: string | null;
};

/// 读取指定用户头像：无头像时 etag/data_url 均为 null；data_url 可直接作为 <img src>。
export async function fetchAvatar(
  userId: number,
  accessToken: string | undefined,
  signal?: AbortSignal,
): Promise<AvatarContent> {
  const response = await fetch(
    `${apiBase}/api/v1/users/avatar?user_id=${userId}`,
    {
      method: "GET",
      headers: {
        Accept: "application/json",
        ...(accessToken ? { Authorization: `Bearer ${accessToken}` } : {}),
      },
      credentials: "include",
      signal,
    },
  );
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
  const data = recordData(payload.data);
  return {
    etag: typeof data?.etag === "string" && data.etag ? data.etag : null,
    dataUrl:
      typeof data?.data_url === "string" && data.data_url
        ? data.data_url
        : null,
  };
}

/// 上传当前用户头像：body 为 base64 内容与 MIME，成功返回新的内容版本（etag）。
export async function uploadAvatar(
  contentBase64: string,
  mime: string,
  accessToken: string | undefined,
  signal?: AbortSignal,
): Promise<{ avatarVersion: string }> {
  const result = await postAuthenticated(
    "/api/v1/users/avatar",
    { content_base64: contentBase64, mime },
    accessToken,
    signal,
  );
  const data = recordData(result.payload.data);
  if (typeof data?.avatar_version !== "string" || !data.avatar_version) {
    throw new ApiError("头像上传响应缺少有效版本", {
      status: result.status,
      code: result.payload.code,
      requestId: result.requestId,
      details: result.payload,
    });
  }
  return { avatarVersion: data.avatar_version };
}

export async function changePassword(
  oldPassword: string,
  newPassword: string,
  accessToken: string | undefined,
  signal?: AbortSignal,
  stepUpProof?: string,
): Promise<CredentialMutationResult> {
  const result = await postAuthenticated(
    "/api/v1/users/change-password",
    { old_password: oldPassword, new_password: newPassword },
    accessToken,
    signal,
    stepUpProof,
  );
  const data = recordData(result.payload.data);
  return {
    reloginRequired: data?.relogin_required === true,
    immediateConvergence: data?.immediate_convergence === true,
  };
}

export async function changeUsername(
  newUsername: string,
  accessToken: string | undefined,
  signal?: AbortSignal,
  stepUpProof?: string,
): Promise<CredentialMutationResult> {
  const result = await postAuthenticated(
    "/api/v1/users/change-username",
    { new_username: newUsername },
    accessToken,
    signal,
    stepUpProof,
  );
  const data = recordData(result.payload.data);
  return {
    reloginRequired: data?.relogin_required === true,
    immediateConvergence: data?.immediate_convergence === true,
  };
}

export async function requestChangeEmail(
  newEmail: string,
  accessToken: string | undefined,
  signal?: AbortSignal,
): Promise<{ expiresIn: number; resendAfter: number }> {
  const result = await postAuthenticated(
    "/api/v1/users/change-email-verifications",
    { new_email: newEmail },
    accessToken,
    signal,
  );
  const data = recordData(result.payload.data);
  if (
    data?.accepted !== true ||
    typeof data.expires_in !== "number" ||
    typeof data.resend_after !== "number"
  ) {
    throw new ApiError("换绑验证码响应缺少有效时限", {
      status: result.status,
      code: result.payload.code,
      requestId: result.requestId,
      details: result.payload,
    });
  }
  return { expiresIn: data.expires_in, resendAfter: data.resend_after };
}

export async function changeEmail(
  newEmail: string,
  emailCode: string,
  accessToken: string | undefined,
  signal?: AbortSignal,
  stepUpProof?: string,
): Promise<CredentialMutationResult> {
  const result = await postAuthenticated(
    "/api/v1/users/change-email",
    { new_email: newEmail, email_code: emailCode },
    accessToken,
    signal,
    stepUpProof,
  );
  const data = recordData(result.payload.data);
  return {
    reloginRequired: data?.relogin_required === true,
    immediateConvergence: data?.immediate_convergence === true,
  };
}

export type SessionInfo = {
  sessionId: string;
  createdAt: number;
  lastSeenAt: number;
  ip: string;
  userAgent: string;
  revokedAt: number | null;
  current: boolean;
};

export async function listSessions(
  accessToken: string | undefined,
  signal?: AbortSignal,
): Promise<SessionInfo[]> {
  const response = await fetch(`${apiBase}/api/v1/users/sessions`, {
    method: "GET",
    headers: {
      Accept: "application/json",
      ...(accessToken ? { Authorization: `Bearer ${accessToken}` } : {}),
    },
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
  const data = recordData(payload.data);
  const sessions = Array.isArray(data?.sessions) ? data.sessions : [];
  return sessions.map((raw) => {
    const session = raw as Record<string, unknown>;
    return {
      sessionId: String(session.session_id),
      createdAt: session.created_at as number,
      lastSeenAt: session.last_seen_at as number,
      ip: String(session.ip),
      userAgent: String(session.user_agent),
      revokedAt:
        typeof session.revoked_at === "number"
          ? (session.revoked_at as number)
          : null,
      current: session.current === true,
    };
  });
}

export async function revokeSession(
  sessionId: string,
  accessToken: string | undefined,
  signal?: AbortSignal,
  stepUpProof?: string,
): Promise<void> {
  const result = await postAuthenticated(
    "/api/v1/users/sessions/revoke",
    { session_id: sessionId },
    accessToken,
    signal,
    stepUpProof,
  );
  const data = recordData(result.payload.data);
  if (data?.session_revoked !== true) {
    throw new ApiError("撤销会话响应缺少确认", {
      status: result.status,
      code: result.payload.code,
      requestId: result.requestId,
      details: result.payload,
    });
  }
}

export type TotpSetupResult = {
  secret: string;
  otpauthUri: string;
  digits: number;
};

/// TOTP 配置初始化：生成共享密钥与 otpauth URI（未激活，需 Step-up）。
export async function setupTotp(
  accessToken: string | undefined,
  signal?: AbortSignal,
  stepUpProof?: string,
): Promise<TotpSetupResult> {
  const result = await postAuthenticated(
    "/api/v1/users/mfa/totp/setup",
    {},
    accessToken,
    signal,
    stepUpProof,
  ).catch((cause: unknown) => {
    // 服务端未配置 [security.totp] 时 MFA Action 不注册（404），转为可操作的提示。
    if (cause instanceof ApiError && cause.status === 404) {
      throw new ApiError(
        "服务端未启用双重验证（缺少 [security.totp] 配置），请联系管理员",
        { status: cause.status, code: cause.code },
      );
    }
    throw cause;
  });
  const data = recordData(result.payload.data);
  if (
    typeof data?.secret !== "string" ||
    !data.secret ||
    typeof data.otpauth_uri !== "string" ||
    data.activated !== false ||
    typeof data.digits !== "number"
  ) {
    throw new ApiError("TOTP 初始化响应缺少有效密钥", {
      status: result.status,
      code: result.payload.code,
      requestId: result.requestId,
      details: result.payload,
    });
  }
  return {
    secret: data.secret,
    otpauthUri: data.otpauth_uri,
    digits: data.digits,
  };
}

export type TotpActivateResult = {
  recoveryCodes: string[];
  immediateConvergence: boolean;
};

/// TOTP 激活：验码启用第二因子并签发一次性恢复码（明文仅此响应回显一次）。
/// 成功后既有会话全部失效，调用方必须引导重新登录。
export async function activateTotp(
  secret: string,
  code: string,
  accessToken: string | undefined,
  signal?: AbortSignal,
  stepUpProof?: string,
): Promise<TotpActivateResult> {
  const result = await postAuthenticated(
    "/api/v1/users/mfa/totp/activate",
    { secret, code },
    accessToken,
    signal,
    stepUpProof,
  );
  const data = recordData(result.payload.data);
  const recoveryCodes = Array.isArray(data?.recovery_codes)
    ? data.recovery_codes.filter(
        (code): code is string => typeof code === "string" && code.length > 0,
      )
    : [];
  if (
    data?.totp_activated !== true ||
    recoveryCodes.length === 0 ||
    data.relogin_required !== true
  ) {
    throw new ApiError("TOTP 激活响应缺少恢复码", {
      status: result.status,
      code: result.payload.code,
      requestId: result.requestId,
      details: result.payload,
    });
  }
  return {
    recoveryCodes,
    immediateConvergence: data.immediate_convergence === true,
  };
}

/// TOTP 停用：关闭第二因子并作废全部恢复码（需登录 + Step-up 重认证）。
/// 成功后既有会话全部失效，调用方必须引导重新登录。
export async function deactivateTotp(
  accessToken: string | undefined,
  signal?: AbortSignal,
  stepUpProof?: string,
): Promise<CredentialMutationResult> {
  const result = await postAuthenticated(
    "/api/v1/users/mfa/totp/deactivate",
    {},
    accessToken,
    signal,
    stepUpProof,
  );
  const data = recordData(result.payload.data);
  if (data?.totp_activated !== false || data.relogin_required !== true) {
    throw new ApiError("TOTP 停用响应缺少确认", {
      status: result.status,
      code: result.payload.code,
      requestId: result.requestId,
      details: result.payload,
    });
  }
  return {
    reloginRequired: true,
    immediateConvergence: data.immediate_convergence === true,
  };
}

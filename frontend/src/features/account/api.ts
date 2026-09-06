import { ApiError } from "@/engine/http/errors";
import { apiBase, parseJson } from "@/engine/http/http";
import { stepUpRequiredError } from "@/engine/session/step-up-response";

/**
 * account 账号中心业务流程请求：当前用户资料、修改密码、修改用户名、停用账号。
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
    createdAt: data.created_at as number,
    updatedAt: data.updated_at as number,
  };
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

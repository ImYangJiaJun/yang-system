import { withRequestDeadline } from "@/engine";
import { requestWithTokenRefresh } from "@/engine";
import { ApiError } from "@/engine/http/errors";
import { apiBase, parseJson } from "@/engine/http/http";

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

async function readEnvelope(response: Response): Promise<ApiEnvelope> {
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
  return payload ?? {};
}

export type AdminUser = {
  id: number;
  username: string;
  email: string | null;
  status: string;
};

export async function listUsers(
  token: string | undefined,
  page: number,
  signal?: AbortSignal,
): Promise<AdminUser[]> {
  return withRequestDeadline(async (signal) => {
    const response = await requestWithTokenRefresh(
      token,
      (current) =>
        fetch(`${apiBase}/api/v1/users?page=${page}&page_size=20`, {
          method: "GET",
          headers: {
            Accept: "application/json",
            ...(current ? { Authorization: `Bearer ${current}` } : {}),
          },
          credentials: "include",
          signal,
        }),
      signal,
    );
    const payload = await readEnvelope(response);
    const users = recordData(payload.data)?.users;
    if (!Array.isArray(users)) {
      throw new ApiError("用户列表响应缺少 users", {
        status: response.status,
        details: payload,
      });
    }
    return users.flatMap((row) => {
      const user = recordData(row);
      if (typeof user?.id !== "number" || typeof user.username !== "string") {
        return [];
      }
      return [
        {
          id: user.id,
          username: user.username,
          email: typeof user.email === "string" ? user.email : null,
          status: typeof user.status === "string" ? user.status : "",
        },
      ];
    });
  }, signal);
}

export async function issuePasswordReset(
  token: string | undefined,
  userId: number,
  signal?: AbortSignal,
): Promise<{ resetToken: string; expiresIn: number }> {
  return withRequestDeadline(async (signal) => {
    const response = await requestWithTokenRefresh(
      token,
      (current) =>
        fetch(`${apiBase}/api/v1/users/${userId}/password-reset-tokens`, {
          method: "POST",
          headers: {
            Accept: "application/json",
            ...(current ? { Authorization: `Bearer ${current}` } : {}),
          },
          credentials: "include",
          signal,
        }),
      signal,
    );
    const payload = await readEnvelope(response);
    const data = recordData(payload.data);
    if (
      typeof data?.reset_token !== "string" ||
      typeof data.expires_in !== "number"
    ) {
      throw new ApiError("重置凭证响应无效", {
        status: response.status,
        details: payload,
      });
    }
    return { resetToken: data.reset_token, expiresIn: data.expires_in };
  }, signal);
}

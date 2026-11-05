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

/// 管理端请求的共同骨架：带截止时间、Token 刷新重试与信封校验。
async function adminRequest<T>(
  token: string | undefined,
  path: string,
  options: {
    method?: "GET" | "POST";
    parse: (data: unknown, response: Response) => T;
    signal?: AbortSignal;
  },
): Promise<T> {
  const { method = "GET", parse, signal } = options;
  return withRequestDeadline(async (abortSignal) => {
    const response = await requestWithTokenRefresh(
      token,
      (current) =>
        fetch(`${apiBase}${path}`, {
          method,
          headers: {
            Accept: "application/json",
            ...(current ? { Authorization: `Bearer ${current}` } : {}),
          },
          credentials: "include",
          signal: abortSignal,
        }),
      abortSignal,
    );
    const payload = await readEnvelope(response);
    return parse(payload.data, response);
  }, signal);
}

async function adminMutation(
  token: string | undefined,
  path: string,
  signal?: AbortSignal,
): Promise<void> {
  await adminRequest(token, path, {
    method: "POST",
    signal,
    parse: () => undefined,
  });
}

export type AdminUser = {
  id: number;
  username: string;
  email: string | null;
  status: string;
};

/// 管理端用户列表。带关键词时走 authenticated-only 的 `/users/lookup`
/// （`list_users` 只有 page/page_size，没有关键词参数）；两者都是同一个 UserView
/// 投影，后者在调用方没有 account.users.read 时把 email 置空。
export async function listUsers(
  token: string | undefined,
  page: number,
  keyword = "",
  signal?: AbortSignal,
): Promise<AdminUser[]> {
  const q = keyword.trim();
  const path = q
    ? `/api/v1/users/lookup?q=${encodeURIComponent(q)}&page=${page}&page_size=20`
    : `/api/v1/users?page=${page}&page_size=20`;
  return adminRequest<AdminUser[]>(token, path, {
    signal,
    parse: (data, response) => {
      const users = recordData(data)?.users;
      if (!Array.isArray(users)) {
        throw new ApiError("用户列表响应缺少 users", {
          status: response.status,
          details: data,
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
    },
  });
}

/// 停用 / 启用账号（管理端，需 account.users.manage）。两个接口都是路径参数 + 空响应体。
export function disableUser(
  token: string | undefined,
  userId: number,
  signal?: AbortSignal,
): Promise<void> {
  return adminMutation(token, `/api/v1/users/${userId}/disable`, signal);
}

export function enableUser(
  token: string | undefined,
  userId: number,
  signal?: AbortSignal,
): Promise<void> {
  return adminMutation(token, `/api/v1/users/${userId}/enable`, signal);
}

export async function issuePasswordReset(
  token: string | undefined,
  userId: number,
  signal?: AbortSignal,
): Promise<{ resetToken: string; expiresIn: number }> {
  return adminRequest(token, `/api/v1/users/${userId}/password-reset-tokens`, {
    method: "POST",
    signal,
    parse: (data, response) => {
      const record = recordData(data);
      if (
        typeof record?.reset_token !== "string" ||
        typeof record.expires_in !== "number"
      ) {
        throw new ApiError("重置凭证响应无效", {
          status: response.status,
          details: data,
        });
      }
      return { resetToken: record.reset_token, expiresIn: record.expires_in };
    },
  });
}

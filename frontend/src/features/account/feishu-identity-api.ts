import {
  ApiError,
  apiBase,
  parseJson,
  requestWithTokenRefresh,
  withRequestDeadline,
} from "@/engine";

/**
 * 账号域的飞书身份绑定 API。
 *
 * 为什么放在 account 域：`AccountSettingsPage` 属于 account，而 features 域之间
 * 禁止互相 import，所以这里不引用 `features/feishu` 的任何代码。端点本身是
 * account/identity 的资源，飞书只是 provider 值。
 *
 * 端点（设计 §5.2）：
 *   GET    /api/v1/auth/feishu/status                      公开，登录/绑定总开关
 *   GET    /api/v1/users/me/identities                     authenticated，绑定列表
 *   POST   /api/v1/users/me/identities/feishu/authorize    authenticated + Step-up
 *   DELETE /api/v1/users/me/identities/feishu              authenticated + Step-up
 */

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

function optionalString(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value : null;
}

function optionalNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/// 飞书登录/绑定是否启用。查询失败由调用方按「不可用」处理。
export async function fetchFeishuBindingAvailability(
  signal?: AbortSignal,
): Promise<{ enabled: boolean }> {
  return withRequestDeadline(async (signal) => {
    const response = await fetch(`${apiBase}/api/v1/auth/feishu/status`, {
      method: "GET",
      headers: { Accept: "application/json" },
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
    return { enabled: recordData(payload.data)?.enabled === true };
  }, signal);
}

export type UserIdentity = {
  id: number | null;
  provider: string;
  externalName: string | null;
  externalEmail: string | null;
  status: string;
  boundAt: number | null;
  lastLoginAt: number | null;
};

/// 当前用户的全部外部身份绑定。
///
/// 后端 `feishu_identities` 返回的是**裸数组**（`data: [{...}]`），字段为
/// `provider/name/email_masked/bound_at/last_login_at`。这里同时兼容
/// `{ identities: [...] }` 与早期的 `external_*` 命名，避免契约微调时整块静默为空。
export async function fetchMyIdentities(
  accessToken: string | undefined,
  signal?: AbortSignal,
): Promise<UserIdentity[]> {
  const data = await requestAuthenticatedJson(
    "/api/v1/users/me/identities",
    { method: "GET" },
    accessToken,
    signal,
  );
  const container = recordData(data);
  const rawIdentities = container?.identities;
  const rows = Array.isArray(data)
    ? data
    : Array.isArray(rawIdentities)
      ? rawIdentities
      : [];
  return rows.map((raw: unknown) => {
    const identity = recordData(raw);
    return {
      id: optionalNumber(identity?.id),
      provider: optionalString(identity?.provider) ?? "",
      externalName:
        optionalString(identity?.name) ??
        optionalString(identity?.external_name),
      externalEmail:
        optionalString(identity?.email_masked) ??
        optionalString(identity?.external_email),
      status: optionalString(identity?.status) ?? "active",
      boundAt: optionalNumber(identity?.bound_at),
      lastLoginAt: optionalNumber(identity?.last_login_at),
    };
  });
}

/// 开始绑定/重新授权：返回飞书授权页 URL，调用方整页跳转。
export async function requestFeishuBindAuthorizeUrl(
  accessToken: string | undefined,
  password: string,
  signal?: AbortSignal,
): Promise<string> {
  const data = await requestAuthenticatedJson(
    "/api/v1/users/me/identities/feishu/authorize",
    { method: "POST", body: { confirm: "true", password } },
    accessToken,
    signal,
  );
  const url = optionalString(recordData(data)?.authorize_url);
  if (!url) {
    throw new ApiError("绑定响应缺少飞书授权地址", { status: 200 });
  }
  return url;
}

/// 解绑当前账号的飞书身份。删除是硬删除，重新绑定需要重新走授权。
export async function unbindFeishuIdentity(
  accessToken: string | undefined,
  password: string,
  signal?: AbortSignal,
): Promise<void> {
  await requestAuthenticatedJson(
    "/api/v1/users/me/identities/feishu",
    { method: "DELETE", body: { confirm: "true", password } },
    accessToken,
    signal,
  );
}

/// 账号域的 Step-up 文案。
///
/// 绑定/解绑要求当前账号密码作为 Step-up；后端校验失败会返回 InvalidPassword。
/// 这里保留 428 的可行动提示，兼容未来框架级二次验证中间件。
export function describeIdentityError(cause: unknown): string {
  if (cause instanceof ApiError) {
    if (cause.status === 428) {
      return "该操作需要二次身份验证。请重新登录后再试；如仍被拒绝，请联系管理员。";
    }
    if (cause.status === 403) {
      return "当前账号没有执行该操作的权限，请联系管理员。";
    }
    return cause.message;
  }
  return cause instanceof Error ? cause.message : "操作失败，请稍后重试";
}

async function requestAuthenticatedJson(
  path: string,
  options: { method: "GET" | "POST" | "DELETE"; body?: unknown },
  accessToken: string | undefined,
  signal?: AbortSignal,
): Promise<unknown> {
  return withRequestDeadline(async (signal) => {
    const hasBody = options.body !== undefined;
    const response = await requestWithTokenRefresh(
      accessToken,
      (token) =>
        fetch(`${apiBase}${path}`, {
          method: options.method,
          headers: {
            Accept: "application/json",
            ...(hasBody ? { "Content-Type": "application/json" } : {}),
            ...(token ? { Authorization: `Bearer ${token}` } : {}),
          },
          ...(hasBody ? { body: JSON.stringify(options.body) } : {}),
          credentials: "include",
          signal,
        }),
      signal,
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
    return payload.data;
  }, signal);
}

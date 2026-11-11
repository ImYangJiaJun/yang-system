import { apiBase, parseJson, withRequestDeadline } from "@/engine";
import { ApiError, SecondFactorRequiredError } from "@/engine/http/errors";

/**
 * account 域业务流程请求：注册（用户名与密码）、密码重置、飞书登录流程。
 * 会话生命周期（login/refresh/logout/disable）属引擎会话协议，见 engine/session/lifecycle.ts。
 */

export type RegisteredUser = {
  id: number;
  username: string;
};

type ApiEnvelope = {
  code?: number;
  message?: string;
  data?: unknown;
};

async function requestPublicJson(
  path: string,
  options: { method?: "GET" | "POST"; body?: unknown } = {},
  signal?: AbortSignal,
): Promise<{ payload: ApiEnvelope; status: number; requestId?: string }> {
  return withRequestDeadline(async (signal) => {
    const hasBody = options.body !== undefined;
    const response = await fetch(`${apiBase}${path}`, {
      method: options.method ?? "GET",
      headers: {
        Accept: "application/json",
        ...(hasBody ? { "Content-Type": "application/json" } : {}),
      },
      ...(hasBody ? { body: JSON.stringify(options.body) } : {}),
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
  }, signal);
}

async function requestPublicAction(
  path: string,
  body: Record<string, string>,
  signal?: AbortSignal,
): Promise<{ payload: ApiEnvelope; status: number; requestId?: string }> {
  return requestPublicJson(path, { method: "POST", body }, signal);
}

function recordData(data: unknown): Record<string, unknown> | undefined {
  return data !== null && typeof data === "object" && !Array.isArray(data)
    ? (data as Record<string, unknown>)
    : undefined;
}

export type MfaEmailCodeChallenge = {
  expiresIn: number;
  resendAfter: number;
};

/// 登录 MFA 备用邮箱验证码：认证器不可用时请求向注册邮箱发送一次性登录验证码。
/// 后端等时重验密码（失败响应与登录一致，防枚举）；只有 TOTP 已激活的账号
/// 才会真实投递，其余情况返回统一 accepted 响应。
export async function requestMfaEmailCode(
  username: string,
  password: string,
  signal?: AbortSignal,
): Promise<MfaEmailCodeChallenge> {
  const result = await requestPublicAction(
    "/api/v1/users/mfa/email-code",
    { username, password },
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

export type LoginEmailCodeChallenge = {
  expiresIn: number;
  resendAfter: number;
};

/// 登录邮箱验证码（免密登录第一环节）：向登录邮箱发送一次性验证码。
/// 后端统一 accepted 响应防枚举，不暴露邮箱是否注册。
export async function requestLoginEmailCode(
  email: string,
  signal?: AbortSignal,
): Promise<LoginEmailCodeChallenge> {
  const result = await requestPublicAction(
    "/api/v1/users/login-email-code",
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
  signal?: AbortSignal,
): Promise<RegisteredUser> {
  const result = await requestPublicAction(
    "/api/v1/users/register",
    { username, password },
    signal,
  );
  const { payload } = result;
  const data = recordData(payload.data);
  if (
    typeof data?.id !== "number" ||
    !Number.isSafeInteger(data.id) ||
    data.id <= 0 ||
    typeof data.username !== "string" ||
    !data.username
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
  };
}

/* ---------------------------------------------------------------------------
 * 飞书登录（SSO）
 *
 * 端点形状见 docs/architecture/2026-11-11-feishu-sso-and-batch-approval-design.md §5.2：
 *   GET  /api/v1/auth/feishu/status        公开，登录入口是否可用
 *   GET  /api/v1/auth/feishu/choose        公开（pending token + nonce Cookie）
 *   POST /api/v1/auth/feishu/choose/bind   公开，账号密码（+ MFA）绑定已有账号
 *   POST /api/v1/auth/feishu/choose/create 公开，用户名 + 密码新建账号
 *   POST /api/v1/auth/feishu/mfa           公开，飞书登录的第二因子
 *
 * 认证器/恢复码的第二因子在 bind 上沿用 700012 两段式；本文件只负责把错误映射成
 * `SecondFactorRequiredError`，由页面决定弹出哪个输入框。
 * ------------------------------------------------------------------------- */

const SECOND_FACTOR_REQUIRED_CODE = 700012;

export type FeishuLoginAvailability = {
  enabled: boolean;
};

/// 登录页飞书入口的总开关。请求失败按「不可用」处理：入口隐藏比显示一个必然
/// 失败的按钮更可行动；调用方不得把失败当异常继续渲染。
export async function fetchFeishuLoginStatus(
  signal?: AbortSignal,
): Promise<FeishuLoginAvailability> {
  const result = await requestPublicJson(
    "/api/v1/auth/feishu/status",
    { method: "GET" },
    signal,
  );
  const data = recordData(result.payload.data);
  return { enabled: data?.enabled === true };
}

export type FeishuChooseProfile = {
  name: string;
  maskedEmail: string | null;
  /// 飞书邮箱是否命中已有系统账号（只做提示，不自动绑定）。
  emailMatched: boolean;
  /// 后端是否允许在该部署上新建账号；缺省按允许处理（配置默认 true）。
  allowCreate: boolean;
  /// 发起登录时记录的回跳路径（后端查 pending token 得到；选择页 URL 本身不带）。
  redirect: string | null;
};

/// 邮箱脱敏兜底：后端本应直接返回脱敏值；这里再收一道，避免任何形态把完整
/// 飞书邮箱铺到公开页面上。
function maskEmail(email: string): string {
  const trimmed = email.trim();
  const at = trimmed.lastIndexOf("@");
  if (at <= 0) return "***";
  const local = trimmed.slice(0, at);
  const domain = trimmed.slice(at + 1);
  const visible = local.slice(0, 1) || "*";
  return `${visible}***@${domain}`;
}

function stringField(
  data: Record<string, unknown> | undefined,
  ...keys: string[]
): string | null {
  for (const key of keys) {
    const value = data?.[key];
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return null;
}

/// 读取账号选择页的只读信息（不消费 pending token）。
export async function fetchFeishuChooseProfile(
  token: string,
  signal?: AbortSignal,
): Promise<FeishuChooseProfile> {
  const result = await requestPublicJson(
    `/api/v1/auth/feishu/choose?token=${encodeURIComponent(token)}`,
    { method: "GET" },
    signal,
  );
  const data = recordData(result.payload.data);
  // 飞书 user_info 的姓名可能缺失（授权 scope 不完整时）；缺名不阻塞绑定/新建，
  // 用中性占位而不是把一个可继续的流程判成错误。
  const name =
    stringField(data, "name", "feishu_name", "external_name") ?? "飞书用户";
  const rawMasked = stringField(data, "email_masked", "masked_email");
  const rawEmail = stringField(data, "email");
  return {
    name,
    maskedEmail: rawMasked ?? (rawEmail ? maskEmail(rawEmail) : null),
    // 后端当前字段是 `email_matches_existing`；其余别名留给早期契约。
    emailMatched:
      data?.email_matches_existing === true ||
      data?.email_matched === true ||
      data?.has_account === true ||
      data?.existing_account === true,
    allowCreate: data?.allow_create !== false,
    redirect:
      typeof data?.redirect === "string" && data.redirect
        ? data.redirect
        : null,
  };
}

export type FeishuAuthResult = {
  accessToken: string;
  /// 后端校验过白名单后的回跳路径；缺省时由页面使用 URL 上的 redirect。
  redirect: string | null;
};

function parseFeishuAuthResult(
  payload: ApiEnvelope,
  status: number,
  requestId: string | undefined,
): FeishuAuthResult {
  const data = recordData(payload.data);
  if (typeof data?.access_token !== "string" || !data.access_token) {
    throw new ApiError("飞书登录响应缺少有效 Token", {
      status,
      code: payload.code,
      requestId,
      details: payload,
    });
  }
  return {
    accessToken: data.access_token,
    redirect:
      typeof data.redirect === "string" && data.redirect ? data.redirect : null,
  };
}

function mapSecondFactorRequired(cause: unknown): never {
  if (cause instanceof ApiError && cause.code === SECOND_FACTOR_REQUIRED_CODE) {
    throw new SecondFactorRequiredError(cause.message, {
      code: cause.code,
      requestId: cause.requestId,
    });
  }
  throw cause;
}

/// 选择页「绑定已有账号」：账号标识（用户名或已验证邮箱）+ 密码。
/// 目标账号启用 TOTP 时，后端返回 700012；带上 `mfaCode` 重试同一次绑定。
export async function bindFeishuChoose(
  input: {
    token: string;
    username: string;
    password: string;
    mfaCode?: string;
  },
  signal?: AbortSignal,
): Promise<FeishuAuthResult> {
  const mfaCode = input.mfaCode?.trim();
  try {
    const result = await requestPublicJson(
      "/api/v1/auth/feishu/choose/bind",
      {
        method: "POST",
        body: {
          token: input.token,
          username: input.username,
          password: input.password,
          ...(mfaCode ? { mfa_code: mfaCode } : {}),
        },
      },
      signal,
    );
    return parseFeishuAuthResult(
      result.payload,
      result.status,
      result.requestId,
    );
  } catch (cause) {
    return mapSecondFactorRequired(cause);
  }
}

/// 选择页「新建账号」：与正常注册一致，只传一个 password 字段。
export async function createFeishuChoose(
  input: { token: string; username: string; password: string },
  signal?: AbortSignal,
): Promise<FeishuAuthResult> {
  const result = await requestPublicJson(
    "/api/v1/auth/feishu/choose/create",
    {
      method: "POST",
      body: {
        token: input.token,
        username: input.username,
        password: input.password,
      },
    },
    signal,
  );
  return parseFeishuAuthResult(result.payload, result.status, result.requestId);
}

/// 已绑定用户的飞书登录第二因子：TOTP 动态码 / 恢复码 / 备用邮箱验证码。
export async function completeFeishuMfa(
  input: { token: string; mfaCode: string },
  signal?: AbortSignal,
): Promise<FeishuAuthResult> {
  const result = await requestPublicJson(
    "/api/v1/auth/feishu/mfa",
    {
      method: "POST",
      body: {
        token: input.token,
        mfa_code: input.mfaCode.trim(),
      },
    },
    signal,
  );
  return parseFeishuAuthResult(result.payload, result.status, result.requestId);
}

/// 已绑定用户走飞书登录时，向 MFA 备用邮箱发送一次性验证码。
export async function requestFeishuMfaEmailCode(
  token: string,
  signal?: AbortSignal,
): Promise<void> {
  await requestPublicJson(
    "/api/v1/auth/feishu/mfa/email-code",
    { method: "POST", body: { token } },
    signal,
  );
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

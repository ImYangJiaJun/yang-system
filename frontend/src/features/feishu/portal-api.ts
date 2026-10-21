/**
 * 飞书查询门户的数据访问层。
 *
 * 两个方向**刻意不同源**：
 *
 * - **管理侧**（列表 / 建 / 改 / 删 / 启停 / 口令 / 日志）走引擎的 Action 调用，
 *   与 `api.ts` 同一约定：`invokeAction` 从目录里解析 method + path 并负责鉴权、
 *   刷新、信封解析。权限就是「目录里有没有这粒 `operation_id`」——服务端只在
 *   对应权限位下注册 Action，所以 `canWritePortals` 读目录而不是解析 JWT。
 * - **公开侧**（门户元信息 + 查询）**不走目录**：公开页在未登录时就该能打开，
 *   而目录本身要认证才拉得到。所以它像 `features/account/api.ts` 一样用裸 `fetch`
 *   直连公开端点，`credentials: "omit"`——这是匿名访客的请求，不该带 cookie。
 *
 * 管理侧的 7 粒 `operation_id` 与后端 `src/addon/feishu/portal/actions/*.rs` 的
 * `action_name!(...)` 逐个对齐（模块名 `feishu.portal`）。写一个不存在的 id 的后果
 * 是**静默**的：`hasOperation` 恒为 false，写侧入口永远不渲染。
 */

import {
  keepPreviousData,
  useQuery,
  useQueryClient,
  type UseQueryResult,
} from "@tanstack/react-query";
import { useMemo } from "react";

import {
  ApiError,
  hasOperation,
  useSessionCredentials,
  useUiCatalog,
} from "@/engine";
import type { UiCatalog } from "@/engine";
import type { SessionContext } from "@/engine";
import { apiBase, parseJson } from "@/engine/http/http";
import { requestWithTokenRefresh } from "@/engine/session/auth-session";

import { feishuQueryKeys, invokeFeishuAction } from "./api";
import type { FeishuInvokeDeps } from "./api";
import type {
  AccessLogEntry,
  AccessLogListResponse,
  CreatedPortal,
  CreatePortalInput,
  Portal,
  PortalAccessMode,
  PortalListResponse,
  PortalQueryResponse,
  ToggledPortal,
  UpdatePortalInput,
} from "./types";
import { PORTAL_ERROR_CODES } from "./types";

/// 管理侧门户 Action。**每一粒都必须是服务端真实注册过的 `operation_id`**。
export const PORTAL_OPERATION_IDS = {
  list: "feishu.portal.list_portals",
  create: "feishu.portal.create_portal",
  update: "feishu.portal.update_portal",
  remove: "feishu.portal.delete_portal",
  toggle: "feishu.portal.toggle_portal",
  accessLog: "feishu.portal.portal_access_log",
  setPassphrase: "feishu.portal.set_passphrase",
} as const;

/// 能否看门户列表（详情页「查询门户」Tab 的渲染条件）。
export function canReadPortals(catalog: UiCatalog | undefined): boolean {
  return hasOperation(catalog, PORTAL_OPERATION_IDS.list);
}

/// 能否写门户（建 / 改 / 删 / 启停 / 口令的渲染条件）。
export function canWritePortals(catalog: UiCatalog | undefined): boolean {
  return hasOperation(catalog, PORTAL_OPERATION_IDS.create);
}

/* ------------------------------ 小工具 ------------------------------ */

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

function asNumber(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function asNullableString(value: unknown): string | null {
  return typeof value === "string" && value !== "" ? value : null;
}

/* ------------------------------ 响应解析 ------------------------------ */

function parsePortal(raw: Record<string, unknown>): Portal | null {
  const id = typeof raw.id === "number" ? raw.id : null;
  const slug = typeof raw.slug === "string" ? raw.slug : "";
  if (id === null || slug === "") return null;
  return {
    id,
    datasource_id: asNumber(raw.datasource_id, 0),
    title: typeof raw.title === "string" ? raw.title : "",
    slug,
    custom_slug: asNullableString(raw.custom_slug),
    query_field_id:
      typeof raw.query_field_id === "string" ? raw.query_field_id : "",
    query_field_source_key:
      typeof raw.query_field_source_key === "string"
        ? raw.query_field_source_key
        : "",
    access_mode: asAccessMode(raw.access_mode),
    exposed_columns:
      typeof raw.exposed_columns === "string" ? raw.exposed_columns : "[]",
    result_labels: asNullableString(raw.result_labels),
    empty_message: asNullableString(raw.empty_message),
    rate_limit_per_min: asNumber(raw.rate_limit_per_min, 30),
    fuzzy_enabled: raw.fuzzy_enabled !== false,
    fuzzy_min_digits: asNumber(raw.fuzzy_min_digits, 4),
    enabled: raw.enabled !== false,
    created_at: asNumber(raw.created_at, 0),
    updated_at: asNumber(raw.updated_at, 0),
  };
}

function asAccessMode(value: unknown): PortalAccessMode {
  return value === "passphrase" || value === "login" ? value : "public";
}

function parsePortalPage(data: unknown): PortalListResponse {
  const record = asRecord(data);
  const items = (Array.isArray(record?.items) ? record.items : [])
    .map((item) => asRecord(item))
    .filter((item): item is Record<string, unknown> => item !== undefined)
    .map(parsePortal)
    .filter((item): item is Portal => item !== null);
  return {
    items,
    page: asNumber(record?.page, 1),
    page_size: asNumber(record?.page_size, items.length),
    total: typeof record?.total === "number" ? record.total : null,
  };
}

function parseAccessLogPage(data: unknown): AccessLogListResponse {
  const record = asRecord(data);
  const items = (Array.isArray(record?.items) ? record.items : [])
    .map((item) => asRecord(item))
    .filter((item): item is Record<string, unknown> => item !== undefined)
    .map((raw): AccessLogEntry => ({
      id: asNumber(raw.id, 0),
      portal_id: asNumber(raw.portal_id, 0),
      query_value_hash:
        typeof raw.query_value_hash === "string" ? raw.query_value_hash : "",
      result_count: asNumber(raw.result_count, 0),
      ip_address: asNullableString(raw.ip_address),
      user_agent: asNullableString(raw.user_agent),
      access_mode_used:
        typeof raw.access_mode_used === "string" ? raw.access_mode_used : "",
      status: asLogStatus(raw.status),
      created_at: asNumber(raw.created_at, 0),
    }));
  return {
    items,
    page: asNumber(record?.page, 1),
    page_size: asNumber(record?.page_size, items.length),
    total: typeof record?.total === "number" ? record.total : null,
  };
}

function asLogStatus(value: unknown): AccessLogEntry["status"] {
  return value === "hit" || value === "rate_limited" || value === "forbidden"
    ? value
    : "miss";
}

/* ------------------------------ 管理侧 API ------------------------------ */

/// 某个数据源下的全部门户。`datasource_id` 折成 `where` 等值条件（已声明 filterable）。
export async function listPortals(
  datasourceId: number,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<PortalListResponse> {
  const result = await invokeFeishuAction(
    deps,
    PORTAL_OPERATION_IDS.list,
    {
      page: 1,
      page_size: 100,
      where: { type: "eq", field: "datasource_id", value: datasourceId },
      order_by: [
        { field: "created_at", direction: "Desc" },
        { field: "id", direction: "Asc" },
      ],
      count_total: true,
    },
    signal,
  );
  return parsePortalPage(result.data);
}

export async function createPortal(
  input: CreatePortalInput,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<CreatedPortal> {
  const result = await invokeFeishuAction(
    deps,
    PORTAL_OPERATION_IDS.create,
    {
      datasource_id: input.datasource_id,
      title: input.title,
      query_field_id: input.query_field_id,
      exposed_columns: input.exposed_columns,
      access_mode: input.access_mode,
      passphrase: input.passphrase,
      result_labels: input.result_labels,
      empty_message: input.empty_message,
      rate_limit_per_min: input.rate_limit_per_min,
      fuzzy_enabled: input.fuzzy_enabled,
      fuzzy_min_digits: input.fuzzy_min_digits,
      // 空短链必须**省略**：后端对 `Some("")` 走形状校验（长度 1..=64）会判参数错。
      custom_slug: input.custom_slug === "" ? undefined : input.custom_slug,
    },
    signal,
  );
  const data = asRecord(result.data);
  const portalId = typeof data?.portal_id === "number" ? data.portal_id : null;
  if (portalId === null) {
    throw new ApiError("创建门户的回执缺少 portal_id", { status: 200 });
  }
  return {
    portalId,
    slug: typeof data?.slug === "string" ? data.slug : "",
  };
}

/// 更新门户。缺省字段靠**省略键**表达「不改」（后端 `deny_unknown_fields`）。
///
/// 口令**不在这里**：后端把它拆到独立的 `set_passphrase`（见 [`setPassphrase`]）。
export async function updatePortal(
  input: UpdatePortalInput,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<void> {
  await invokeFeishuAction(
    deps,
    PORTAL_OPERATION_IDS.update,
    {
      portal_id: input.portal_id,
      title: input.title,
      query_field_id: input.query_field_id,
      exposed_columns: input.exposed_columns,
      access_mode: input.access_mode,
      result_labels: input.result_labels,
      empty_message: input.empty_message,
      rate_limit_per_min: input.rate_limit_per_min,
      fuzzy_enabled: input.fuzzy_enabled,
      fuzzy_min_digits: input.fuzzy_min_digits,
      // 显式空串 = 清空短链（后端如此约定），所以这个键**照发**。
      custom_slug: input.custom_slug,
    },
    signal,
  );
}

export async function deletePortal(
  portalId: number,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<void> {
  await invokeFeishuAction(
    deps,
    PORTAL_OPERATION_IDS.remove,
    { portal_id: portalId },
    signal,
  );
}

export async function togglePortal(
  portalId: number,
  enabled: boolean,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<ToggledPortal> {
  const result = await invokeFeishuAction(
    deps,
    PORTAL_OPERATION_IDS.toggle,
    { portal_id: portalId, enabled },
    signal,
  );
  const data = asRecord(result.data);
  return {
    portalId: typeof data?.portal_id === "number" ? data.portal_id : portalId,
    enabled: data?.enabled === true,
  };
}

/// 设置/更换口令。`expiresAt` 是 unix 秒，省略 = 不过期。
export async function setPassphrase(
  portalId: number,
  passphrase: string,
  deps: FeishuInvokeDeps,
  expiresAt?: number,
  signal?: AbortSignal,
): Promise<void> {
  await invokeFeishuAction(
    deps,
    PORTAL_OPERATION_IDS.setPassphrase,
    { portal_id: portalId, passphrase, expires_at: expiresAt },
    signal,
  );
}

export async function getPortalAccessLog(
  portalId: number,
  page: number,
  pageSize: number,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<AccessLogListResponse> {
  const result = await invokeFeishuAction(
    deps,
    PORTAL_OPERATION_IDS.accessLog,
    {
      // 后端把标准分页契约 `#[serde(flatten)]` 进同一个结构，所以 `portal_id`
      // 与 `page` 等键平铺在同一层。
      portal_id: portalId,
      page,
      page_size: pageSize,
      order_by: [
        { field: "created_at", direction: "Desc" },
        { field: "id", direction: "Desc" },
      ],
      count_total: true,
    },
    signal,
  );
  return parseAccessLogPage(result.data);
}

/* ------------------------------ 公开侧 API ------------------------------ */

type ApiEnvelope = { code?: number; message?: string; data?: unknown };

/// 公开端点的信封解析。
///
/// # 为什么要带上 Bearer（而不是纯匿名）
///
/// `access_mode = login` 的门户靠后端 `ctx.authenticated_user()` 判权，而那个判据
/// 只在**带 Bearer** 的请求上才有值——一个不带头的请求永远是匿名的，于是 login 模式
/// 变成「登录了也进不去」的死循环。所以：**有会话就带上，没有就不带**（匿名访客
/// 照旧匿名，public / passphrase 不受影响）。
///
/// 走 `requestWithTokenRefresh` 而不是裸 fetch：access token 过期时透明续期；
/// 续期也失败才把会话标记失效（SessionBridge 会把人送去登录页），而不是让公开页
/// 卡在一句 `HTTP 401`。
///
/// **cookie 一律不发**（`credentials: "omit"`）：公开端点不用 cookie 鉴权，少一个
/// 跨站请求面。
///
/// # 失败为什么按信封 `code` 分类
///
/// 业务失败（门户不存在 / 口令不对 / 需登录）在**信封 code** 上，HTTP 状态仍是 200
/// （后端 `ApiResponse::fail` 不带 http_status）——所以调用方必须看 `ApiError.code`，
/// 不能看 `status`。只有限速是真的 `Err`，那一个才是 429。
async function publicJson(
  path: string,
  init: RequestInit,
  token?: string,
): Promise<unknown> {
  const response = await requestWithTokenRefresh(token, (current) =>
    fetch(`${apiBase}${path}`, {
      ...init,
      headers: {
        Accept: "application/json",
        ...(current ? { Authorization: `Bearer ${current}` } : {}),
        ...init.headers,
      },
      credentials: "omit",
    }),
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
}

/// 公开端点的失败 → 页面该做什么。
///
/// 分类**只能按信封 `code`**：业务拒绝是 HTTP 200（见 `publicJson` 的说明），
/// 按 `status` 分会把「门户不存在」当成成功、把「口令不对」当成网络故障。
export type PortalFailureView = {
  message: string;
  /// 该把口令框展开（后端说「请输入口令」/「口令不正确」/「口令已过期」）。
  revealPassphrase: boolean;
  /// 该送去登录页（`access_mode = login` 且未登录）。
  loginRequired: boolean;
};

export function portalFailureView(cause: unknown): PortalFailureView {
  if (cause instanceof ApiError) {
    if (cause.status === 429) {
      return {
        message: "查询过于频繁，请稍后再试",
        revealPassphrase: false,
        loginRequired: false,
      };
    }
    if (cause.code === PORTAL_ERROR_CODES.notFound) {
      return {
        // 后端把「停用」也归到这一码（不泄露存在性），所以文案两说。
        message: "门户不存在或已停用",
        revealPassphrase: false,
        loginRequired: false,
      };
    }
    if (cause.code === PORTAL_ERROR_CODES.forbidden) {
      // 后端在这一码里区分「需要登录」与口令类文案，文案是唯一判据。
      const needsLogin = cause.message.includes("登录");
      return {
        message: cause.message,
        revealPassphrase: !needsLogin,
        loginRequired: needsLogin,
      };
    }
    return {
      message: cause.message,
      revealPassphrase: false,
      loginRequired: false,
    };
  }
  return {
    message: cause instanceof Error ? cause.message : String(cause),
    revealPassphrase: false,
    loginRequired: false,
  };
}

/// 公开页渲染所需的门户元信息（后端 `GET /api/v1/feishu/portal/{slug}`）。
///
/// 与查询响应**刻意分开**：标题、查询键提示、访问模式是配置，页面在第一次查询
/// 之前就要用；把它们并进查询响应意味着「先查一次才知道该填什么」。
export interface PortalMetadata {
  title: string;
  /// 查询键的展示名，直接当输入框 placeholder。
  query_field_label: string;
  access_mode: PortalAccessMode;
  /// 查无结果时的提示；门户未配置时后端回空串，这里折成 `undefined`。
  empty_message?: string;
  /// 是否启用模糊匹配。
  fuzzy_enabled: boolean;
  /// 触发模糊匹配所需的最少连续数字位数。
  fuzzy_min_digits: number;
}

/// 取门户元信息。公开端点，停用与不存在同回 `40401`（不泄露存在性）。
///
/// 与 [`queryPortal`] 共用 [`publicJson`]：一样按信封 `code` 分类失败，所以 40401
/// 会变成带 `code` 的 [`ApiError`]，页面据此显示「门户不存在或已停用」。
export async function getPortalMetadata(
  slug: string,
  session?: SessionContext,
  signal?: AbortSignal,
): Promise<PortalMetadata> {
  const data = asRecord(
    await publicJson(
      `/api/v1/feishu/portal/${encodeURIComponent(slug)}`,
      { method: "GET", signal },
      session?.token,
    ),
  );
  return {
    title: typeof data?.title === "string" ? data.title : "",
    query_field_label:
      typeof data?.query_field_label === "string" ? data.query_field_label : "",
    access_mode: asAccessMode(data?.access_mode),
    empty_message: asNullableString(data?.empty_message) ?? undefined,
    fuzzy_enabled: data?.fuzzy_enabled !== false,
    fuzzy_min_digits:
      typeof data?.fuzzy_min_digits === "number" ? data.fuzzy_min_digits : 4,
  };
}

/// 执行一次门户查询（精确匹配）。
///
/// `session` 只为 `access_mode = login` 而存在：带上会话里的 Bearer，后端才认得出
/// 「这个访客已登录」；匿名时它是 `undefined`，请求就是纯匿名的。
export async function queryPortal(
  slug: string,
  value: string,
  passphrase?: string,
  session?: SessionContext,
  signal?: AbortSignal,
): Promise<PortalQueryResponse> {
  const body: Record<string, unknown> = { value };
  if (passphrase !== undefined && passphrase !== "")
    body.passphrase = passphrase;
  const data = asRecord(
    await publicJson(
      `/api/v1/feishu/portal/${encodeURIComponent(slug)}/query`,
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
        signal,
      },
      session?.token,
    ),
  );
  const columns = (Array.isArray(data?.columns) ? data.columns : [])
    .map((item) => asRecord(item))
    .filter((item): item is Record<string, unknown> => item !== undefined)
    .map((raw) => ({
      field_id: typeof raw.field_id === "string" ? raw.field_id : "",
      label: typeof raw.label === "string" ? raw.label : "",
      field_type: typeof raw.field_type === "string" ? raw.field_type : "text",
    }))
    .filter((column) => column.field_id !== "");
  const rows = (Array.isArray(data?.rows) ? data.rows : [])
    .map((item) => asRecord(item))
    .filter((item): item is Record<string, unknown> => item !== undefined)
    .map((raw) => ({ values: asRecord(raw.values) ?? {} }));
  return {
    found: data?.found === true,
    columns,
    rows,
    message: typeof data?.message === "string" ? data.message : undefined,
  };
}

/// 取门户查询建议（自动补全候选列表）。
///
/// 公开端点，匿名可访问。按输入前缀返回匹配的查询值候选，供前端自动补全下拉。
/// 失败时返回空数组（不抛异常），让 UI 静默降级。
export async function suggestPortal(
  slug: string,
  query: string,
  session?: SessionContext,
  signal?: AbortSignal,
): Promise<string[]> {
  const params = new URLSearchParams({ query });
  try {
    const data = asRecord(
      await publicJson(
        `/api/v1/feishu/portal/${encodeURIComponent(slug)}/suggest?${params}`,
        { method: "GET", signal },
        session?.token,
      ),
    );
    return Array.isArray(data?.suggestions)
      ? data.suggestions.filter(
          (item): item is string => typeof item === "string",
        )
      : [];
  } catch {
    // 建议失败不阻塞查询：静默返回空列表，让 UI 降级为纯输入框。
    return [];
  }
}

/* ------------------------------ React 接线 ------------------------------ */

/// 公开页 mount 时取的门户元信息。
///
/// `retry: false`：门户不存在 / 已停用是**确定性**结果，react-query 默认的 3 次
/// 退避重试会让骨架多转好几秒才显示「门户不存在或已停用」。
export function usePortalMetadata(
  slug: string,
): UseQueryResult<PortalMetadata> {
  const session = useSessionCredentials();
  return useQuery({
    enabled: slug !== "",
    queryKey: feishuQueryKeys.portalMetadata(slug),
    queryFn: ({ signal }) => getPortalMetadata(slug, session, signal),
    staleTime: 60_000,
    retry: false,
  });
}

/// 某个数据源下的门户列表（管理面板用）。
export function usePortalList(
  datasourceId: number | null,
): UseQueryResult<PortalListResponse> {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;
  return useQuery({
    enabled: datasourceId !== null && canReadPortals(catalogData),
    queryKey: feishuQueryKeys.portals(datasourceId ?? -1),
    queryFn: ({ signal }) =>
      listPortals(
        datasourceId as number,
        { catalog: catalogData, session },
        signal,
      ),
    placeholderData: keepPreviousData,
    staleTime: 15_000,
  });
}

/// 访问日志（按门户 + 分页）。
export function usePortalAccessLog(
  portalId: number,
  page: number,
  pageSize: number,
): UseQueryResult<AccessLogListResponse> {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;
  return useQuery({
    enabled: canReadPortals(catalogData),
    queryKey: feishuQueryKeys.portalAccessLog(portalId, page, pageSize),
    queryFn: ({ signal }) =>
      getPortalAccessLog(
        portalId,
        page,
        pageSize,
        {
          catalog: catalogData,
          session,
        },
        signal,
      ),
    placeholderData: keepPreviousData,
    staleTime: 15_000,
  });
}

/// 管理面板用的一组已绑定目录与会话的门户写操作。
export type PortalActions = {
  createPortal: (input: CreatePortalInput) => Promise<CreatedPortal>;
  updatePortal: (input: UpdatePortalInput) => Promise<void>;
  deletePortal: (portalId: number) => Promise<void>;
  togglePortal: (portalId: number, enabled: boolean) => Promise<ToggledPortal>;
  /// 换口令（独立端点：`update_portal` 不收 passphrase）。`expiresAt` 是 unix 秒。
  setPassphrase: (
    portalId: number,
    passphrase: string,
    expiresAt?: number,
  ) => Promise<void>;
};

export function usePortalActions(): PortalActions {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;
  const deps = useMemo<FeishuInvokeDeps>(
    () => ({ catalog: catalogData, session }),
    [catalogData, session],
  );
  return useMemo(
    () => ({
      createPortal: (input: CreatePortalInput) => createPortal(input, deps),
      updatePortal: (input: UpdatePortalInput) => updatePortal(input, deps),
      deletePortal: (portalId: number) => deletePortal(portalId, deps),
      togglePortal: (portalId: number, enabled: boolean) =>
        togglePortal(portalId, enabled, deps),
      setPassphrase: (
        portalId: number,
        passphrase: string,
        expiresAt?: number,
      ) => setPassphrase(portalId, passphrase, deps, expiresAt),
    }),
    [deps],
  );
}

/// 门户变更后回读列表：回执只给单条，列表要重拉。
export function useInvalidatePortals(): (
  datasourceId: number,
) => Promise<void> {
  const queryClient = useQueryClient();
  return useMemo(
    () => (datasourceId: number) =>
      queryClient.invalidateQueries({
        queryKey: feishuQueryKeys.portals(datasourceId),
      }),
    [queryClient],
  );
}

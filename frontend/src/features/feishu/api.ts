/**
 * 飞书数据源控制台的数据访问层：`@/engine` 的 Action 调用**薄封装** + query key 工厂。
 *
 * 本文件不写 `fetch`、不新建第二个 HTTP 客户端：`invokeAction` 已经从 Action schema
 * 解析 method + path，并负责 token 刷新、信封解析与错误映射。
 *
 * 三个契约事实（都已对着后端源码核实，不要凭直觉改）：
 * 1. 请求体是 `deny_unknown_fields`：**没声明的键一个都不能多传**，因此「留空 = 不改」
 *    只能靠**省略该 key**实现——这里用 `undefined` 表达省略，引擎不会把它写进 body；
 * 2. `order_by[].direction` 是 PascalCase 的 `"Asc"` / `"Desc"`；
 * 3. `approval_options` 是唯一不吃 `Authorization` 的 Action：明文 Token 走**请求体**的
 *    `token` 字段，且它的路由 `{source_key}` **没有**在 handler 里声明成 path 参数。
 */

import {
  keepPreviousData,
  useQuery,
  type UseQueryResult,
} from "@tanstack/react-query";
import { useCallback, useMemo } from "react";

import {
  ApiError,
  invokeAction,
  useSessionCredentials,
  useUiCatalog,
} from "@/engine";
import type {
  ActionDemoSchema,
  InvocationResult,
  SessionContext,
  UiCatalog,
} from "@/engine";

import { withStableOrder } from "./list-query";
import type {
  DatasourceItem,
  IngestMode,
  DatasourceListQuery,
  DatasourceStatus,
  DatasourceStatusFilter,
  DefaultLocale,
  ListPage,
  OptionItem,
  OptionListQuery,
  OrderByClause,
  TokenPrecheckResult,
} from "./types";
import { approvalCodeHint, approvalCodeVerdict } from "./types";

/// 后端 8 个 Action 里控制台要用的 6 个（另两个是给飞书/自动化调用的机器入口）。
export const DATASOURCE_OPERATION_IDS = {
  list: "feishu.datasource.list_datasources",
  create: "feishu.datasource.create_datasource",
  update: "feishu.datasource.update_datasource",
  remove: "feishu.datasource.delete_datasource",
} as const;

export const OPTION_OPERATION_IDS = {
  list: "feishu.option.list_options",
  approvalOptions: "feishu.option.approval_options",
} as const;

/// 详情页默认排序：按「最近推送」倒序，并以唯一键 `option_id` 收尾。
/// 一次推送会把整批选项写成同一个 unix 秒，只按 `updated_at` 排会得到不稳定的平局，
/// 翻页就会重复或漏行。
export const DEFAULT_OPTION_ORDER_BY: OrderByClause[] = [
  { field: "updated_at", direction: "Desc" },
  { field: "option_id", direction: "Asc" },
];

/// 调用 Action 需要的两样东西：目录（含 method/path）与会话凭据。
export type FeishuInvokeDeps = {
  catalog: UiCatalog | undefined;
  session: SessionContext;
};

/* ------------------------------- 权限门控 -------------------------------- */

/**
 * 权限就是「目录里有没有这个 operation_id」（设计 §4.5）。
 * 目录本身已按身份投影（服务端用 `policy.allows(context)` 过滤），
 * 所以不要解析 JWT，也不要用 `presentation.availability`——那是声明期静态提示，
 * 后端测试明写它不能替代服务端授权。
 */
export function hasOperation(
  catalog: UiCatalog | undefined,
  operationId: string,
): boolean {
  return (
    catalog?.actions.some((action) => action.operation_id === operationId) ??
    false
  );
}

/// 能否看列表（侧边栏那条 NavLink 的渲染条件）。
export function canReadDatasources(catalog: UiCatalog | undefined): boolean {
  return hasOperation(catalog, DATASOURCE_OPERATION_IDS.list);
}

/// 能否写（「添加数据源」与列表项上的重命名/停用/启用/删除是否**渲染**，不是禁用）。
export function canWriteDatasources(catalog: UiCatalog | undefined): boolean {
  return hasOperation(catalog, DATASOURCE_OPERATION_IDS.create);
}

/// 能否看某个数据源的选项（详情页 403 分支的分支条件）。
export function canReadOptions(catalog: UiCatalog | undefined): boolean {
  return hasOperation(catalog, OPTION_OPERATION_IDS.list);
}

/* -------------------------------- query key ------------------------------- */

/// 顶层键：会话边界（`session-reset` 的 `queryClient.clear()`）按前缀清空查询缓存，
/// 这里以 `"feishu"` 作为本域的唯一顶层段。
export const FEISHU_QUERY_ROOT = "feishu";

/// query key 工厂。key 里只放可序列化的原始值，形状稳定、可断言。
export const feishuQueryKeys = {
  root: () => [FEISHU_QUERY_ROOT] as const,
  datasources: () => [FEISHU_QUERY_ROOT, "datasources"] as const,
  datasourceList: (query: DatasourceListQuery) =>
    [
      ...feishuQueryKeys.datasources(),
      {
        page: query.page,
        pageSize: query.pageSize,
        search: query.search.trim(),
        status: query.status,
        orderBy: withStableOrder(query.orderBy),
      },
    ] as const,
  options: (sourceKey: string) =>
    [FEISHU_QUERY_ROOT, "options", sourceKey] as const,
  optionList: (query: OptionListQuery) =>
    [
      ...feishuQueryKeys.options(query.sourceKey),
      {
        page: query.page,
        pageSize: query.pageSize,
        orderBy:
          query.orderBy.length > 0 ? query.orderBy : DEFAULT_OPTION_ORDER_BY,
      },
    ] as const,
};

/* ------------------------------- Action 调用 ------------------------------ */

/// 目录里找不到 operation_id 时抛明确错误，而不是静默发一个空请求。
export function requireAction(
  catalog: UiCatalog | undefined,
  operationId: string,
): ActionDemoSchema {
  const action = catalog?.actions.find(
    (candidate) => candidate.operation_id === operationId,
  );
  if (!action) {
    throw new Error(
      `UI 目录里找不到 Action「${operationId}」：当前身份可能没有该权限，或目录尚未加载完成`,
    );
  }
  return action;
}

/// 薄封装：找 Action → invokeAction。`data` 原样返回给上层解析。
export async function invokeFeishuAction(
  deps: FeishuInvokeDeps,
  operationId: string,
  values: Record<string, unknown>,
  signal?: AbortSignal,
): Promise<InvocationResult> {
  const action = requireAction(deps.catalog, operationId);
  return invokeAction(action, values, deps.session, signal);
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

function asNumber(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function asString(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

/* ------------------------------- 输入构造 -------------------------------- */

/// 状态筛选 → `where` 布尔树；`all` 时返回 undefined（引擎不会把 undefined 写进 body）。
function statusWhere(
  status: DatasourceStatusFilter,
): Record<string, unknown> | undefined {
  return status === "all"
    ? undefined
    : { type: "eq", field: "status", value: status };
}

/// 列表查询 → 请求体。缺省键一律用 `undefined` 省略，不传空串/空数组。
export function buildDatasourceListBody(
  query: DatasourceListQuery,
): Record<string, unknown> {
  const search = query.search.trim();
  return {
    page: query.page,
    page_size: query.pageSize,
    search: search === "" ? undefined : search,
    where: statusWhere(query.status),
    // 恒非空：排序键是分页确定性的前提，调用方给了空数组也要兜住。
    order_by: withStableOrder(query.orderBy),
    count_total: true,
  };
}

function buildOptionListBody(query: OptionListQuery): Record<string, unknown> {
  return {
    page: query.page,
    page_size: query.pageSize,
    // `source_key` 是本服务自己的扩展字段，**发在顶层**，不是塞进 where
    source_key: query.sourceKey,
    order_by:
      query.orderBy.length > 0 ? query.orderBy : DEFAULT_OPTION_ORDER_BY,
    count_total: true,
  };
}

/* ------------------------------- 响应解析 -------------------------------- */

function parsePage<T>(
  data: unknown,
  parseItem: (raw: Record<string, unknown>) => T | null,
): ListPage<T> {
  const record = asRecord(data);
  const rawItems = Array.isArray(record?.items) ? record.items : [];
  const items = rawItems
    .map((raw) => asRecord(raw))
    .filter((raw): raw is Record<string, unknown> => raw !== undefined)
    .map(parseItem)
    .filter((item): item is T => item !== null);
  return {
    items,
    page: asNumber(record?.page, 1),
    pageSize: asNumber(record?.page_size, items.length),
    total: typeof record?.total === "number" ? record.total : null,
  };
}

function parseDatasourceItem(
  raw: Record<string, unknown>,
): DatasourceItem | null {
  const sourceKey = asString(raw.source_key);
  if (!sourceKey) return null;
  return {
    sourceKey,
    title: asString(raw.title),
    encryptEnabled: raw.encrypt_enabled === true,
    defaultLocale: asString(raw.default_locale),
    status: raw.status === "disabled" ? "disabled" : "active",
    updatedAt: asNumber(raw.updated_at, 0),
    ingestMode: asString(raw.ingest_mode),
    bitableBaseToken: asNullableString(raw.bitable_base_token),
    bitableTableId: asNullableString(raw.bitable_table_id),
    bitableViewId: asNullableString(raw.bitable_view_id),
    bitableFieldName: asNullableString(raw.bitable_field_name),
    linkageMapping: asNullableString(raw.linkage_mapping),
    lastPullAt: asNullableNumber(raw.last_pull_at),
    lastSuccessAt: asNullableNumber(raw.last_success_at),
    consecutiveFailures: asNumber(raw.consecutive_failures, 0),
    lastError: asNullableString(raw.last_error),
    snapshotDigest: asNullableString(raw.snapshot_digest),
  };
}

/// 可空时间戳：后端把「没有值」编码成 `null`（列本身可空），**不是 0**。
///
/// 与 `asNumber(x, 0)` 的区别很重要：0 是一个合法的时间戳语义（1970 年），
/// 用它表示「从未同步过」会让界面显示成 1970 而不是「—」。
function asNullableNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/// 可空字符串：空串与缺失一律折成 `null`，避免界面出现「有值但不可见」的空白格。
function asNullableString(value: unknown): string | null {
  return typeof value === "string" && value !== "" ? value : null;
}

function parseOptionItem(raw: Record<string, unknown>): OptionItem | null {
  const optionId = asString(raw.option_id);
  if (!optionId) return null;
  return {
    optionId,
    sourceKey: asString(raw.source_key),
    label: asString(raw.label),
    i18n: typeof raw.i18n === "string" ? raw.i18n : null,
    sortOrder: asNumber(raw.sort_order, 0),
    isDefault: raw.is_default === true,
    enabled: raw.enabled !== false,
    parentKey: asNullableString(raw.parent_key),
    updatedAt: asNumber(raw.updated_at, 0),
    lastPushAt: asNullableNumber(raw.last_push_at),
  };
}

/* ------------------------------- 列表与变更 ------------------------------- */

export async function listDatasources(
  query: DatasourceListQuery,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<ListPage<DatasourceItem>> {
  const result = await invokeFeishuAction(
    deps,
    DATASOURCE_OPERATION_IDS.list,
    buildDatasourceListBody(query),
    signal,
  );
  return parsePage(result.data, parseDatasourceItem);
}

export async function listOptions(
  query: OptionListQuery,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<ListPage<OptionItem>> {
  const result = await invokeFeishuAction(
    deps,
    OPTION_OPERATION_IDS.list,
    buildOptionListBody(query),
    signal,
  );
  return parsePage(result.data, parseOptionItem);
}

/// 坐标与取数方式。两者一起提交：单独给坐标而不说 `ingest_mode` 没有意义，
/// 而 `ingest_mode = "pull"` 又必须有坐标（后端会拒）。
export type DatasourceCoordinatesInput = {
  ingestMode: IngestMode;
  bitableBaseToken: string;
  bitableTableId: string;
  bitableViewId: string;
  bitableFieldName: string;
  linkageMapping: string;
};

/// 把坐标折成 wire 形状：**空串一律不发**。
///
/// 建源时后端对「传了空串」与「没传」同样处理（都落 NULL），但少发几个键能让
/// 请求体更接近「用户实际填了什么」，排查时更好读。
function coordinateBody(
  coordinates: DatasourceCoordinatesInput | undefined,
): Record<string, unknown> {
  if (coordinates === undefined) return {};
  const body: Record<string, unknown> = { ingest_mode: coordinates.ingestMode };
  const pairs: Array<[string, string]> = [
    ["bitable_base_token", coordinates.bitableBaseToken],
    ["bitable_table_id", coordinates.bitableTableId],
    ["bitable_view_id", coordinates.bitableViewId],
    ["bitable_field_name", coordinates.bitableFieldName],
    ["linkage_mapping", coordinates.linkageMapping],
  ];
  for (const [key, value] of pairs) {
    if (value.trim() !== "") body[key] = value.trim();
  }
  return body;
}

export type CreateDatasourceInput = {
  sourceKey: string;
  title: string;
  token: string;
  encryptEnabled: boolean;
  defaultLocale: DefaultLocale;
  /// 省略表示按后端默认（`push`）。
  coordinates?: DatasourceCoordinatesInput;
};

/// 新建。成功只回 `{source_key}`，不回整条记录——所以调用方必须回读列表。
export async function createDatasource(
  input: CreateDatasourceInput,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<{ sourceKey: string }> {
  const result = await invokeFeishuAction(
    deps,
    DATASOURCE_OPERATION_IDS.create,
    {
      source_key: input.sourceKey,
      title: input.title,
      token: input.token,
      encrypt_enabled: input.encryptEnabled,
      default_locale: input.defaultLocale,
      ...coordinateBody(input.coordinates),
    },
    signal,
  );
  const sourceKey = asString(asRecord(result.data)?.source_key);
  if (!sourceKey) {
    throw new Error("创建数据源的响应里没有 source_key，无法确认结果");
  }
  return { sourceKey };
}

export type UpdateDatasourceInput = {
  sourceKey: string;
  title?: string;
  /// 省略 = 不轮换 Token。传空串会被后端拒绝（`Token 不能为空`）。
  token?: string;
  encryptEnabled?: boolean;
  defaultLocale?: DefaultLocale;
  status?: DatasourceStatus;
  /// 坐标。**与其它字段不同：这里传空串是有意义的——表示清空该坐标。**
  /// 所以不做 `coordinateBody` 那套「空串不发」的折叠，逐字段原样发出。
  coordinates?: DatasourceCoordinatesInput;
};

/// 更新。`source_key` 之外**全部可选，省略即保持原值**——所以这里逐字段判 undefined，
/// 绝不能把缺省字段写成空串或 null 发出去。
export async function updateDatasource(
  input: UpdateDatasourceInput,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<{ affected: number }> {
  const body: Record<string, unknown> = { source_key: input.sourceKey };
  if (input.title !== undefined) body.title = input.title;
  // 留空 = 不改：空白 Token 一律按省略处理，避免踩到后端的「Token 不能为空」
  if (input.token !== undefined && input.token.trim() !== "") {
    body.token = input.token;
  }
  if (input.encryptEnabled !== undefined) {
    body.encrypt_enabled = input.encryptEnabled;
  }
  if (input.defaultLocale !== undefined) {
    body.default_locale = input.defaultLocale;
  }
  if (input.status !== undefined) body.status = input.status;
  // 坐标：传空串表示清空（后端语义如此），所以这里**不能**按「空即省略」处理——
  // 那会让「想清掉一个填错的 base_token」变得做不到。
  if (input.coordinates !== undefined) {
    body.ingest_mode = input.coordinates.ingestMode;
    body.bitable_base_token = input.coordinates.bitableBaseToken.trim();
    body.bitable_table_id = input.coordinates.bitableTableId.trim();
    body.bitable_view_id = input.coordinates.bitableViewId.trim();
    body.bitable_field_name = input.coordinates.bitableFieldName.trim();
    body.linkage_mapping = input.coordinates.linkageMapping.trim();
  }

  const result = await invokeFeishuAction(
    deps,
    DATASOURCE_OPERATION_IDS.update,
    body,
    signal,
  );
  return { affected: asNumber(asRecord(result.data)?.affected, 0) };
}

/// 删除。后端会在同一事务里连带停用其下全部选项，`disabled_options` 就是那个计数。
export async function deleteDatasource(
  sourceKey: string,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<{ deleted: number; disabledOptions: number }> {
  const result = await invokeFeishuAction(
    deps,
    DATASOURCE_OPERATION_IDS.remove,
    { source_key: sourceKey },
    signal,
  );
  const data = asRecord(result.data);
  return {
    deleted: asNumber(data?.deleted, 0),
    disabledOptions: asNumber(data?.disabled_options, 0),
  };
}

/* ------------------------------- 连通性预检 ------------------------------- */

/// 目录拿不到这条 Action 时的兜底契约（它是 public 端点，目录可能不投影它）。
const FALLBACK_APPROVAL_OPTIONS_ACTION: ActionDemoSchema = {
  operation_id: OPTION_OPERATION_IDS.approvalOptions,
  title: "飞书外部选项",
  description: "用明文 Token 试拉一次选项，验证数据源与 Token 是否匹配",
  method: "POST",
  path: "/api/v1/feishu/approval/options/{source_key}",
  params: [],
  input_schema: {},
  output_schema: {},
  request_media_type: "json",
  response_kind: "json",
  requires_auth: false,
};

/**
 * 预检用的请求契约。
 *
 * `{source_key}` 是路由模板里的路径段，但该 handler 的 `ParamInput::params()` 是空集，
 * 引擎的路径替换因此填不进去（会在最后一步抛「路径仍有未填写参数」）——这里显式补全，
 * 并把 params 清空，让明文 Token 走请求体。
 */
function approvalOptionsAction(
  catalog: UiCatalog | undefined,
  sourceKey: string,
): ActionDemoSchema {
  const declared = catalog?.actions.find(
    (action) => action.operation_id === OPTION_OPERATION_IDS.approvalOptions,
  );
  const base = declared ?? FALLBACK_APPROVAL_OPTIONS_ACTION;
  return {
    ...base,
    path: base.path.replace("{source_key}", encodeURIComponent(sourceKey)),
    params: [],
  };
}

/**
 * 用明文 Token 试拉一次选项。
 *
 * **必须排在创建/轮换成功之后**：该端点按 `source_key` 查库、再拿存储的哈希与传入的
 * 明文比对，所以数据源行得先存在。这也是全系统唯一能验证凭据的时刻——服务端永远
 * 回不出明文 Token，事后没有任何办法再校验一次。
 *
 * 失败不抛异常：它**不阻塞**创建结果，只回答「链路通没通」。
 */
export async function precheckApprovalOptions(
  sourceKey: string,
  token: string,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<TokenPrecheckResult> {
  try {
    const result = await invokeAction(
      approvalOptionsAction(deps.catalog, sourceKey),
      { token },
      deps.session,
      signal,
    );
    const data = asRecord(result.data);
    // 开了「加密返回」且服务端配了密钥时，`data.result` 是一段 base64 密文。
    if (typeof data?.result === "string") {
      return { status: "ok", optionCount: null, encrypted: true };
    }
    const body = asRecord(data?.result);
    const options = body?.options;
    return {
      status: "ok",
      optionCount: Array.isArray(options) ? options.length : 0,
      // 本页条数**不是**选项总数：`approval_options` 单页上限 100（后端 `PAGE_SIZE`），
      // 超过 100 条时这里只会看到 100。区分「还有更多」只能用 `nextPageToken`
      // ——它非空当且仅当还有下一页（后端由 COUNT 与 SELECT 的差值推导，
      // 没有下一页时整个键都不输出）。所以只认它，不猜。
      hasMore: asString(body?.nextPageToken) !== "",
      encrypted: false,
    };
  } catch (error) {
    const code = error instanceof ApiError ? (error.code ?? null) : null;
    // 这个端点的顶层键是 `msg` 而不是 `message`，引擎取不到就会回落到 "HTTP 200"——
    // 所以直接读 details 里的原文，别让真实原因丢掉。
    const envelopeMessage = asRecord(
      error instanceof ApiError ? error.details : undefined,
    )?.msg;
    const message =
      typeof envelopeMessage === "string" && envelopeMessage
        ? envelopeMessage
        : error instanceof Error
          ? error.message
          : "预检请求失败";
    return {
      status: "failed",
      code,
      message,
      // 结论与指引都按码给：40401 / 40301 / 50002 这几种里 Token 其实是对的，
      // 用一句泛化的「Token 没通过验证」盖住会把排查方向带偏。
      verdict: approvalCodeVerdict(code),
      hint: approvalCodeHint(code),
    };
  }
}

/* --------------------------------- hooks --------------------------------- */

/// 数据源列表：目录里没有读权限时不发请求（省一次注定 403 的往返）。
export function useDatasourceList(
  query: DatasourceListQuery,
): UseQueryResult<ListPage<DatasourceItem>> {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;
  return useQuery({
    enabled: canReadDatasources(catalogData),
    queryKey: feishuQueryKeys.datasourceList(query),
    queryFn: ({ signal }) =>
      listDatasources(query, { catalog: catalogData, session }, signal),
    // 翻页/搜索时保留上一页结果，避免整页闪空
    placeholderData: keepPreviousData,
    staleTime: 15_000,
  });
}

/// 某个数据源下的选项（只读）。缺 `feishu.option.read` 时不发请求，页面渲染 403 说明。
export function useOptionList(
  query: OptionListQuery,
): UseQueryResult<ListPage<OptionItem>> {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;
  return useQuery({
    enabled: canReadOptions(catalogData),
    queryKey: feishuQueryKeys.optionList(query),
    queryFn: ({ signal }) =>
      listOptions(query, { catalog: catalogData, session }, signal),
    placeholderData: keepPreviousData,
    staleTime: 15_000,
  });
}

/// 页面用的一组「已绑定目录与会话」的写操作入口。
/// 变更函数本身不缓存失效——`update` 只回 `{affected}`，页面提交后必须回读列表。
export type FeishuActions = {
  canRead: boolean;
  canWrite: boolean;
  canReadOptions: boolean;
  createDatasource: (
    input: CreateDatasourceInput,
  ) => Promise<{ sourceKey: string }>;
  updateDatasource: (
    input: UpdateDatasourceInput,
  ) => Promise<{ affected: number }>;
  deleteDatasource: (
    sourceKey: string,
  ) => Promise<{ deleted: number; disabledOptions: number }>;
  precheckToken: (
    sourceKey: string,
    token: string,
  ) => Promise<TokenPrecheckResult>;
};

export function useFeishuActions(): FeishuActions {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;

  const deps = useMemo<FeishuInvokeDeps>(
    () => ({ catalog: catalogData, session }),
    [catalogData, session],
  );

  const create = useCallback(
    (input: CreateDatasourceInput) => createDatasource(input, deps),
    [deps],
  );
  const update = useCallback(
    (input: UpdateDatasourceInput) => updateDatasource(input, deps),
    [deps],
  );
  const remove = useCallback(
    (sourceKey: string) => deleteDatasource(sourceKey, deps),
    [deps],
  );
  const precheck = useCallback(
    (sourceKey: string, token: string) =>
      precheckApprovalOptions(sourceKey, token, deps),
    [deps],
  );

  return {
    canRead: canReadDatasources(catalogData),
    canWrite: canWriteDatasources(catalogData),
    canReadOptions: canReadOptions(catalogData),
    createDatasource: create,
    updateDatasource: update,
    deleteDatasource: remove,
    precheckToken: precheck,
  };
}

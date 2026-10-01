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
  hasOperation,
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
  ApprovalConfigItem,
  ApprovalConfigMap,
  ApprovalRequestItem,
  ApprovalRequestsQuery,
  ApprovalTaskItem,
  ApprovalWidget,
  BitableField,
  BitableTable,
  BitableView,
  DatasourceFieldBinding,
  DatasourceItem,
  DatasourceListQuery,
  DatasourceStatusFilter,
  HealthReport,
  IngestMode,
  ListPage,
  OptionItem,
  OptionListQuery,
  OrderByClause,
  PullScheduleInfo,
  TableWizardField,
  TokenPrecheckResult,
} from "./types";
import { approvalCodeHint, approvalCodeVerdict } from "./types";

/// 控制台要用的数据源 Action。
///
/// **每一粒都必须是服务端真实注册过的 `operation_id`**（逐个对着
/// `src/addon/feishu/datasource/actions/*.rs` 的 `action_name!(...)` 核过）。
/// 写一个不存在的 id 的后果不是报错而是**静默**：`hasOperation` 恒为 false，
/// 于是整个写侧（「添加数据源」与行内菜单）永远不渲染——界面上看不出任何异常。
///
/// 三个写操作（建 / 改 / 删）都是**表级**的：字段级的 `create_datasource` /
/// `update_datasource` / `delete_datasource` 已在「退役字段级可写入口」那一次重构里删掉，
/// 服务端目录里再也不会出现它们。
///
/// `pullNow` / `pullSchedule` 只在服务端 `can_pull()` 为真（出站凭证齐备）时才注册，
/// 所以目录里查不到它们是**正常状态**，不是权限问题——页面要按「没有」处理。
export const DATASOURCE_OPERATION_IDS = {
  list: "feishu.datasource.list_datasources",
  create: "feishu.datasource.create_datasource_table",
  update: "feishu.datasource.update_datasource_table",
  remove: "feishu.datasource.delete_datasource_table",
  pullNow: "feishu.datasource.pull_now",
  pullSchedule: "feishu.datasource.pull_schedule",
} as const;

export const OPTION_OPERATION_IDS = {
  list: "feishu.option.list_options",
  approvalOptions: "feishu.option.approval_options",
} as const;

/// 审批派发控制台的 7 个 Action。
///
/// **每一粒都必须是服务端真实注册过的 `operation_id`**（逐个对着
/// `src/addon/feishu/approval/actions/*.rs` 的 `action_name!(...)` 核过）。
/// 服务端 `action_name!` 的展开形态是「模块名 + 点 + 动作名」，七个动作的模块名
/// 都是 `feishu.approval`（注册在 `approval/mod.rs` 的 `build_module`）。
/// 与数据源 Action 不同，它们**无条件注册**（不挂 `can_pull()` 门禁）：控制台
/// 不依赖飞书凭证存在，`create_config` / `list_widgets` 运行时按其自身逻辑校验。
export const APPROVAL_OPERATION_IDS = {
  listConfigs: "feishu.approval.list_configs",
  createConfig: "feishu.approval.create_config",
  updateConfig: "feishu.approval.update_config",
  deleteConfig: "feishu.approval.delete_config",
  listWidgets: "feishu.approval.list_widgets",
  listRequests: "feishu.approval.list_requests",
  listTasks: "feishu.approval.list_tasks",
} as const;

/// 表级配置（T3/T4/T5）与凭据生命周期（T11/T12）的 Action。
///
/// 这些端点在服务端都在**表级**这一侧：元数据三个是出站查飞书（归 `datasource.write`
/// 一侧，见 `list_bitable_tables.rs` 的注释），创建是一次写两张表的事务。
export const TABLE_OPERATION_IDS = {
  listTables: "feishu.datasource.list_bitable_tables",
  listViews: "feishu.datasource.list_bitable_views",
  listFields: "feishu.datasource.list_bitable_fields",
  createTable: "feishu.datasource.create_datasource_table",
  /// 体检：勾选的 `field_id` 与表实际字段比对（读 + 出站，不改任何状态）。
  healthCheck: "feishu.datasource.health_check",
  /// 回显 Token：**纯读**，但读的是凭据，所以是独立权限位。
  reveal: "feishu.datasource.reveal_token",
  /// 轮换 Token：写，事务内换 hash + cipher + 轮换时间。
  rotate: "feishu.datasource.rotate_token",
} as const;

/// xlsx 文件导入的三个 Action。
///
/// 三个都**不出网**，服务端因此没有给它们 `settings.can_pull()` 门控：没有飞书凭证的
/// 环境里它们照样注册、照样可用（这正是文件导入存在的理由）。
export const XLSX_OPERATION_IDS = {
  /// 只读表头：上传文件 → 回列名。不写库。
  probe: "feishu.datasource.probe_xlsx_headers",
  /// 导数据：上传同一批文件 → 建选项。写。
  importFiles: "feishu.datasource.import_xlsx",
  /// 查导入进度：只读**进程内**状态，不写库、不碰飞书。
  ///
  /// 鉴权**与 `importFiles` 不是同一粒**：导入端点是 `feishu.datasource.write`
  /// （`import_xlsx.rs` 的 `.permissions`），本端点是 `feishu.datasource.read`。
  /// 两个方向都要知道：只读身份看得见进度、看不见导入入口；而**只有 write 没有 read**
  /// 的身份能导入、查进度却会 403——`useImportProgress` 把失败一律吞掉，那一侧的进度行
  /// 就会静默停在「正在上传文件…」这种不确定态，不报错也不前进。
  importProgress: "feishu.datasource.get_import_progress",
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
 *
 * 实现已下沉到 `engine/catalog/has-operation.ts`：它不是飞书域的私有判据，
 * 权限组管理面同样要用。这里只是转出，签名与语义保持不变。
 */
export { hasOperation };

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

/// 能否看审批派发控制台（侧边栏两条 NavLink 的渲染条件）。
export function canReadApproval(catalog: UiCatalog | undefined): boolean {
  return hasOperation(catalog, APPROVAL_OPERATION_IDS.listConfigs);
}

/// 能否操作审批派发配置（启停/删除/新建向导的渲染条件）。
export function canWriteApproval(catalog: UiCatalog | undefined): boolean {
  return hasOperation(catalog, APPROVAL_OPERATION_IDS.createConfig);
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
        // 详情页靠它按主键取单条。**它必须进 key**：详情页除 `id` 外所有入参都固定
        // （page=1 / pageSize=1 / search="" / status="all"），漏了它两条数据源就共用
        // 同一个缓存条目——15s 的 `staleTime` 内从 #7 跳到 #8 不会发请求，页面直接
        // 渲染上一条的标题、字段绑定、凭据清单（URL/回显/轮换的目标字段）与拉取目标。
        // 请求体是对的，坏在缓存不认这个入参。
        id: query.id ?? null,
      },
    ] as const,
  options: (sourceKey: string) =>
    [FEISHU_QUERY_ROOT, "options", sourceKey] as const,
  /// 排程是**全局**的，所以 key 里没有 sourceKey。
  pullSchedule: () => [FEISHU_QUERY_ROOT, "pull-schedule"] as const,
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
  approvalConfigs: () => [FEISHU_QUERY_ROOT, "approval-configs"] as const,
  approvalConfigList: (query: {
    page: number;
    pageSize: number;
    search: string;
  }) =>
    [
      ...feishuQueryKeys.approvalConfigs(),
      // 入参必须进 key：漏掉 search，react-query 15s staleTime 内搜索只改闭包
      // 不改 key，缓存命中旧的全量列表，用户感知为搜索失效（同 datasourceList 的教训）。
      {
        page: query.page,
        pageSize: query.pageSize,
        search: query.search.trim(),
      },
    ] as const,
  approvalRequests: () => [FEISHU_QUERY_ROOT, "approval-requests"] as const,
  approvalRequestList: (query: ApprovalRequestsQuery) =>
    [
      ...feishuQueryKeys.approvalRequests(),
      {
        page: query.page,
        pageSize: query.pageSize,
        baseToken: query.baseToken.trim(),
        tableId: query.tableId.trim(),
        outcome: query.outcome,
        orderBy: withStableOrder(query.orderBy),
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

/// 表级主键等值条件。详情页用它取「就是这一条」——`id` 已声明 `filterable`。
///
/// **不能改用 `search`**：服务端的检索只覆盖 `searchable` 的列，而表级行上只有
/// `title` 可搜（`source_key` 已经不在表级行上了，它属于字段绑定）。拿一个
/// `source_key` 去 search 会命中零行，而「零行」会被页面读成「这条数据源不存在」。
function idWhere(id: number | undefined): Record<string, unknown> | undefined {
  return id === undefined ? undefined : { type: "eq", field: "id", value: id };
}

/// 把若干条件折成一棵树：无 → `undefined`（不带 `where`），一条 → 它本身，
/// 多条 → `and` 组。DSL 的 where 树只有 `and`/`or` 组节点，**没有隐式合取**，
/// 所以「id 且 状态」必须显式包一层。
function allWhere(
  clauses: Array<Record<string, unknown> | undefined>,
): Record<string, unknown> | undefined {
  const kept = clauses.filter(
    (clause): clause is Record<string, unknown> => clause !== undefined,
  );
  if (kept.length === 0) return undefined;
  if (kept.length === 1) return kept[0];
  return { type: "and", conditions: kept };
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
    where: allWhere([idWhere(query.id), statusWhere(query.status)]),
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

/// `list_datasources` 的**前端一半契约**：后端键 ↔ 它落到的前端字段。
///
/// 对账方是 `frontend/contracts/feishu-projections.json`（后端 `list_datasources.rs`
/// 的 `the_committed_contract_*` 读同一份文件），两个方向都堵死：
///   · 键少了 → 后端发了没人读（死投影）；
///   · 键多了 → 前端读一个后端不发的键 ⇒ 恒得 `null`，而界面会把它当成
///     「服务端说没有」，画出一句可查证的假话（`linkage_mapping` 就是这么活了很久的）。
///
/// `satisfies` 让编译器再保一层：值必须是真实的字段名，改错字段名编译不过。
export const DATASOURCE_ITEM_KEYS = {
  id: "id",
  title: "title",
  status: "status",
  updated_at: "updatedAt",
  ingest_mode: "ingestMode",
  bitable_base_token: "bitableBaseToken",
  bitable_table_id: "bitableTableId",
  bitable_view_id: "bitableViewId",
  last_pull_at: "lastPullAt",
  last_success_at: "lastSuccessAt",
  consecutive_failures: "consecutiveFailures",
  last_error: "lastError",
  fields: "fields",
} as const satisfies Record<string, keyof DatasourceItem>;

/// 选项行那一半（`list_options` 的 `items[]`）。
export const OPTION_ITEM_KEYS = {
  option_id: "optionId",
  source_key: "sourceKey",
  label: "label",
  i18n: "i18n",
  sort_order: "sortOrder",
  is_default: "isDefault",
  enabled: "enabled",
  parent_key: "parentKey",
  last_push_at: "lastPushAt",
  updated_at: "updatedAt",
} as const satisfies Record<string, keyof OptionItem>;

/// 体检报告那一半（`health_check`）。
export const HEALTH_REPORT_KEYS = {
  ok: "ok",
  missing_fields: "missingFields",
  view_missing: "viewMissing",
  table_missing: "tableMissing",
  unchecked: "unchecked",
} as const satisfies Record<string, keyof HealthReport>;

/// 体检报告里嵌套的那一项（`missing_fields[]`）——**另一个形状**，单独对账。
export const HEALTH_MISSING_FIELD_KEYS = {
  field_id: "fieldId",
  source_key: "sourceKey",
} as const satisfies Record<
  string,
  keyof HealthReport["missingFields"][number]
>;

/// 自动拉取排程那一半（`pull_schedule`）。
export const PULL_SCHEDULE_KEYS = {
  interval_seconds: "intervalSeconds",
  next_run_at: "nextRunAt",
} as const satisfies Record<string, keyof PullScheduleInfo>;

/// 字段绑定那一半。`datasource_id` **不在这里**：它属于契约的 `query_only`
/// （后端用它把绑定分组到各条数据源上，分组之后就不 emit 了）。
export const FIELD_BINDING_KEYS = {
  field_id: "fieldId",
  field_name: "fieldName",
  source_key: "sourceKey",
  parent_field_id: "parentFieldId",
  enabled: "enabled",
  encrypt_enabled: "encryptEnabled",
  default_locale: "defaultLocale",
  token_rotated_at: "tokenRotatedAt",
} as const satisfies Record<string, keyof DatasourceFieldBinding>;

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

/// 字段绑定行（`list_datasources` 的 `fields[]`）。
///
/// 表级化之后 `source_key` / 凭据 / 父指针都在这一层：一条表级行有 N 条绑定。
function parseFieldBinding(
  raw: Record<string, unknown>,
): DatasourceFieldBinding | null {
  const sourceKey = asString(raw.source_key);
  const fieldId = asString(raw.field_id);
  // 没有 source_key 或 field_id 的绑定行无法定位（一个进 URL、一个是身份）
  if (sourceKey === "" || fieldId === "") return null;
  return {
    fieldId,
    fieldName: asNullableString(raw.field_name),
    sourceKey,
    parentFieldId: asNullableString(raw.parent_field_id),
    // `enabled` 缺失时按启用处理：那是存量行（列默认 true）的语义
    enabled: raw.enabled !== false,
    // 加密返回与默认语言是**绑定级**的：一条数据源有 N 个字段，可以各自加密、
    // 各自语言。表级行上曾经也有这两个名字，那是字段级时代的残影——读它只会
    // 恒得 `false` 与 `""`，于是列表上那两列对每一条源都在说同一句假话。
    // 兜底值刻意**等于列默认值**（`false` / `zh_cn`）：万一投影漏了这一列，
    // 画面上的值仍与库里的实际值一致，不会凭空多出一句「未加密 / 空语言徽标」。
    encryptEnabled: raw.encrypt_enabled === true,
    defaultLocale: asString(raw.default_locale, "zh_cn"),
    tokenRotatedAt: rotatedAtOf(raw),
  };
}

/// 绑定投影里的 `token_rotated_at`，**三态**读法。
///
/// - 键不在：`undefined` = 拿不到（服务端还没投影这一列）；
/// - 键是 `null`：明确知道从未轮换过；
/// - 数字：那次轮换的时间。
///
/// 不能折成「有值 / 没有值」两态：把「拿不到」画成「从未轮换」是一句可查证的
/// 假话，而这张清单是运维判断「该控件配的凭据是不是刚换过」的唯一依据。
function rotatedAtOf(raw: Record<string, unknown>): number | null | undefined {
  if (!("token_rotated_at" in raw)) return undefined;
  const value = raw.token_rotated_at;
  if (value === null) return null;
  // 键在、值却不是数字也不是 null：形状认不出来，按「拿不到」处理，
  // 不要用一个解析失败的残值去冒充时间。
  return typeof value === "number" && Number.isFinite(value)
    ? value
    : undefined;
}

function parseDatasourceItem(
  raw: Record<string, unknown>,
): DatasourceItem | null {
  // 表级化之后**表级行上没有 `source_key`**（它在字段绑定上），身份就是主键 `id`。
  //
  // 这里曾经还接受「只有 `source_key` 的旧形状」，理由是「那条路径还有消费者」。
  // 那些消费者（详情页按 source_key 找数据源、`identityLabel` 的兜底）都已经改成
  // 按 `id`/按绑定定位了，而服务端**从表级化起就不再发这个键**。于是这段容忍
  // 只剩下一个作用：在类型里保留一条已被否决的身份，让下一个消费方还能拿到它。
  // 缺 `id` 的行现在直接丢掉——不编，也不猜。
  const id = typeof raw.id === "number" ? raw.id : null;
  if (id === null) return null;
  const rawFields = raw.fields;
  const fields = Array.isArray(rawFields)
    ? rawFields
        .map((item) => asRecord(item))
        .filter((item): item is Record<string, unknown> => item !== undefined)
        .map(parseFieldBinding)
        .filter((item): item is DatasourceFieldBinding => item !== null)
    : [];
  return {
    id,
    fields,
    title: asString(raw.title),
    // `encrypt_enabled` / `default_locale` 曾经在这里解析——两者都是**绑定级**属性，
    // 表级投影里从来没有它们（见 `FieldBindingItem`）。现在归 `parseFieldBinding`。
    status: raw.status === "disabled" ? "disabled" : "active",
    updatedAt: asNumber(raw.updated_at, 0),
    ingestMode: asString(raw.ingest_mode),
    bitableBaseToken: asNullableString(raw.bitable_base_token),
    bitableTableId: asNullableString(raw.bitable_table_id),
    bitableViewId: asNullableString(raw.bitable_view_id),
    // `bitable_field_name` 与 `linkage_mapping` 曾经在这里解析——两者都已不在表级行上
    // （取数列归绑定层；级联映射被逐字段的 `parent_field_id` 取代）。
    // 后者还活了很久：注释都写对了「都已不在表级行上」，代码却还在读它、恒得 null。
    // 现在由 `DATASOURCE_ITEM_KEYS` 与契约文件对账，那种「注释对、代码不对」不会再悄悄存在。
    lastPullAt: asNullableNumber(raw.last_pull_at),
    lastSuccessAt: asNullableNumber(raw.last_success_at),
    consecutiveFailures: asNumber(raw.consecutive_failures, 0),
    lastError: asNullableString(raw.last_error),
    // `snapshot_digest` 不再解析：表级那一列是**保留列**（谁都不写），摘要归属在字段绑定。
    // 它曾经被解析进 `snapshotDigest` 并文档成「内容摘要」——而那永远是 null。
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

/// 一条字段绑定在**写**方向的形状（后端 `FieldBindingInput`）。
export type FieldBindingInput = {
  fieldId: string;
  /// 列名。**审批外部选项装配按它配对**——名字为空的绑定会被
  /// `approval_provision` 跳过（只 warn），于是某个控件静默少一个候选列。
  /// 拿不到时为 `null`（xlsx 导入的绑定与多维表格的字段都有名字，但投影是
  /// `Option`，缺了不能编）。
  fieldName: string | null;
  sourceKey: string;
  parentFieldId: string | null;
};

/// 表级行的绑定投影 → 写的形状（**只送启用中的那几条**）。
///
/// 服务端对「出现在集合里」的已有绑定会顺手写上 `enabled = true`：把一条已经停用的
/// 绑定塞回去，等于悄悄把它重新启用——那不是用户在「改名称」时想做的事。
/// 而集合里没有的行只会被停用（幂等），所以省略它们是安全的。
export function enabledBindingInputs(item: {
  fields: DatasourceFieldBinding[];
}): FieldBindingInput[] {
  return item.fields
    .filter((binding) => binding.enabled)
    .map((binding) => ({
      fieldId: binding.fieldId,
      fieldName: binding.fieldName,
      sourceKey: binding.sourceKey,
      parentFieldId: binding.parentFieldId,
    }));
}

export type UpdateTableInput = {
  datasourceId: number;
  /// 省略即不改。**空串会被后端拒**（名称必须在 1..=100 字符）。
  title?: string;
  /// 期望的字段绑定集合（**整份替换**）。后端要求至少一条，且集合里没有的
  /// 已有绑定会被停用——所以这里送的恒是「当前启用中的那一份」（见
  /// [`enabledBindingInputs`]）。
  fields: FieldBindingInput[];
};

/// 更新一条表级数据源。回执三个计数回答「这次改动动了哪些绑定」。
///
/// 入参是**表级主键**：字段级那个按 `source_key` 定位的更新入口已经退役。
export async function updateDatasourceTable(
  input: UpdateTableInput,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<{ inserted: number; updated: number; disabled: number }> {
  const body: Record<string, unknown> = {
    datasource_id: input.datasourceId,
    fields: input.fields.map((field) => {
      const entry: Record<string, unknown> = {
        field_id: field.fieldId,
        source_key: field.sourceKey,
        parent_field_id: field.parentFieldId,
      };
      // **新增**绑定时后端才写这个列（已有绑定刻意不动它——列名与 `source_key`、
      // 凭据同属身份信息，改了要重走配置），而留空会让审批装配跳过这条绑定。
      // 与建源那条路同一个理由（见 `createDatasourceTable`）。后端是「非空才写」，
      // 所以拿不到名字时**整个键不出现**（空串与缺键同义，但缺键更清楚）。
      const name = field.fieldName?.trim() ?? "";
      if (name !== "") entry.field_name = name;
      return entry;
    }),
  };
  if (input.title !== undefined) body.title = input.title;

  const result = await invokeFeishuAction(
    deps,
    DATASOURCE_OPERATION_IDS.update,
    body,
    signal,
  );
  const data = asRecord(result.data);
  return {
    inserted: asNumber(data?.inserted, 0),
    updated: asNumber(data?.updated, 0),
    disabled: asNumber(data?.disabled, 0),
  };
}

/// 删除一条表级数据源：连带删掉它的全部字段绑定，其下选项只停用不删除
/// （`disabled_options` 就是那个计数）。
export async function deleteDatasourceTable(
  datasourceId: number,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<{ deletedFields: number; disabledOptions: number }> {
  const result = await invokeFeishuAction(
    deps,
    DATASOURCE_OPERATION_IDS.remove,
    { datasource_id: datasourceId },
    signal,
  );
  const data = asRecord(result.data);
  return {
    deletedFields: asNumber(data?.deleted_fields, 0),
    disabledOptions: asNumber(data?.disabled_options, 0),
  };
}

/* ---------------------------- 表级配置：元数据 ---------------------------- */

function parseBitableTable(raw: Record<string, unknown>): BitableTable | null {
  const tableId = asString(raw.table_id);
  // 没有 table_id 的行不可用：它是下一步的坐标，猜不出来
  return tableId === "" ? null : { tableId, name: asString(raw.name) };
}

function parseBitableView(raw: Record<string, unknown>): BitableView | null {
  const viewId = asString(raw.view_id);
  return viewId === ""
    ? null
    : {
        viewId,
        viewName: asString(raw.view_name),
        viewType: asString(raw.view_type),
      };
}

function parseBitableField(raw: Record<string, unknown>): BitableField | null {
  const fieldId = asString(raw.field_id);
  if (fieldId === "") return null;
  return {
    fieldId,
    fieldName: asString(raw.field_name),
    type: asNumber(raw.type, 0),
  };
}

function listOf<T>(
  data: unknown,
  key: string,
  parse: (raw: Record<string, unknown>) => T | null,
): T[] {
  const raw = asRecord(data)?.[key];
  if (!Array.isArray(raw)) return [];
  return raw
    .map((item) => asRecord(item))
    .filter((item): item is Record<string, unknown> => item !== undefined)
    .map(parse)
    .filter((item): item is T => item !== null);
}

/// 列出某个多维表格 App 下的数据表（向导第一步）。
///
/// 它**会出站调飞书**并消耗本应用频控配额，所以服务端把三粒元数据权限都归到
/// `feishu.datasource.write` 一侧——目录里没有它时这里会走 `requireAction` 抛错。
export async function listBitableTables(
  appToken: string,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<BitableTable[]> {
  const result = await invokeFeishuAction(
    deps,
    TABLE_OPERATION_IDS.listTables,
    { app_token: appToken },
    signal,
  );
  return listOf(result.data, "tables", parseBitableTable);
}

/// 列出数据表的视图（向导第二步）。**它决定拉取哪些行，不决定能勾哪些字段。**
export async function listBitableViews(
  appToken: string,
  tableId: string,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<BitableView[]> {
  const result = await invokeFeishuAction(
    deps,
    TABLE_OPERATION_IDS.listViews,
    { app_token: appToken, table_id: tableId },
    signal,
  );
  return listOf(result.data, "views", parseBitableView);
}

/// 列出数据表的字段（向导第三步）。
///
/// **一个 `view_id` 都不发**：实测「列出字段」的 `view_id` 参数不生效（带与不带返回
/// 完全相同的字段集合与顺序）。加回去只会让人以为「换视图能换出一批字段」。
export async function listBitableFields(
  appToken: string,
  tableId: string,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<BitableField[]> {
  const result = await invokeFeishuAction(
    deps,
    TABLE_OPERATION_IDS.listFields,
    { app_token: appToken, table_id: tableId },
    signal,
  );
  return listOf(result.data, "fields", parseBitableField);
}

/* ---------------------------- 表级配置：创建 ----------------------------- */

/// 一次「建表级数据源」的完整提交物。**两个向导共用**：
///
/// - 多维表格向导：给全部三项坐标，`ingestMode` 省略（= `pull`）；
/// - xlsx 向导：**三项坐标都不给**（给了会被服务端当成多维表格坐标去解析，而这张表
///   在飞书侧根本不存在），`ingestMode` 给 `xlsx_import`。
export type CreateTableSubmission = {
  title: string;
  /// 取数方式。省略 = `pull`（多维表格向导那条路的老行为，由服务端按间隔拉）。
  ingestMode?: IngestMode;
  appToken?: string;
  tableId?: string;
  /// 空串 = 取全表。**不发这个键**（发空串会被后端当成「有一个空坐标」）。
  viewId?: string;
  fields: TableWizardField[];
};

/// 创建时系统生成的凭据。明文只在这一刻与回显端点上出现。
export type CreatedCredential = {
  fieldId: string;
  sourceKey: string;
  token: string;
};

export type CreatedTable = {
  datasourceId: number;
  credentials: CreatedCredential[];
};

/// 建表级数据源：表级行 + N 条字段绑定，服务端**同一事务**写入。
export async function createDatasourceTable(
  input: CreateTableSubmission,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<CreatedTable> {
  const body: Record<string, unknown> = {
    title: input.title,
    // 由调用方决定：多维表格向导用 `pull`，xlsx 向导用 `xlsx_import`。
    ingest_mode: input.ingestMode ?? "pull",
    fields: input.fields.map((field) => ({
      field_id: field.fieldId,
      // xlsx 绑定必须带列名：列名就是这条绑定的身份（`field_id` = `field_name` = 列名），
      // 而绑定行上的 `field_name` 是**展示名**——审批外部选项装配按它给出候选列。
      // 留空（或只有空白）时服务端**宽容跳过**这条绑定（`binding_display_name` 返回
      // `None` + 告警，不报错），代价是那个控件**少一个候选列**；范围不是全表，
      // 也不会让整批装配失败。（这条注释曾经说反了：Task 8 之前确实是 `require`。）
      // 多维表格那条路给了也无害——`pull` 每轮会按解析结果回写覆盖它。
      field_name: field.fieldName,
      source_key: field.sourceKey,
      // 显式 `null`（后端 `#[serde(default)] Option<String>`，两者同义，但写出来更清楚）
      parent_field_id: field.parentFieldId,
    })),
  };
  // 多维表格专用坐标：xlsx 源三项都不给（给了反而会被解析成坐标）。
  if (input.appToken !== undefined) body.bitable_base_token = input.appToken;
  if (input.tableId !== undefined) body.bitable_table_id = input.tableId;
  if (input.viewId !== undefined && input.viewId.trim() !== "") {
    body.bitable_view_id = input.viewId.trim();
  }

  const result = await invokeFeishuAction(
    deps,
    TABLE_OPERATION_IDS.createTable,
    body,
    signal,
  );
  const data = asRecord(result.data);
  const rawCredentials = data?.credentials;
  const credentials: CreatedCredential[] = Array.isArray(rawCredentials)
    ? rawCredentials
        .map((item) => asRecord(item))
        .filter((item): item is Record<string, unknown> => item !== undefined)
        .map((item) => ({
          fieldId: asString(item.field_id),
          sourceKey: asString(item.source_key),
          token: asString(item.token),
        }))
        .filter((item) => item.sourceKey !== "")
    : [];
  return { datasourceId: asNumber(data?.datasource_id, 0), credentials };
}

/// 向导要用的一组数据访问入口。
///
/// 抽成一个对象是为了**可注入**：组件不直接摸目录与会话，也就没有「渲染它必须先把
/// 整棵应用壳搭起来」这个前提。真实实现见 [`useTableWizardClient`]。
export type TableWizardClient = {
  listTables: (appToken: string) => Promise<BitableTable[]>;
  listViews: (appToken: string, tableId: string) => Promise<BitableView[]>;
  listFields: (appToken: string, tableId: string) => Promise<BitableField[]>;
  createTable: (input: CreateTableSubmission) => Promise<CreatedTable>;
};

/// 绑定当前会话与界面目录的向导数据访问。
///
/// **一律走目录声明的 Action**：`requireAction` 对未知 operation_id 抛错，所以
/// 「服务端没部署这个端点」会表现为一句明确的错误，而不是一个打到空路径的请求。
export function useTableWizardClient(): TableWizardClient {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;

  const deps = useMemo<FeishuInvokeDeps>(
    () => ({ catalog: catalogData, session }),
    [catalogData, session],
  );

  return useMemo(
    () => ({
      listTables: (appToken: string) => listBitableTables(appToken, deps),
      listViews: (appToken: string, tableId: string) =>
        listBitableViews(appToken, tableId, deps),
      listFields: (appToken: string, tableId: string) =>
        listBitableFields(appToken, tableId, deps),
      createTable: (input: CreateTableSubmission) =>
        createDatasourceTable(input, deps),
    }),
    [deps],
  );
}

/* ------------------------------ xlsx 文件导入 ----------------------------- */

/// 探表头的回执（`probe_xlsx_headers`）。**只读表头**，不含任何数据行。
export type XlsxHeaderProbe = {
  /// 读了哪张 sheet。**只读第一张**——`sheets` 多于一项时由界面提示这件事。
  sheetName: string;
  /// 文件里所有 sheet 的名字。
  sheets: string[];
  /// 表头在第几行（1-based）。它也是导入时跳掉表头行的依据。
  headerRow: number;
  /// **全部列**（服务端不做任何类型/语义过滤），`index` 是 1-based 物理列号。
  columns: Array<{ name: string; index: number }>;
  /// 这次探测的文件，按上传顺序。
  files: Array<{ name: string }>;
};

/// 导入回执里的一条绑定（`import_xlsx` 的 `bindings[]`）。
///
/// `snapshot_digest` **刻意不解析**：它是服务端拿来判断「这一轮要不要写库」的内部
/// 状态，界面上没有一句话要用它，读进来只会多一个没人看的字段。
export type XlsxBindingReport = {
  sourceKey: string;
  /// 这次读到的行数。口径是「在**被请求的列**里至少有一个非空」的行——
  /// 它**不是文件的总行数**（没勾的列与整行全空的都被丢掉了）。
  fetched: number;
  /// 派生出的选项数。
  derived: number;
  /// 这一轮被补集停用的选项数。
  disabled: number;
  /// **非 null = 这一轮压根没扫补集**，值是当时已启用的选项数。
  ///
  /// 单轮补集停用的上限是 2 万条，而银行网点那份实测文件有 15 万多条选项：每一轮都走
  /// 这一支。没有它，`disabled: 0` 在「扫完了没有要停用的」与「根本没扫」之间没有区别
  /// ——而后者意味着改过名/删掉的旧选项**仍然启用着**，继续被出站喂给飞书控件。
  complementSkipped: number | null;
  /// 内容没变、也没有已停用行要复活 → 这一轮什么都没写。
  unchanged: boolean;
  /// 非空 = 这一轮**跳过**了这条绑定（它派生出 0 个选项而本地仍有启用选项），
  /// 原因在这里。跳过意味着那条绑定的选项**一行没动**。
  skippedReason: string | null;
  /// 异常行逐条（如文案超长被截断，那行不入飞书但仍在库里）。
  anomalies: Array<{ label: string; reason: string }>;
  /// 异常清单被服务端上限截断过，真实条数看不到。
  truncatedDetails: boolean;
};

/// 导入回执（`import_xlsx`）。
export type XlsxImportReport = {
  datasourceId: number;
  elapsedMs: number;
  files: Array<{ name: string; rowsRead: number }>;
  bindings: XlsxBindingReport[];
};

/// 导入阶段（`get_import_progress`）。**这就是后端的线格式取值**：认不出的取值一律
/// 折成 `idle`（见 [`parseImportProgress`]），前端不猜新阶段。
export type ImportStage = "idle" | "loading" | "parsing" | "writing";

/// 一次导入的进度快照。**每一项都是可空的**，因为各阶段的字段集不同：
/// `file_index` / `rows_*` 只在解析期有值，`bindings_*` 只在写入期有值。
///
/// 两条口径**刻意不同**、不许「顺手统一」（[`describeImportProgress`] 的文案按它写）：
/// - `rowsDone` 是**物理**行序号（含空行），回执里的 `rowsRead` 是**非空**数据行数；
/// - `bindingsDone` 是「已开始处理」的绑定数，所以它**到不了** `bindingsTotal`。
export type ImportProgress = {
  stage: ImportStage;
  filesTotal: number | null;
  filesDone: number | null;
  fileIndex: number | null;
  rowsDone: number | null;
  rowsTotal: number | null;
  bindingsTotal: number | null;
  bindingsDone: number | null;
};

/// 导入进度的**前端一半契约**（后端 `assert_keys` 读的是同一份
/// `frontend/contracts/feishu-projections.json`）。键一个不多一个不少：多一个就是读
/// 一个后端不发的键（恒 `null`，界面会把它当成「服务端说没有」），少一个就是死投影。
export const IMPORT_PROGRESS_KEYS = {
  stage: "stage",
  files_total: "filesTotal",
  files_done: "filesDone",
  file_index: "fileIndex",
  rows_done: "rowsDone",
  rows_total: "rowsTotal",
  bindings_total: "bindingsTotal",
  bindings_done: "bindingsDone",
} as const satisfies Record<string, keyof ImportProgress>;

function parseXlsxHeaderProbe(data: unknown): XlsxHeaderProbe {
  const record = asRecord(data);
  const rawColumns = record?.columns;
  const rawFiles = record?.files;
  return {
    sheetName: asString(record?.sheet_name),
    sheets: Array.isArray(record?.sheets)
      ? record.sheets.filter((item): item is string => typeof item === "string")
      : [],
    // 缺 `header_row` 时兜成 1 而不是 0：导入要按它跳掉表头行，兜成 0 会把表头
    // 当成一行数据导进去。
    headerRow: asNumber(record?.header_row, 1),
    columns: Array.isArray(rawColumns)
      ? rawColumns
          .map((item) => asRecord(item))
          .filter((item): item is Record<string, unknown> => item !== undefined)
          .map((item) => ({
            name: asString(item.name),
            index: asNumber(item.index, 0),
          }))
          // 没有列名的列勾不了（列名就是这条绑定的身份），留着只会画一个空标签
          .filter((column) => column.name !== "")
      : [],
    files: Array.isArray(rawFiles)
      ? rawFiles
          .map((item) => asRecord(item))
          .filter((item): item is Record<string, unknown> => item !== undefined)
          .map((item) => ({ name: asString(item.name) }))
      : [],
  };
}

function parseXlsxBindingReport(
  raw: Record<string, unknown>,
): XlsxBindingReport {
  const rawAnomalies = raw.anomalies;
  return {
    sourceKey: asString(raw.source_key),
    fetched: asNumber(raw.fetched, 0),
    derived: asNumber(raw.derived, 0),
    disabled: asNumber(raw.disabled, 0),
    complementSkipped: asNullableNumber(raw.complement_skipped),
    unchanged: raw.unchanged === true,
    skippedReason: asNullableString(raw.skipped_reason),
    anomalies: Array.isArray(rawAnomalies)
      ? rawAnomalies
          .map((item) => asRecord(item))
          .filter((item): item is Record<string, unknown> => item !== undefined)
          .map((item) => ({
            label: asString(item.label),
            reason: asString(item.reason),
          }))
      : [],
    truncatedDetails: raw.truncated_details === true,
  };
}

function parseXlsxImportReport(data: unknown): XlsxImportReport {
  const record = asRecord(data);
  const rawFiles = record?.files;
  const rawBindings = record?.bindings;
  return {
    datasourceId: asNumber(record?.datasource_id, 0),
    elapsedMs: asNumber(record?.elapsed_ms, 0),
    files: Array.isArray(rawFiles)
      ? rawFiles
          .map((item) => asRecord(item))
          .filter((item): item is Record<string, unknown> => item !== undefined)
          .map((item) => ({
            name: asString(item.name),
            rowsRead: asNumber(item.rows_read, 0),
          }))
      : [],
    bindings: Array.isArray(rawBindings)
      ? rawBindings
          .map((item) => asRecord(item))
          .filter((item): item is Record<string, unknown> => item !== undefined)
          .map(parseXlsxBindingReport)
      : [],
  };
}

const IMPORT_STAGES: readonly ImportStage[] = [
  "idle",
  "loading",
  "parsing",
  "writing",
];

function isImportStage(value: unknown): value is ImportStage {
  return IMPORT_STAGES.some((stage) => stage === value);
}

/// 一个全 `null` 的 idle 快照。它是**兜底值**：认不出响应时交回它，界面据此显示
/// 不确定态（而不是画一个假的「0/0」进度）。
function idleImportProgress(): ImportProgress {
  return {
    stage: "idle",
    filesTotal: null,
    filesDone: null,
    fileIndex: null,
    rowsDone: null,
    rowsTotal: null,
    bindingsTotal: null,
    bindingsDone: null,
  };
}

/// 进度解析：**认不出的 stage 折成 idle + 全 null，前端不猜**。
///
/// 为什么不把未知 stage 当成「上一个已知阶段」：这是个每 1 秒打一次的旁路观测端点，
/// 服务端将来加一个阶段（如 `finalizing`）时，旧前端把它的字段按别的阶段读出来，
/// 画的就是一句可查证的假话；折成 idle 只丢了这一拍的精度，而文案本身就是不确定态。
function parseImportProgress(data: unknown): ImportProgress {
  const record = asRecord(data);
  const stage = record?.stage;
  if (!isImportStage(stage)) return idleImportProgress();
  return {
    stage,
    filesTotal: asNullableNumber(record?.files_total),
    filesDone: asNullableNumber(record?.files_done),
    fileIndex: asNullableNumber(record?.file_index),
    rowsDone: asNullableNumber(record?.rows_done),
    rowsTotal: asNullableNumber(record?.rows_total),
    bindingsTotal: asNullableNumber(record?.bindings_total),
    bindingsDone: asNullableNumber(record?.bindings_done),
  };
}

/// 探测一批 xlsx 文件的表头（向导第一步）。**不写库**。
///
/// `files` 必须是 values **顶层**的 `File[]`：引擎的 `appendMultipart` 只识别顶层
/// `File` 或「全是 File 的数组」，包成 `{ files: { items: [...] } }` 会被
/// `JSON.stringify` 成一个文本字段，服务端只会看到一段 JSON 字符串。
export async function probeXlsxHeaders(
  files: File[],
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<XlsxHeaderProbe> {
  const result = await invokeFeishuAction(
    deps,
    XLSX_OPERATION_IDS.probe,
    { files },
    signal,
  );
  return parseXlsxHeaderProbe(result.data);
}

/// 把一批 xlsx 导入某条数据源：整份快照替换它的选项。**写**。
///
/// 与 [`probeXlsxHeaders`] 一样，`files` 在 values 顶层。
export async function importXlsxFiles(
  datasourceId: number,
  files: File[],
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<XlsxImportReport> {
  const declared = requireAction(deps.catalog, XLSX_OPERATION_IDS.importFiles);
  // `datasource_id` 在**路径段**里，而服务端的 `ImportInput::params()` 是空集
  // （**文件**走 multipart body、id 走路径，两个来源各读各的），引擎的路径替换因此
  // 填不进去，会在最后一步抛「路径仍有未填写参数」——与 `approval_options` 同一条
  // 约束。这里显式补全并清空 params，让 `files` 落进 body。
  const action: ActionDemoSchema = {
    ...declared,
    path: declared.path.replace(
      "{datasource_id}",
      encodeURIComponent(String(datasourceId)),
    ),
    params: [],
  };
  const result = await invokeAction(action, { files }, deps.session, signal);
  return parseXlsxImportReport(result.data);
}

/// 查一条数据源**当前这一拍**的导入进度。**不写库**。
///
/// **不吃 [`importXlsxFiles`] 那个人工补路径的补丁**：本端点的 `params()` 声明了
/// `datasource_id` 是路径参数，引擎的路径替换因此填得进去（补了反而会因为
/// `params: []` 而把 id 当未知值塞进 body）。
///
/// 恒 200：数据源不存在、已删、本进程上没有导入在跑，三者都答 `idle`。
/// 服务端**不用 404** 表达「没在跑」——那会让前端把正常态读成故障。
export async function getImportProgress(
  datasourceId: number,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<ImportProgress> {
  const result = await invokeFeishuAction(
    deps,
    XLSX_OPERATION_IDS.importProgress,
    { datasource_id: datasourceId },
    signal,
  );
  return parseImportProgress(result.data);
}

/// xlsx 导入向导要用的一组数据访问入口（**与 [`TableWizardClient`] 同一注入范式**：
/// 组件只经注入的 client 访问数据，不摸 `useSessionCredentials` / `useUiCatalog`，
/// 于是渲染它不必先把整棵应用壳搭起来）。
export type XlsxImportClient = {
  probe: (files: File[]) => Promise<XlsxHeaderProbe>;
  createTable: (input: CreateTableSubmission) => Promise<CreatedTable>;
  importFiles: (
    datasourceId: number,
    files: File[],
  ) => Promise<XlsxImportReport>;
  importProgress: (datasourceId: number) => Promise<ImportProgress>;
};

/// 绑定当前会话与界面目录的 xlsx 导入入口。
export function useXlsxImportClient(): XlsxImportClient {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;

  const deps = useMemo<FeishuInvokeDeps>(
    () => ({ catalog: catalogData, session }),
    [catalogData, session],
  );

  return useMemo(
    () => ({
      probe: (files: File[]) => probeXlsxHeaders(files, deps),
      createTable: (input: CreateTableSubmission) =>
        createDatasourceTable(input, deps),
      importFiles: (datasourceId: number, files: File[]) =>
        importXlsxFiles(datasourceId, files, deps),
      importProgress: (datasourceId: number) =>
        getImportProgress(datasourceId, deps),
    }),
    [deps],
  );
}

/* ------------------------------ 体检与凭据 ------------------------------- */

function parseHealthReport(data: unknown): HealthReport {
  const record = asRecord(data);
  const rawMissing = record?.missing_fields;
  const missingFields = Array.isArray(rawMissing)
    ? rawMissing
        .map((item) => asRecord(item))
        .filter((item): item is Record<string, unknown> => item !== undefined)
        .map((item) => ({
          fieldId: asString(item.field_id),
          sourceKey: asString(item.source_key),
        }))
        .filter((item) => item.fieldId !== "" || item.sourceKey !== "")
    : [];
  const rawUnchecked = record?.unchecked;
  return {
    // 缺 `ok` 键时按**不通过**处理：一份读不出结论的报告不能被读成通过
    ok: record?.ok === true,
    missingFields,
    viewMissing: record?.view_missing === true,
    tableMissing: record?.table_missing === true,
    unchecked: Array.isArray(rawUnchecked)
      ? rawUnchecked.filter((item): item is string => typeof item === "string")
      : [],
  };
}

/// 体检：把该表**启用中的** `field_id` 集合拿去和「列出字段」比对（T11）。
///
/// 按 `id`（表级主键）定位——体检的粒度就是一张表。
export async function checkDatasourceHealth(
  datasourceId: number,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<HealthReport> {
  const result = await invokeFeishuAction(
    deps,
    TABLE_OPERATION_IDS.healthCheck,
    { datasource_id: datasourceId },
    signal,
  );
  return parseHealthReport(result.data);
}

/// 回显端点的响应契约：`{"token":"<明文>"}`。两个端点同形。
function tokenFrom(data: unknown, operationId: string): string {
  const token = asString(asRecord(data)?.token);
  if (token === "") {
    throw new Error(`「${operationId}」的响应里没有 token`);
  }
  return token;
}

/// 回显某个字段的 Token。**纯读**：不改任何状态，可以反复调（设计 §10.2.1）。
///
/// `source_key` 走**请求体**（路由 `/api/v1/feishu/datasources/reveal-token` 里没有
/// 路径段）——与 T6/T11 的既有口径一致：本模块凡按标识定位的端点都把它放 body。
export async function revealFieldToken(
  sourceKey: string,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<string> {
  const operationId = TABLE_OPERATION_IDS.reveal;
  const result = await invokeFeishuAction(
    deps,
    operationId,
    { source_key: sourceKey },
    signal,
  );
  return tokenFrom(result.data, operationId);
}

/// 轮换某个字段的 Token。**写**：服务端在事务内换掉摘要 + 密文 + 轮换时间，
/// 旧值当场失效——所以调用方必须先让用户确认过。
export async function rotateFieldToken(
  sourceKey: string,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<string> {
  const operationId = TABLE_OPERATION_IDS.rotate;
  const result = await invokeFeishuAction(
    deps,
    operationId,
    { source_key: sourceKey },
    signal,
  );
  return tokenFrom(result.data, operationId);
}

/// 清单页要的一组凭据入口。`reveal` 是读、`rotate` 是写——**两个不同的操作**，
/// 界面上也必须分开（决策 D10：复制只回显，轮换独立成按钮）。
export type CredentialClient = {
  reveal: (sourceKey: string) => Promise<string>;
  rotate: (sourceKey: string) => Promise<string>;
};

/// 绑定当前会话与界面目录的凭据入口。
///
/// 两条都要求目录里**分别**有对应的 Action：回显是独立权限位（读的是凭据，
/// 不是数据源配置），所以一个部署完全可能只给轮换不给回显，或反过来。
export function useCredentialClient(): CredentialClient {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;

  const deps = useMemo<FeishuInvokeDeps>(
    () => ({ catalog: catalogData, session }),
    [catalogData, session],
  );

  return useMemo(
    () => ({
      reveal: (sourceKey: string) => revealFieldToken(sourceKey, deps),
      rotate: (sourceKey: string) => rotateFieldToken(sourceKey, deps),
    }),
    [deps],
  );
}

/* ------------------------------- 手动触发拉取 ------------------------------ */

/// 请求后台立刻跑一轮。**只回「已受理」**——真正的结果靠轮询数据源行收口。
///
/// 后端刻意不在这里同步拉：一条大表可能超过 HTTP 请求超时（`[http].request_timeout_seconds`，
/// 默认 60 秒），届时客户端看到报错而服务端还在跑，两边对不上。
///
/// 入参是**表级主键** `datasource_id`：拉取的单位是一张表（设计 §6.2），
/// 按 `source_key` 点名一条字段没有意义。后端 `PullNowInput` 是
/// `deny_unknown_fields` + 必填 `datasource_id`，所以这里多发一个键就会被拒。
export async function pullNow(
  datasourceId: number,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<void> {
  await invokeFeishuAction(
    deps,
    DATASOURCE_OPERATION_IDS.pullNow,
    { datasource_id: datasourceId },
    signal,
  );
}

/// 查询自动拉取排程；目录里没有这个 Action（服务端未启用出站拉取）时返回 `null`。
///
/// 返回 `null` 而不是抛错：「没启用」是正常状态，页面按「未知」显示即可。
/// 这与 `requireAction` 的取舍不同——那个是用来暴露「有权限却没有」这种异常配置的。
export async function fetchPullSchedule(
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<PullScheduleInfo | null> {
  if (!hasOperation(deps.catalog, DATASOURCE_OPERATION_IDS.pullSchedule)) {
    return null;
  }
  const result = await invokeFeishuAction(
    deps,
    DATASOURCE_OPERATION_IDS.pullSchedule,
    {},
    signal,
  );
  const data = asRecord(result.data);
  return {
    intervalSeconds: asNumber(data?.interval_seconds, 0),
    // 缺这个键与显式 `null` 都表示「答不出来」，两者不该被区分对待。
    nextRunAt: typeof data?.next_run_at === "number" ? data.next_run_at : null,
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
/// 数据源列表查询。
///
/// `options.enabled` 用于**按需**查询：父级候选那一份只在编辑对话框打开时才要，
/// 让它在列表页每次渲染都跑会白白多拉一页数据。
///
/// `options.refetchInterval` 给「立即拉取」用：触发之后要盯着 `lastPullAt` 何时变化。
/// 默认 `false`（不轮询）——常态下这个列表没有轮询的必要。
export function useDatasourceList(
  query: DatasourceListQuery,
  options: { enabled?: boolean; refetchInterval?: number | false } = {},
): UseQueryResult<ListPage<DatasourceItem>> {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;
  return useQuery({
    enabled: canReadDatasources(catalogData) && options.enabled !== false,
    queryKey: feishuQueryKeys.datasourceList(query),
    queryFn: ({ signal }) =>
      listDatasources(query, { catalog: catalogData, session }, signal),
    // 翻页/搜索时保留上一页结果，避免整页闪空
    placeholderData: keepPreviousData,
    staleTime: 15_000,
    refetchInterval: options.refetchInterval ?? false,
  });
}

/// 自动拉取排程（全局，不随数据源变）。
///
/// `staleTime` 取得比列表短：这个值每一轮都会变，而它正是用来回答「还要等多久」的，
/// 缓存久了会给出一个已经过去的时刻。
export function usePullSchedule(): UseQueryResult<PullScheduleInfo | null> {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;
  return useQuery({
    enabled: canReadDatasources(catalogData),
    queryKey: feishuQueryKeys.pullSchedule(),
    queryFn: ({ signal }) =>
      fetchPullSchedule({ catalog: catalogData, session }, signal),
    staleTime: 5_000,
  });
}

/// 某个数据源下的选项（只读）。缺 `feishu.option.read` 时不发请求，页面渲染 403 说明。
export function useOptionList(
  query: OptionListQuery,
  options: { enabled?: boolean } = {},
): UseQueryResult<ListPage<OptionItem>> {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;
  return useQuery({
    // 详情页的 `source_key` 现在由它自己解析出的绑定给出，所以会先空一拍。
    // `enabled` 让它别拿着空串去打接口——`source_key` 是必填入参，
    // 空串那一发必然是错，而错误的响应看起来和「没有选项」一模一样。
    enabled: canReadOptions(catalogData) && options.enabled !== false,
    queryKey: feishuQueryKeys.optionList(query),
    queryFn: ({ signal }) =>
      listOptions(query, { catalog: catalogData, session }, signal),
    placeholderData: keepPreviousData,
    staleTime: 15_000,
  });
}

/// 页面用的一组「已绑定目录与会话」的写操作入口。
/// 变更函数本身不缓存失效——`update` 只回三个计数，页面提交后必须回读列表。
///
/// **建源不在这一组里**：它是一次写两张表的事务，界面走的是配置向导
/// （`useTableWizardClient`），不是一个「填完就提交」的对话框。
export type FeishuActions = {
  canRead: boolean;
  canWrite: boolean;
  canReadOptions: boolean;
  updateDatasourceTable: (
    input: UpdateTableInput,
  ) => Promise<{ inserted: number; updated: number; disabled: number }>;
  deleteDatasourceTable: (
    datasourceId: number,
  ) => Promise<{ deletedFields: number; disabledOptions: number }>;
  precheckToken: (
    sourceKey: string,
    token: string,
  ) => Promise<TokenPrecheckResult>;
  /// 请求后台立刻拉一次。只回「已受理」，结果靠轮询数据源行。
  /// 入参是**表级主键**（后端 `pull_now` 的单位就是一张表）。
  pullNow: (datasourceId: number) => Promise<void>;
};

export function useFeishuActions(): FeishuActions {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;

  const deps = useMemo<FeishuInvokeDeps>(
    () => ({ catalog: catalogData, session }),
    [catalogData, session],
  );

  const update = useCallback(
    (input: UpdateTableInput) => updateDatasourceTable(input, deps),
    [deps],
  );
  const remove = useCallback(
    (datasourceId: number) => deleteDatasourceTable(datasourceId, deps),
    [deps],
  );
  const precheck = useCallback(
    (sourceKey: string, token: string) =>
      precheckApprovalOptions(sourceKey, token, deps),
    [deps],
  );
  const trigger = useCallback(
    (datasourceId: number) => pullNow(datasourceId, deps),
    [deps],
  );

  return {
    canRead: canReadDatasources(catalogData),
    canWrite: canWriteDatasources(catalogData),
    canReadOptions: canReadOptions(catalogData),
    updateDatasourceTable: update,
    deleteDatasourceTable: remove,
    precheckToken: precheck,
    pullNow: trigger,
  };
}

/* --------------------------- 审批派发控制台 --------------------------- */

/// 配置列表的请求体。坐标列不可搜（`searchable` 只覆盖 `title`），
/// 缺省键一律用 `undefined` 省略。
function buildApprovalConfigListBody(query: {
  page: number;
  pageSize: number;
  search: string;
}): Record<string, unknown> {
  const search = query.search.trim();
  return {
    page: query.page,
    page_size: query.pageSize,
    search: search === "" ? undefined : search,
    // 恒非空：分页要有确定性全序，否则翻页会漏行或重复（后端兜底也是同一对键）。
    order_by: [
      { field: "updated_at", direction: "Desc" },
      { field: "id", direction: "Asc" },
    ],
    count_total: true,
  };
}

/// 记录列表的请求体：坐标/结果筛选折成 `where` 树（`outcome` 已声明 filterable）。
function buildApprovalRequestsBody(
  query: ApprovalRequestsQuery,
): Record<string, unknown> {
  const baseToken = query.baseToken.trim();
  const tableId = query.tableId.trim();
  const where: Record<string, unknown>[] = [];
  if (baseToken !== "") {
    where.push({ type: "eq", field: "base_token", value: baseToken });
  }
  if (tableId !== "") {
    where.push({ type: "eq", field: "table_id", value: tableId });
  }
  if (query.outcome !== "all") {
    where.push({ type: "eq", field: "outcome", value: query.outcome });
  }
  return {
    page: query.page,
    page_size: query.pageSize,
    where:
      where.length === 0
        ? undefined
        : where.length === 1
          ? where[0]
          : { type: "and", conditions: where },
    order_by: withStableOrder(query.orderBy),
    count_total: true,
  };
}

function parseApprovalConfigMap(
  raw: Record<string, unknown>,
): ApprovalConfigMap | null {
  const widgetId = asString(raw.widget_id);
  if (widgetId === "") return null;
  return {
    widgetId,
    widgetType: asString(raw.widget_type),
    bitableField: asString(raw.bitable_field),
    bitableFieldName: asNullableString(raw.bitable_field_name),
    required: raw.required === true,
    converter: asString(raw.converter, "direct"),
  };
}

function parseApprovalConfigItem(
  raw: Record<string, unknown>,
): ApprovalConfigItem | null {
  const id = typeof raw.id === "number" ? raw.id : null;
  if (id === null) return null;
  const rawMaps = raw.maps;
  const maps = Array.isArray(rawMaps)
    ? rawMaps
        .map((item) => asRecord(item))
        .filter((item): item is Record<string, unknown> => item !== undefined)
        .map(parseApprovalConfigMap)
        .filter((item): item is ApprovalConfigMap => item !== null)
    : [];
  return {
    id,
    title: asString(raw.title),
    baseToken: asString(raw.base_token),
    tableId: asString(raw.table_id),
    approvalCode: asString(raw.approval_code),
    applicantField: asString(raw.applicant_field),
    backfillField: asString(raw.backfill_field),
    baseTimezone: asString(raw.base_timezone),
    enabled: raw.enabled === true,
    formSnapshotAt: asNullableNumber(raw.form_snapshot_at),
    updatedAt: asNumber(raw.updated_at, 0),
    maps,
  };
}

function parseApprovalWidget(
  raw: Record<string, unknown>,
): ApprovalWidget | null {
  const id = asString(raw.id);
  return id === ""
    ? null
    : {
        id,
        name: asString(raw.name),
        type: asString(raw.type),
        required: raw.required === true,
      };
}

function parseApprovalRequestItem(
  raw: Record<string, unknown>,
): ApprovalRequestItem | null {
  const id = typeof raw.id === "number" ? raw.id : null;
  if (id === null) return null;
  return {
    id,
    requestedBy: asNullableString(raw.requested_by),
    baseToken: asString(raw.base_token),
    tableId: asString(raw.table_id),
    configId: asNullableNumber(raw.config_id),
    recordId: asNullableString(raw.record_id),
    requestBody: asString(raw.request_body),
    outcome: asString(raw.outcome),
    message: asString(raw.message),
    serialNumber: asNullableString(raw.serial_number),
    responseBody: asNullableString(raw.response_body),
    createdAt: asNumber(raw.created_at, 0),
  };
}

function parseApprovalTaskItem(
  raw: Record<string, unknown>,
): ApprovalTaskItem | null {
  const id = typeof raw.id === "number" ? raw.id : null;
  if (id === null) return null;
  return {
    id,
    configId: asNumber(raw.config_id, 0),
    recordId: asString(raw.record_id),
    state: asString(raw.state),
    instanceCode: asNullableString(raw.instance_code),
    serialNumber: asNullableString(raw.serial_number),
    attempts: asNumber(raw.attempts, 0),
    lastError: asNullableString(raw.last_error),
    createdAt: asNumber(raw.created_at, 0),
    updatedAt: asNumber(raw.updated_at, 0),
  };
}

/// 配置列表（`feishu.approval.list_configs`）。
export async function listApprovalConfigs(
  query: { page: number; pageSize: number; search: string },
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<ListPage<ApprovalConfigItem>> {
  const result = await invokeFeishuAction(
    deps,
    APPROVAL_OPERATION_IDS.listConfigs,
    buildApprovalConfigListBody(query),
    signal,
  );
  return parsePage(result.data, parseApprovalConfigItem);
}

/// 创建配置（`feishu.approval.create_config`）：六件套全部必填，由服务端整条
/// `build_plan + insert_plan` 校验链把关——失败（含唯一冲突「该多维表格已配置」）
/// 一律抛 `ApiError`，message 是可行动原因。
export type CreateApprovalConfigInput = {
  baseToken: string;
  tableId: string;
  approvalCode: string;
  applicantField: string;
  backfillField: string;
  baseTimezone: string;
};

export async function createApprovalConfig(
  input: CreateApprovalConfigInput,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<number> {
  const result = await invokeFeishuAction(
    deps,
    APPROVAL_OPERATION_IDS.createConfig,
    {
      base_token: input.baseToken.trim(),
      table_id: input.tableId.trim(),
      approval_code: input.approvalCode.trim(),
      applicant_field: input.applicantField.trim(),
      backfill_field: input.backfillField.trim(),
      base_timezone: input.baseTimezone.trim(),
    },
    signal,
  );
  const data = asRecord(result.data);
  return asNumber(data?.config_id, 0);
}

/// 更新配置（`feishu.approval.update_config`）：仅 title/enabled/base_timezone 三项，
/// 全可省、至少给一个；省略的键不出现（`deny_unknown_fields` 下「留空 = 不改」）。
export type UpdateApprovalConfigPatch = {
  title?: string;
  enabled?: boolean;
  baseTimezone?: string;
};

export async function updateApprovalConfig(
  configId: number,
  patch: UpdateApprovalConfigPatch,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<void> {
  const body: Record<string, unknown> = { config_id: configId };
  if (patch.title !== undefined) body.title = patch.title;
  if (patch.enabled !== undefined) body.enabled = patch.enabled;
  if (patch.baseTimezone !== undefined) body.base_timezone = patch.baseTimezone;
  await invokeFeishuAction(
    deps,
    APPROVAL_OPERATION_IDS.updateConfig,
    body,
    signal,
  );
}

/// 删除配置（`feishu.approval.delete_config`）：配置行 + 映射行 + pending 任务行
/// 同事务删除；backfilled/terminal 任务保留作流水。
export async function deleteApprovalConfig(
  configId: number,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<{ deletedMaps: number; deletedPendingTasks: number }> {
  const result = await invokeFeishuAction(
    deps,
    APPROVAL_OPERATION_IDS.deleteConfig,
    { config_id: configId },
    signal,
  );
  const data = asRecord(result.data);
  return {
    deletedMaps: asNumber(data?.deleted_field_maps, 0),
    deletedPendingTasks: asNumber(data?.deleted_pending_tasks, 0),
  };
}

/// 控件预览（`feishu.approval.list_widgets`）：按 approval_code 出站调飞书并解析表单。
/// 归 write 侧（出站耗频控配额），目录里没有它时 `requireAction` 抛错。
export async function listApprovalWidgets(
  approvalCode: string,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<ApprovalWidget[]> {
  const result = await invokeFeishuAction(
    deps,
    APPROVAL_OPERATION_IDS.listWidgets,
    { approval_code: approvalCode.trim() },
    signal,
  );
  return listOf(result.data, "widgets", parseApprovalWidget);
}

/// 派发记录（`feishu.approval.list_requests`）。请求体/返回体原文随行返回，本地展开。
export async function listApprovalRequests(
  query: ApprovalRequestsQuery,
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<ListPage<ApprovalRequestItem>> {
  const result = await invokeFeishuAction(
    deps,
    APPROVAL_OPERATION_IDS.listRequests,
    buildApprovalRequestsBody(query),
    signal,
  );
  return parsePage(result.data, parseApprovalRequestItem);
}

/// 任务列表（`feishu.approval.list_tasks`）：批量受理行的下钻视图，按 config_id 过滤。
export async function listApprovalTasks(
  query: { page: number; pageSize: number; configId: number },
  deps: FeishuInvokeDeps,
  signal?: AbortSignal,
): Promise<ListPage<ApprovalTaskItem>> {
  const result = await invokeFeishuAction(
    deps,
    APPROVAL_OPERATION_IDS.listTasks,
    {
      page: query.page,
      page_size: query.pageSize,
      where: { type: "eq", field: "config_id", value: query.configId },
      order_by: [
        { field: "updated_at", direction: "Desc" },
        { field: "id", direction: "Asc" },
      ],
      count_total: true,
    },
    signal,
  );
  return parsePage(result.data, parseApprovalTaskItem);
}

/// 配置列表页的查询 hook。搜索词去抖由页面自己做（照 `useListQuery` 的
/// `useDebouncedValue`），这里只接分页。
export function useApprovalConfigList(
  query: { page: number; pageSize: number; search: string },
  options: { enabled?: boolean } = {},
): UseQueryResult<ListPage<ApprovalConfigItem>> {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;
  return useQuery({
    enabled: canReadApproval(catalogData) && options.enabled !== false,
    queryKey: feishuQueryKeys.approvalConfigList(query),
    queryFn: ({ signal }) =>
      listApprovalConfigs(query, { catalog: catalogData, session }, signal),
    placeholderData: keepPreviousData,
    staleTime: 15_000,
  });
}

/// 记录列表页的查询 hook。
export function useApprovalRequestList(
  query: ApprovalRequestsQuery,
  options: { enabled?: boolean } = {},
): UseQueryResult<ListPage<ApprovalRequestItem>> {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;
  return useQuery({
    enabled: canReadApproval(catalogData) && options.enabled !== false,
    queryKey: feishuQueryKeys.approvalRequestList(query),
    queryFn: ({ signal }) =>
      listApprovalRequests(query, { catalog: catalogData, session }, signal),
    placeholderData: keepPreviousData,
    staleTime: 15_000,
  });
}

/// 任务列表 hook（记录页的「查看任务」下钻）。`configId` 为 null 时不发请求。
export function useApprovalTasks(
  configId: number | null,
  options: { enabled?: boolean } = {},
): UseQueryResult<ListPage<ApprovalTaskItem>> {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;
  return useQuery({
    enabled:
      canReadApproval(catalogData) &&
      configId !== null &&
      options.enabled !== false,
    queryKey: [FEISHU_QUERY_ROOT, "approval-tasks", configId],
    queryFn: ({ signal }) =>
      listApprovalTasks(
        { page: 1, pageSize: 20, configId: configId as number },
        { catalog: catalogData, session },
        signal,
      ),
    staleTime: 15_000,
  });
}

/// 配置页与向导要用的「已绑定目录与会话」的审批写操作入口。
export type ApprovalActions = {
  canRead: boolean;
  canWrite: boolean;
  updateConfig: (
    configId: number,
    patch: UpdateApprovalConfigPatch,
  ) => Promise<void>;
  deleteConfig: (
    configId: number,
  ) => Promise<{ deletedMaps: number; deletedPendingTasks: number }>;
};

export function useApprovalActions(): ApprovalActions {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;

  const deps = useMemo<FeishuInvokeDeps>(
    () => ({ catalog: catalogData, session }),
    [catalogData, session],
  );

  const updateConfig = useCallback(
    (configId: number, patch: UpdateApprovalConfigPatch) =>
      updateApprovalConfig(configId, patch, deps),
    [deps],
  );
  const deleteConfig = useCallback(
    (configId: number) => deleteApprovalConfig(configId, deps),
    [deps],
  );

  return {
    canRead: canReadApproval(catalogData),
    canWrite: canWriteApproval(catalogData),
    updateConfig,
    deleteConfig,
  };
}

/// 建配置向导要用的一组数据访问入口（可注入，照 `TableWizardClient` 的形态）。
///
/// 前三个是既有元数据端点（`list_bitable_*`），与建数据源向导共用；后两个是本域新增。
export type ApprovalWizardClient = {
  listTables: (appToken: string) => Promise<BitableTable[]>;
  listViews: (appToken: string, tableId: string) => Promise<BitableView[]>;
  listFields: (appToken: string, tableId: string) => Promise<BitableField[]>;
  listWidgets: (approvalCode: string) => Promise<ApprovalWidget[]>;
  createConfig: (input: CreateApprovalConfigInput) => Promise<number>;
};

export function useApprovalWizardClient(): ApprovalWizardClient {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;

  const deps = useMemo<FeishuInvokeDeps>(
    () => ({ catalog: catalogData, session }),
    [catalogData, session],
  );

  return useMemo(
    () => ({
      listTables: (appToken: string) => listBitableTables(appToken, deps),
      listViews: (appToken: string, tableId: string) =>
        listBitableViews(appToken, tableId, deps),
      listFields: (appToken: string, tableId: string) =>
        listBitableFields(appToken, tableId, deps),
      listWidgets: (approvalCode: string) =>
        listApprovalWidgets(approvalCode, deps),
      createConfig: (input: CreateApprovalConfigInput) =>
        createApprovalConfig(input, deps),
    }),
    [deps],
  );
}

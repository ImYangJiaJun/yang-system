/**
 * 飞书数据源控制台的领域类型与展示词汇。
 *
 * 这里的字段形状逐条对齐后端（`src/addon/feishu/**`）：
 * - `updated_at` 是 **unix 秒（number）**，不是 ISO 串；
 * - `i18n` 是 **JSON 文本**（可能是 `'{"zh_cn":"差旅费"}'` 或 `null`），不要直接铺在单元格里；
 * - `default_locale` 的取值域是下划线形式 `zh_cn` / `en_us` / `ja_jp`，
 *   它会被原样当作回给飞书的 `locale` 键，**后端对它零校验**，只能靠界面守住。
 */

import { PRODUCT_LOCALE } from "@/shared/lib/product-locale";

/// 数据源状态：后端只回这两个值（`datasource/table.rs` 的 Radio 选项）。
export type DatasourceStatus = "active" | "disabled";

/// 默认语言。写错（例如 `zh-CN`）会让该数据源在飞书侧所有语言下都取不到文案。
export type DefaultLocale = "zh_cn" | "en_us" | "ja_jp";

export const DEFAULT_LOCALE: DefaultLocale = "zh_cn";

/// 三选一的下拉选项：value 必须是后端取值域，label 才是给人看的。
export const DEFAULT_LOCALE_OPTIONS: ReadonlyArray<{
  value: DefaultLocale;
  label: string;
}> = [
  { value: "zh_cn", label: "简体中文" },
  { value: "en_us", label: "English" },
  { value: "ja_jp", label: "日本語" },
];

/// 语言代码 → 展示名。未知值原样回显（不猜、不静默替换成默认值）。
export function localeLabel(value: string): string {
  return (
    DEFAULT_LOCALE_OPTIONS.find((option) => option.value === value)?.label ??
    value
  );
}

/// 任意字符串 → `DefaultLocale`；**认不出就回 null**，不做任何替换。
///
/// 后端对 `default_locale` 零校验，所以库里可能出现取值域以外的值（例如 `zh-CN`）。
/// 界面要守住这个取值域，就只能在**认得出**的时候才把它放进三选一控件：
/// 认不出还硬塞一个默认值，会让用户一保存就把那个值悄悄改掉，而那个值正是
/// 「所有语言下控件都显示为空」的原因，不能不经确认就抹掉。
export function asDefaultLocale(value: string): DefaultLocale | null {
  return DEFAULT_LOCALE_OPTIONS.some((option) => option.value === value)
    ? (value as DefaultLocale)
    : null;
}

/// 状态 → 展示名。
export function statusLabel(status: DatasourceStatus): string {
  return status === "active" ? "启用" : "已停用";
}

/// 取数方式：后端 `datasource/table.rs` 的 Radio 取值域。
///
/// - `push`：多维表格自动化工作流持管理 Token 推选项过来（存量数据源都是这种）；
/// - `pull`：服务端按 `feishu.pull_interval_seconds` 自己去多维表格拉。
///
/// 两种方式的**失败面完全不同**：`push` 出问题看不出任何服务端状态（没有那次请求），
/// `pull` 才有 `lastSuccessAt` / `consecutiveFailures` 可看。表单里切换它是有后果的。
export type IngestMode = "push" | "pull";

export const INGEST_MODE_OPTIONS: ReadonlyArray<{
  value: IngestMode;
  label: string;
  hint: string;
}> = [
  {
    value: "push",
    label: "手工推送",
    hint: "由多维表格自动化工作流推送选项，服务端不主动出网。",
  },
  {
    value: "pull",
    label: "定时拉取",
    hint: "服务端按配置的间隔主动拉取，需要先配好应用凭证与坐标。",
  },
];

/// 取数方式 → 展示名。认不出的值原样回显（与 `localeLabel` 同一取舍：不猜、不替换）。
export function ingestModeLabel(value: string): string {
  return (
    INGEST_MODE_OPTIONS.find((option) => option.value === value)?.label ?? value
  );
}

/// 任意字符串 → `IngestMode`；认不出回 `null`。
export function asIngestMode(value: string): IngestMode | null {
  return INGEST_MODE_OPTIONS.some((option) => option.value === value)
    ? (value as IngestMode)
    : null;
}

/// 日期时间格式必须显式带产品 locale（`scripts/verify-locale-contract.mjs` 的合同）。
const DATE_TIME_FORMATTER = new Intl.DateTimeFormat(PRODUCT_LOCALE, {
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  hour12: false,
});

/// unix 秒 → 本地展示文本。后端所有时间字段都是 unix 秒，缺失时是 0。
export function formatUnixSeconds(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return "—";
  return DATE_TIME_FORMATTER.format(new Date(value * 1000));
}

/// 数据源列表项；与 `list_datasources` 的 `items[]` 一一对应。
export type DatasourceItem = {
  /// 表级主键。体检与绑定都按它定位（`list_datasources` 的 `id`）。
  id: number | null;
  /// 勾选的字段绑定。**空数组表示一条都没配**，不是「没取到」。
  ///
  /// 表级行上**没有** `source_key`——一条表级行有 N 个，全在这一层。它曾经作为
  /// 一个恒为空串的字段留在这里「为了不把存量调用点一次全改掉」，结果就是
  /// 详情页拿它去查一张没有这一列的表（`order_by: source_key` → 400）。
  /// 现在删掉了：留一个恒空的旧身份，等于给下一个消费方留一条错路。
  fields: DatasourceFieldBinding[];
  title: string;
  /// **没有**「加密返回」与「默认语言」：两者都是**绑定级**属性（一条数据源有 N 个
  /// 字段，可以各自加密、各自语言），表级行上没有单一值可显示。它们在这里曾经各占
  /// 一个字段，而表级投影从来不发那两个键——于是列表上那两列对每一条源都恒画
  /// 「—」与一个空语言徽标。逐字段的取值见 [`DatasourceFieldBinding`]。
  status: DatasourceStatus;
  /// 该行最后一次被写入的时间（unix 秒）。
  updatedAt: number;

  /* ------------------------------ 取数配置 ------------------------------ */

  /// 取数方式。`push` 数据源下面这些坐标都是 null。
  ingestMode: string;
  bitableBaseToken: string | null;
  bitableTableId: string | null;
  /// 视图 ID。**可空 = 整表拉取**，不是「缺一个坐标」。
  bitableViewId: string | null;
  // 级联映射**不在这里**：表级化之后级联就是每条绑定的 `parentFieldId`，
  // 前端也从未消费过 `linkageMapping`（它恒为 null——后端从不发那个键）。

  /* ------------------------------ 同步状态 ------------------------------ */

  /// 最近一次尝试拉取的时间（unix 秒）；失败也会更新它。
  lastPullAt: number | null;
  /// 最近一次**成功**同步的时间（unix 秒）。
  ///
  /// 这是判断「这个源还活着吗」唯一诚实的信号：`updatedAt` 只在整行被写时变，
  /// 而「拉了一轮但内容没变」根本不会写库。
  lastSuccessAt: number | null;
  /// 连续失败次数。**只在整轮成功时清零**，所以它单调递增即代表「一直没成功」。
  consecutiveFailures: number;
  /// 最近一次失败的错误原文（可能较长）。
  lastError: string | null;
  // `snapshotDigest` **不在这里**：摘要归属在表级化时搬到了字段绑定，
  // 表级那一列谁都不写。逐字段的摘要在绑定表上，需要时再加投影。
};

/// 列表上用来指认一条表级数据源的那一行等宽文字。
///
/// 表级化之后一条行有 N 个 `source_key`，行上自己不再有标识，所以取**首个绑定**的
/// 标识——它就是粘进审批控件地址里的那一段。一条绑定都没有时退到主键 `#id`：
/// 总得有个能指认的东西。
///
/// **不再退回落表级行的 `sourceKey`**：那一列在表级化时已经移走，服务端永远不发它。
export function identityLabel(item: DatasourceItem): string {
  const first = item.fields[0]?.sourceKey;
  if (first !== undefined && first !== "") return first;
  return item.id === null ? "—" : `#${item.id}`;
}

/// 一个数据源的坐标是否配齐。
///
/// 表级化之后的坐标是 **Base Token + 数据表 ID** 两项：视图可空（= 整表拉取），
/// 而「取数列」不再是表级坐标——一条表级行有 N 条字段绑定，每条各自取自己那一列，
/// 单一取数列这个概念已经不存在了。
///
/// **曾经它还要求 `取数列字段名`**，那是字段级时代的坐标。表级化把那一列搬进了绑定层，
/// 于是这个函数恒为 false，详情页对**每一条**数据源都显示「坐标不完整，不会被拉取」——
/// 而服务端其实正在正常拉取。界面在这里说的是一句可查证的假话，比不说更坏。
///
/// 表单与详情页都用它判断「这个 pull 源能不能拉起来」——**不据此禁用保存**：
/// 允许先建一个坐标不全的源再补齐，比强制一次填完更好用，而坐标缺失的源会被
/// 服务端每轮跳过并告警，是响亮的失败。
export function hasCompleteCoordinates(item: {
  bitableBaseToken: string | null;
  bitableTableId: string | null;
}): boolean {
  return [item.bitableBaseToken, item.bitableTableId].every(
    (value) => typeof value === "string" && value.trim() !== "",
  );
}

/// 详情页「取不到这条数据源」的四种原因。**四种必须分开说**——下一步动作完全不同：
/// 地址不合法要回列表重进、加载中要等、查询失败要重试并看错误码、真没有要回列表确认。
/// 折成一句「查不到这条数据源」就是在替用户断言一个页面并不知道的原因，而且往往是假的。
export type DatasourceGap =
  | { kind: "loading" }
  | { kind: "missing-id" }
  /// 当前身份**根本没有** `feishu.datasource.read`。这一态必须与「加载中」分开：
  /// 权限不足时那边查询是 `enabled: false`，永远不会落定，画成骨架屏就是一块
  /// 永远转下去的空白——而真相是一句话就能说清的事。
  | { kind: "forbidden" }
  | {
      kind: "failed";
      status: number | null;
      code: number | null;
      message: string;
      requestId: string | null;
    }
  | { kind: "absent" };

/// 把抛错折成可展示的四要素。引擎的 `ApiError` 恰好带这四项，这里按**结构**取而不
/// import 它——展示层不必依赖传输层的类。取不到就留 `null`，不编一个 0 出来。
export function describeApiError(error: unknown): {
  status: number | null;
  code: number | null;
  message: string;
  requestId: string | null;
} {
  const record =
    error !== null && typeof error === "object"
      ? (error as Record<string, unknown>)
      : undefined;
  const numberOrNull = (value: unknown): number | null =>
    typeof value === "number" && Number.isFinite(value) ? value : null;
  return {
    status: numberOrNull(record?.status),
    code: numberOrNull(record?.code),
    message:
      error instanceof Error
        ? error.message
        : typeof record?.message === "string"
          ? record.message
          : "未知错误",
    requestId: typeof record?.requestId === "string" ? record.requestId : null,
  };
}

/// 详情页该渲染哪一个「缺口」；`null` = 数据源拿到了，渲染正常面板。
///
/// 顺序有讲究，每一档都不能挪：
/// 1. `present` 先判——拿到了就没什么可说的；
/// 2. `id === null`：地址不合法时查询**根本没发**，它既不是加载中也不是失败；
/// 3. `canRead === false`：权限不足时查询同样不发（`enabled: false`），
///    `isPending` 会**永远**为真——先判掉它，否则这一态会被画成永远转下去的骨架屏；
/// 4. 之后才轮到加载中与真失败。
export function datasourceGap(state: {
  id: number | null;
  canRead: boolean;
  isPending: boolean;
  isError: boolean;
  error: unknown;
  present: boolean;
}): DatasourceGap | null {
  if (state.present) return null;
  if (state.id === null) return { kind: "missing-id" };
  if (!state.canRead) return { kind: "forbidden" };
  if (state.isPending) return { kind: "loading" };
  if (state.isError)
    return { kind: "failed", ...describeApiError(state.error) };
  return { kind: "absent" };
}

/// 把绑定按「父在前、子紧随其后并缩进」排成展示序。
///
/// 一条数据源的级联是**同表内**的父子链（`parentFieldId` 指向另一条绑定的
/// `fieldId`），所以「父子在一起」不需要引入分组概念——排一次序就够了。
///
/// 三种边界都要照顾到，它们在真实数据里都会出现：
/// - **父不在集合里**（父列被取消了勾选、或父列本身没勾）：当成根。直接跳过它
///   会把整棵子树从界面上抹掉。
/// - **环**：建源侧有校验，但历史行与并发写都可能留下环。靠 `visited` 兜住，
///   不死循环也不重复出行。
/// - **同一父下的次序**：保持后端给的顺序（绑定建立顺序），**不重排**——
///   重排会让运维每次刷新看到不同的行序，而这一栏是用来对照飞书那张表的。
export function orderBindingsForDisplay(
  bindings: readonly DatasourceFieldBinding[],
): Array<{ binding: DatasourceFieldBinding; depth: number }> {
  const byFieldId = new Map(
    bindings.map((binding) => [binding.fieldId, binding]),
  );
  const children = new Map<string, DatasourceFieldBinding[]>();
  const roots: DatasourceFieldBinding[] = [];
  for (const binding of bindings) {
    const parent = binding.parentFieldId;
    if (parent === null || !byFieldId.has(parent)) {
      roots.push(binding);
      continue;
    }
    const siblings = children.get(parent);
    if (siblings === undefined) children.set(parent, [binding]);
    else siblings.push(binding);
  }

  const out: Array<{ binding: DatasourceFieldBinding; depth: number }> = [];
  const visited = new Set<string>();
  const walk = (binding: DatasourceFieldBinding, depth: number) => {
    if (visited.has(binding.fieldId)) return;
    visited.add(binding.fieldId);
    out.push({ binding, depth });
    for (const child of children.get(binding.fieldId) ?? []) {
      walk(child, depth + 1);
    }
  };
  for (const root of roots) walk(root, 0);
  // 只出现在环里、没有任何根能走到的绑定也要露出来：宁可行序不理想，
  // 也不能让一条真实存在的绑定从界面上消失。
  for (const binding of bindings) walk(binding, 0);
  return out;
}

/// 选项行；与 `list_options` 的 `items[]` 一一对应。控制台对它是**只读**的。
export type OptionItem = {
  optionId: string;
  sourceKey: string;
  label: string;
  /// JSON 文本，可能是 null。展示前必须解析。
  i18n: string | null;
  sortOrder: number;
  isDefault: boolean;
  enabled: boolean;
  /// 级联父键（裸 `option_id`）；无父时为空串。
  ///
  /// 界面拿它显示「这条选项挂在哪个父项下」。级联配错时，一个孤零零的子项
  /// 会在这里暴露出来——比只看到「选项少了几条」好排查得多。
  parentKey: string | null;
  /// unix 秒；这行选项最后一次被**写入**的时间。
  ///
  /// **不要拿它当「推送还活着吗」的信号**：两条写入路径（多维表格推送、服务端拉取）
  /// 都会改它。存活信号是 `lastPushAt`。
  updatedAt: number;
  /// unix 秒；只有写入路径会写它，从未被写入过的行为 null。
  lastPushAt: number | null;
};

/// 分页响应：`total` 只在请求带 `count_total: true` 时非 null。
export type ListPage<T> = {
  items: T[];
  page: number;
  pageSize: number;
  total: number | null;
};

/// 排序方向。**必须是 PascalCase**：后端的 wire 形状就是 `"Asc"` / `"Desc"`，
/// 小写会被反序列化拒绝。
export type SortDirection = "Asc" | "Desc";

export type OrderByClause = { field: string; direction: SortDirection };

/// 台账视图的列头可排序字段。其余列后端没有声明 sortable，画排序箭头就是在要求后端改动。
///
/// 只剩 `title`：表级行上唯一的另一个可排序列是 `id`，而它由 `withStableOrder`
/// 恒作收尾键（不该再出现在列头里）。`source_key` 已经不在表级行上。
export type DatasourceSortField = "title";

export type DatasourceStatusFilter = "all" | "active" | "disabled";

/// 一次数据源列表查询的全部输入（不含视图选择，视图不改变数据）。
export type DatasourceListQuery = {
  page: number;
  pageSize: number;
  search: string;
  status: DatasourceStatusFilter;
  /// 恒非空：不发排序就是无序分页，翻页会重复/漏行。
  orderBy: OrderByClause[];
  /// 按表级主键取**单条**（详情页用）。省略表示不按主键收窄。
  ///
  /// 没有「取单条数据源」这个 Action，所以详情页借列表端点 + 一个 `id` 等值条件。
  /// 这比 `search` 可靠：检索只覆盖 `searchable` 的列，而表级行上只有 `title` 可搜。
  id?: number;
};

/// 一次选项列表查询的全部输入。
export type OptionListQuery = {
  sourceKey: string;
  page: number;
  pageSize: number;
  orderBy: OrderByClause[];
};

export type DatasourceView = "ledger" | "cards";

/// 页面级查询状态：查询输入 + 纯本地的视图选择（两个视图共用同一份查询输入）。
export type ListQueryState = DatasourceListQuery & { view: DatasourceView };

/**
 * `approval_options` 失败信封的错误码（`option/actions/approval_options.rs` 的 `codes`）。
 * 飞书侧只按 `code != 0` 判失败，具体数值是我们自己用来归因的。
 */
export const APPROVAL_OPTION_CODES = {
  sourceNotFound: 40401,
  tokenMissing: 40101,
  tokenMismatch: 40102,
  sourceDisabled: 40301,
  cursorInvalid: 40002,
  encryptionNotConfigured: 50002,
  internal: 50001,
  timeout: 50401,
} as const;

/// 失败码 → 一句「接下来怎么办」。认不出的码不猜，照实说不知道。
export function approvalCodeHint(code: number | null): string {
  switch (code) {
    case APPROVAL_OPTION_CODES.tokenMismatch:
      return "Token 与这个数据源里存的摘要不一致。数据源本身没问题，重填一次 Token 即可，不必删掉重建。";
    case APPROVAL_OPTION_CODES.tokenMissing:
      return "这次请求没有带上 Token。";
    case APPROVAL_OPTION_CODES.sourceNotFound:
      return "服务端查不到这个数据源标识。";
    case APPROVAL_OPTION_CODES.sourceDisabled:
      return "数据源已被停用，飞书侧此刻取不到任何选项。";
    case APPROVAL_OPTION_CODES.encryptionNotConfigured:
      return "数据源开启了「加密返回」，但服务端没有配置加密密钥，控件会拿不到选项。";
    case APPROVAL_OPTION_CODES.timeout:
      return "服务端处理超出预算（飞书这次回调的超时是 3 秒）。";
    case APPROVAL_OPTION_CODES.internal:
      return "服务端内部错误，需要查服务端日志。";
    default:
      return "服务端返回了未预期的失败。";
  }
}

/**
 * 失败码 → 一句「这次失败说明了什么」。
 *
 * 与 [`approvalCodeHint`] 分工不同：这里回答**这次失败意味着什么**，那里回答**接下来怎么办**。
 * 分开是因为「意味着什么」不能一句话盖住——服务端的判定顺序是
 * 「先按 source_key 查数据源 → 再比对 Token → 最后看状态」（`verify_source` 与 `resolve`），
 * 所以 40401 / 40301 / 50002 这三种失败里 **Token 恰恰是对的**。
 * 把它们一律说成「Token 没有通过验证」，会把排查方向引到重填 Token 上，
 * 而真正要修的是别的地方（数据源没落库、数据源被停用、服务端没配密钥）。
 */
export function approvalCodeVerdict(code: number | null): string {
  switch (code) {
    case APPROVAL_OPTION_CODES.tokenMismatch:
      return "Token 没对：服务端存的摘要与这次传进来的不一致。";
    case APPROVAL_OPTION_CODES.tokenMissing:
      return "这次请求没有带上 Token，服务端无从比对。";
    case APPROVAL_OPTION_CODES.sourceNotFound:
      return "服务端查不到这个数据源标识——Token 对不对还无从谈起。";
    case APPROVAL_OPTION_CODES.sourceDisabled:
      return "Token 已经通过了比对；卡住的是数据源本身处于停用状态。";
    case APPROVAL_OPTION_CODES.encryptionNotConfigured:
      return "Token 已经通过了比对；卡住的是服务端没有配置加密密钥。";
    case APPROVAL_OPTION_CODES.timeout:
      return "服务端处理超出预算，这次没验到底。";
    case APPROVAL_OPTION_CODES.internal:
      return "服务端内部错误，这次没验到底。";
    default:
      return "服务端返回了未预期的失败，这次没验到底。";
  }
}

/// 预检回执：`ok` 表示服务端认了这次「数据源 + Token」的组合。
///
/// `ok` 分成两支而不是用两个可选字段：加密返回时前端**读不到**条数与「还有更多」，
/// 那就不该有地方能填出一个值来——`encrypted: true` 那一支根本没有这两个字段。
export type TokenPrecheckResult =
  | {
      status: "ok";
      /// 服务端这一页拉到的选项条数。**单页上限 100**，所以 `hasMore` 为真时它不是总数。
      optionCount: number;
      /// 服务端说有下一页。为真时界面只能说「还有更多」，报不出确切总数。
      hasMore: boolean;
      encrypted: false;
    }
  | {
      status: "ok";
      /// 数据源开了「加密返回」且服务端配了密钥：本次拿到的是一段密文，
      /// 条数与「还有更多」都读不出来，所以都不给。
      optionCount: null;
      encrypted: true;
    }
  | {
      status: "failed";
      code: number | null;
      /// 服务端信封里的 `msg` 原文（这个端点用 `msg` 而不是 `message`）。
      message: string;
      /// 这个码意味着什么（**未必是 Token 的问题**，见 `approvalCodeVerdict`）。
      verdict: string;
      /// 接下来怎么办。
      hint: string;
    };

/// 同步健康度：把 `DatasourceItem` 的同步状态折成一句可展示的判断。
///
/// 抽成纯函数是为了可测——这里的每一分支都对应一种**运维要采取不同动作**的情形，
/// 把它们埋在 JSX 里就只能靠肉眼看。
export type SyncHealth = {
  tone: "positive" | "warning" | "info" | "neutral";
  title: string;
  detail: string;
};

export function syncHealth(item: DatasourceItem): SyncHealth {
  // 停用优先于一切：一个停用的源不参与拉取，它的失败计数停在哪里都不代表现状。
  if (item.status === "disabled") {
    return {
      tone: "neutral",
      title: "已停用",
      detail:
        "数据源处于停用状态，不参与拉取。下面的时间是它停用前的最后一次记录。",
    };
  }

  if (asIngestMode(item.ingestMode) !== "pull") {
    return {
      tone: "info",
      title: "由多维表格推送",
      detail:
        "这个数据源靠多维表格自动化工作流推送选项，服务端不主动出网，因此没有同步状态可看。推送是否还活着，只能去那张多维表格的自动化日志里确认。",
    };
  }

  if (!hasCompleteCoordinates(item)) {
    return {
      tone: "warning",
      title: "坐标不完整，不会被拉取",
      detail:
        "定时拉取需要 Base Token 与数据表 ID 两项齐备（视图可空，表示整表拉取）。缺任何一项，服务端每轮都会跳过这个数据源。",
    };
  }

  if (item.consecutiveFailures > 0) {
    return {
      tone: "warning",
      title: `连续失败 ${item.consecutiveFailures} 次`,
      detail:
        "这个计数只在整轮成功时清零，所以它一直涨就代表「一直没成功过」。最近一次的错误原文见下方。",
    };
  }

  if (item.lastSuccessAt === null) {
    return {
      tone: "info",
      title: "尚未同步过",
      detail:
        "坐标已配齐但还没跑过一轮。服务端启动后会立刻拉一次，之后按配置的间隔轮询。",
    };
  }

  return {
    tone: "positive",
    title: "同步正常",
    detail:
      "最近一轮同步成功。拉取间隔由服务端的 feishu.pull_interval_seconds 决定。",
  };
}

/* ------------------------------ 自动拉取排程 ------------------------------ */

/// 服务端报告的自动拉取排程（`pull_schedule` Action）。
///
/// **排程是全局的，不是每条数据源一份**——只有一个 worker、一个循环，下一轮的时间对
/// 所有数据源都相同。所以它没有 `sourceKey`，任何一条源的详情页看到的都是同一个值。
export type PullScheduleInfo = {
  /// 配置的轮询间隔（秒）。
  intervalSeconds: number;
  /// 下次自动拉取的 unix 秒；`null` = 正在拉取，或服务端还没跑过第一轮。
  nextRunAt: number | null;
};

export type NextPullView = {
  /// 「下次自动拉取」那一栏的主文案。
  label: string;
  /// 补充说明（间隔、或为什么答不出来）。
  detail: string;
};

/// 秒数 → 人话。不足一分钟按秒说，不四舍五入成「0 分钟」。
function describeDuration(seconds: number): string {
  if (seconds < 60) return `${seconds} 秒`;
  if (seconds % 3600 === 0) return `${seconds / 3600} 小时`;
  if (seconds % 60 === 0) return `${seconds / 60} 分钟`;
  return `${Math.round(seconds / 60)} 分钟`;
}

/// 排程 → 「下次自动拉取」那一栏的文案。
///
/// `nowSeconds` 显式传入而不是读内部时钟：这个函数要回答「还有多久」，用内部时钟就没法测。
/// 参数名带上单位是刻意的——`nextRunAt` 是**秒**，而 `Date.now()` 是**毫秒**，
/// 传错单位不会报错，只会让比较恒真、界面永远显示「即将开始」。
///
/// 三种答不出来的形态刻意分开：**没有排程**（端点没注册）、**正在拉取**（下一轮还没排）、
/// **已到点**（随时会开跑）。它们对用户是三个不同的处境，合并成一句「未知」等于没答。
export function describeNextPull(
  schedule: PullScheduleInfo | null,
  nowSeconds: number,
): NextPullView {
  if (schedule === null) {
    return {
      label: "未知",
      detail: "服务端没有报告排程——出站拉取可能未启用。",
    };
  }

  const interval = describeDuration(schedule.intervalSeconds);

  if (schedule.nextRunAt === null) {
    return {
      label: "正在拉取",
      detail: `这一轮跑完才会排出下一次；配置的轮询间隔是 ${interval}。`,
    };
  }

  if (schedule.nextRunAt <= nowSeconds) {
    return {
      label: "即将开始",
      detail: `已经到点，服务端随时会开跑；配置的轮询间隔是 ${interval}。`,
    };
  }

  return {
    label: `${formatUnixSeconds(schedule.nextRunAt)}（约 ${describeDuration(
      schedule.nextRunAt - nowSeconds,
    )}后）`,
    // 后端在一轮**跑完之后**才排下一次，所以「下次」不是「上次 + 间隔」。
    detail: `真实周期是「间隔 + 单轮耗时」，这里给的是最早可能开跑的时刻（间隔 ${interval}）。`,
  };
}

/// 取选项接口的完整地址。
///
/// 用**调用方给的 origin** 拼而不是硬编码主机名：`window.location.origin` 一填，
/// 本地开发时会自动变成 `http://localhost:5273`，不必在两处维护同一个常量。
export function approvalOptionsUrl(origin: string, sourceKey: string): string {
  const base = origin.endsWith("/") ? origin.slice(0, -1) : origin;
  return `${base}/api/v1/feishu/approval/options/${sourceKey}`;
}

/// 手动触发之后，「我点的那一轮跑完了吗」。
///
/// 判据是 `lastPullAt` 变化而**不是** `lastSuccessAt`：后端每轮**开跑就写** `last_pull_at`，
/// 成功与失败都写。手动触发要回答的是「跑了没有」，不是「成功了吗」——后者由
/// `consecutiveFailures` 与 `lastError` 呈现。拿 `lastSuccessAt` 当判据会让失败的一轮
/// 永远等不到落定，按钮一直转。
///
/// 数据源取不到时返回 `false`：列表还在加载、或那一条刚被删掉，都不该被当成跑完了。
export function pullLanded(
  baseline: number | null,
  item: DatasourceItem | null,
): boolean {
  return item !== null && item.lastPullAt !== baseline;
}

/* ------------------------------ 级联（父级） ------------------------------ */

/// 通配键：不指定联动控件代码时用它。与后端 `domain/linkage.rs` 的
/// `WILDCARD_KEY` 必须一致。
///
/// 存在的理由：联动的键是**飞书表单里那个控件的字段代码**（形如
/// `widget17796881173030001`）。要求用户去表单设计器里翻出它，是在索取一个
/// **我们自己从未观测过真实报文**的值——填错了不会报错，只会静默退化成「无级联」。
/// 而一个数据源只服务一个联动控件（契约 C3 的「不带联动要回退全量」正基于此），
/// 所以任何联动参数只可能是它。
export const LINKAGE_WILDCARD_KEY = "*";

/// 级联在界面上的形状。**只有两个必填成员**——后端曾经还有第三个
/// `cascade_field`，但它零消费（拉取用的是数据源自己的取数列），已去掉。
export type LinkageFormValue = {
  /// 父数据源的 `source_key`。
  parentSourceKey: string;
  /// **本表**里承载父文案的列名。
  parentField: string;
  /// 联动控件的字段代码；**空串表示通配**（见 [`LINKAGE_WILDCARD_KEY`]）。
  widgetCode: string;
};

/// `linkage_mapping` 文本 → 界面值。解析不出、或条目不完整时返回 `null`
/// （即「无级联」）——与后端「配错的条目只让那个数据源退化成无级联」同一取舍。
///
/// 只取**第一条**：拉取侧只可能有一条级联，多出来的条目本来就是无效配置。
export function parseLinkageMapping(
  raw: string | null,
): LinkageFormValue | null {
  if (raw === null || raw.trim() === "") return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed))
    return null;
  const entries = Object.entries(parsed as Record<string, unknown>);
  if (entries.length === 0) return null;
  const [key, value] = entries[0];
  if (typeof value !== "object" || value === null) return null;
  const entry = value as Record<string, unknown>;
  const parentSourceKey =
    typeof entry.parent_source_key === "string"
      ? entry.parent_source_key.trim()
      : "";
  const parentField =
    typeof entry.parent_field === "string" ? entry.parent_field.trim() : "";
  if (parentSourceKey === "" || parentField === "") return null;
  return {
    parentSourceKey,
    parentField,
    // 通配键回到界面上是**空串**：用户看到的是「留空」，而不是一个星号。
    widgetCode: key === LINKAGE_WILDCARD_KEY ? "" : key,
  };
}

/* ------------------------------ 表级配置向导 ------------------------------ */

/// `source_key` 的合法形状。与后端 `domain/source_key.rs::valid_source_key` 同一口径：
/// 小写字母开头、只含 `[a-z0-9_]`、最长 64 字节。它进接口 URL 路径段，所以是**硬契约**
/// （后端会拒，且它是全局唯一索引）。
export const SOURCE_KEY_PATTERN = /^[a-z][a-z0-9_]{0,63}$/;

/// `field_id` → 默认 `source_key`。
///
/// **默认值必须直接可用**：运维不改这一栏也能过。所以派生只做两件确定性的事——
/// 小写化、把 `[^a-z0-9_]` 换成下划线——再保证首字符是字母。
/// 不做「拼上字段名」那种聪明事：字段名可以改名，而 `source_key` 创建后不可改。
export function sourceKeyFromFieldId(fieldId: string): string {
  return fieldId
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9_]/g, "_")
    .replace(/^(?![a-z])/, "f_")
    .slice(0, 64);
}

export function isValidSourceKey(value: string): boolean {
  return SOURCE_KEY_PATTERN.test(value);
}

/// 官方字段类型码 → 人话（`docs/reference/feishu/` 的响应体字段表）。
///
/// **认不出的码也照实带出来**（见 [`fieldTypeLabel`]）：字段列表是全量列出的，
/// 判不判得出来由运维自己决定，把未知类型静默吞掉等于少给了一列。
export const FIELD_TYPE_LABELS: Readonly<Record<number, string>> = {
  1: "文本",
  2: "数字",
  3: "单选",
  4: "多选",
  5: "日期",
  7: "复选框",
  11: "人员",
  13: "电话号码",
  15: "超链接",
  17: "附件",
  18: "关联",
  20: "公式",
  21: "双向关联",
  22: "地理位置",
  23: "群组",
  1001: "创建时间",
  1002: "最后更新时间",
  1003: "创建人",
  1004: "修改人",
  1005: "自动编号",
};

/// 字段类型码 → 展示文本。**数字码永远是文案的一部分**：官方枚举会加新值，
/// 而带出原始码之后，认不出的类型也能被运维和官方文档对上号。
export function fieldTypeLabel(type: number): string {
  return `${type} ${FIELD_TYPE_LABELS[type] ?? "未收录"}`;
}

/// 多维表格数据表（`list_bitable_tables` 的一项）。
export type BitableTable = {
  tableId: string;
  name: string;
};

/// 多维表格视图（`list_bitable_views` 的一项）。
export type BitableView = {
  viewId: string;
  viewName: string;
  viewType: string;
};

/// 多维表格字段（`list_bitable_fields` 的一项）。
export type BitableField = {
  fieldId: string;
  fieldName: string;
  /// 官方类型码。
  type: number;
};

/// 向导里一条**勾选后**的字段绑定。
export type TableWizardField = {
  fieldId: string;
  /// 勾选当时的字段名。**只是展示标签**——身份是 `fieldId`，改名不断链。
  fieldName: string;
  type: number;
  sourceKey: string;
  /// 同表内的父列 `field_id`；无父为 `null`（不是空串——空串会被后端当成「没给」，
  /// 两者在 wire 上恰好同义，但显式的 `null` 让「这条没有父」是写出来的，而不是漏掉的）。
  parentFieldId: string | null;
};

/* ------------------------------ 凭据与体检 ------------------------------ */

/// 一条字段绑定（`list_datasources` 的 `fields[]` 一项，设计 §5 的
/// `feishu_datasource_field` 投影）。
///
/// 表级化之后，`source_key` / 凭据 / 父指针都挂在这一层——一条表级行有 N 条绑定。
export type DatasourceFieldBinding = {
  fieldId: string;
  /// 服务端缓存的字段名。**可能是 null**（首次拉取前还没解析过），
  /// 所以界面拿它当标签时必须允许缺省，不能编一个名字出来。
  fieldName: string | null;
  sourceKey: string;
  parentFieldId: string | null;
  enabled: boolean;
  /// 加密返回：回给飞书的信封是否加密。**绑定级**——与 Token 的存储方式无关。
  encryptEnabled: boolean;
  /// 回给飞书的 `locale`。取值域 `zh_cn` / `en_us` / `ja_jp`，**后端零校验**，
  /// 界面是唯一的守卫（写错会让该字段在所有语言下都取不到文案）。
  defaultLocale: string;
  /// 这条绑定的凭据最近一次轮换时间（unix 秒）。**三态**，与
  /// [`CredentialItem.tokenRotatedAt`] 同一语义——投影没给这一列时是
  /// `undefined`（拿不到），服务端明确回 `null` 才是「从未轮换过」。
  /// 折平这两者会把「不知道」画成「从未轮换」，那是一句可查证的假话。
  tokenRotatedAt?: number | null;
};

/// 体检报告（`health_check`，后端 `MissingField` / `HealthReport` 的投影）。
export type HealthReport = {
  /// 没有已知问题**且**每一项都真的查过。
  ok: boolean;
  /// 勾了但表里已被删除的字段。**改名不在里面**（改名能自愈）。
  missingFields: Array<{ fieldId: string; sourceKey: string }>;
  viewMissing: boolean;
  tableMissing: boolean;
  /// 本轮没能查成的项及其原因。非空时 `ok` 必为 false——
  /// 「查不了」不是「没问题」，界面不能把它读成通过。
  unchecked: string[];
};

/// 拷贝清单的一行：一个字段绑定 + 它的轮换时间。
///
/// **不带 Token 明文**：明文只在用户点「复制 Token」时经回显端点取一次、
/// 或轮换之后由那次响应带回。清单是常驻的一页，凭据不该一直躺在里面。
export type CredentialItem = {
  fieldId: string;
  fieldName: string | null;
  sourceKey: string;
  /// 最近一次轮换时间（unix 秒）。三态：`undefined` = **拿不到**（列表端点的绑定
  /// 投影里没有这一列）、`null` = 明确知道从未轮换过、数字 = 那次的时间。
  /// 把「拿不到」画成「从未轮换」是一句可查证的假话，所以两者分开。
  tokenRotatedAt?: number | null;
  enabled: boolean;
};

/// 表级行的字段绑定 → 拷贝清单行。
///
/// **只列启用中的绑定**：停用的绑定出站会吃 `SOURCE_DISABLED`，把它们摆进
/// 「粘到控件里」的清单，会让人配出一个永远取不到选项的控件。
///
/// 抽成纯函数是为了可测——「哪几行会出现在清单上」正是最容易在改动中无声漂移的地方。
export function credentialItems(item: {
  fields: DatasourceFieldBinding[];
}): CredentialItem[] {
  return item.fields
    .filter((binding) => binding.enabled)
    .map((binding) => ({
      fieldId: binding.fieldId,
      fieldName: binding.fieldName,
      sourceKey: binding.sourceKey,
      // 原样透传三态：数字 = 那次轮换的时间、`null` = 从未轮换、
      // `undefined` = 这一列没拿到。**不能写死**——写死的后果是那一列
      // 永远显示「—」，用户看不到刚换过的凭据是什么时候换的。
      tokenRotatedAt: binding.tokenRotatedAt,
      enabled: binding.enabled,
    }));
}

/// 界面值 → `linkage_mapping` 文本。`null` 返回空串（表示不写这一项）。
export function buildLinkageMapping(value: LinkageFormValue | null): string {
  if (value === null) return "";
  const parentSourceKey = value.parentSourceKey.trim();
  const parentField = value.parentField.trim();
  if (parentSourceKey === "" || parentField === "") return "";
  const key =
    value.widgetCode.trim() === ""
      ? LINKAGE_WILDCARD_KEY
      : value.widgetCode.trim();
  return JSON.stringify({
    [key]: { parent_source_key: parentSourceKey, parent_field: parentField },
  });
}

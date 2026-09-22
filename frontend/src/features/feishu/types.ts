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
  /// 数据源标识，进外部选项接口的 URL 路径段。
  sourceKey: string;
  title: string;
  /// 「加密返回」——加密的是**回给飞书的选项信封**，与 Token 存储无关。
  encryptEnabled: boolean;
  defaultLocale: string;
  status: DatasourceStatus;
  /// 该行最后一次被写入的时间（unix 秒）。
  updatedAt: number;

  /* ------------------------------ 取数配置 ------------------------------ */

  /// 取数方式。`push` 数据源下面这些坐标都是 null。
  ingestMode: string;
  bitableBaseToken: string | null;
  bitableTableId: string | null;
  bitableViewId: string | null;
  /// 取数列的**精确字段名**（不是 field_id——接口要的是名字）。
  bitableFieldName: string | null;
  /// 级联映射的 JSON 原文；无级联时为 null。展示前必须解析。
  linkageMapping: string | null;

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
  /// 内容摘要；用于「内容没变就跳过写库」。
  snapshotDigest: string | null;
};

/// 一个数据源的坐标是否配齐。
///
/// 表单与详情页都用它判断「这个 pull 源能不能拉起来」——**不据此禁用保存**：
/// 允许先建一个坐标不全的源再补齐，比强制一次填完更好用，而坐标缺失的源会被
/// 服务端每轮跳过并告警，是响亮的失败。
export function hasCompleteCoordinates(item: {
  bitableBaseToken: string | null;
  bitableTableId: string | null;
  bitableFieldName: string | null;
}): boolean {
  return [
    item.bitableBaseToken,
    item.bitableTableId,
    item.bitableFieldName,
  ].every((value) => typeof value === "string" && value.trim() !== "");
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
export type DatasourceSortField = "title" | "source_key";

export type DatasourceStatusFilter = "all" | "active" | "disabled";

/// 一次数据源列表查询的全部输入（不含视图选择，视图不改变数据）。
export type DatasourceListQuery = {
  page: number;
  pageSize: number;
  search: string;
  status: DatasourceStatusFilter;
  /// 恒非空：不发排序就是无序分页，翻页会重复/漏行。
  orderBy: OrderByClause[];
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
        "定时拉取需要 Base Token、数据表 ID 与取数列字段名三项齐备。缺任何一项，服务端每轮都会跳过这个数据源。",
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

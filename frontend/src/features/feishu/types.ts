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

/// 状态 → 展示名。
export function statusLabel(status: DatasourceStatus): string {
  return status === "active" ? "启用" : "已停用";
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
};

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
  /// unix 秒；只有持管理 Token 的多维表格自动化会写选项行，
  /// 所以它就是「这行选项最后一次被推送的时间」。
  updatedAt: number;
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

/// 预检回执：`ok` 表示服务端认了这次「数据源 + Token」的组合。
export type TokenPrecheckResult =
  | {
      status: "ok";
      /// 服务端拉到的选项条数；加密返回时为 null（前端解不开密文）。
      optionCount: number | null;
      /// 数据源开了「加密返回」且服务端配了密钥：本次拿到的是一段密文。
      encrypted: boolean;
    }
  | {
      status: "failed";
      code: number | null;
      /// 服务端信封里的 `msg` 原文（这个端点用 `msg` 而不是 `message`）。
      message: string;
      hint: string;
    };

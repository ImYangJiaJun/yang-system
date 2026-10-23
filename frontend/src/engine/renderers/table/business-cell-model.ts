import type {
  TableColumnDisplaySchema,
  TableColumnSchema,
} from "@/engine/contracts/ui-catalog";
import { PRODUCT_LOCALE } from "@/shared/lib/product-locale";
import { formatCell } from "./table-view-model";

type DisplayKind = NonNullable<TableColumnDisplaySchema["kind"]>;
type SemanticTone = "neutral" | "info" | "positive" | "warning" | "negative";

export type CellPresentation = {
  kind: DisplayKind;
  text: string;
  tone: SemanticTone;
  tooltip?: string;
};

export function inferDisplayKind(column: TableColumnSchema): DisplayKind {
  if (column.display?.kind) return column.display.kind;
  if (column.relation) return "relation";
  if (column.widget === "integer" || column.widget === "decimal")
    return "number";
  if (column.widget === "switch") return "boolean";
  if (column.widget === "date_time") return "date_time";
  if (column.widget === "json") return "json";
  return "text";
}

function toDate(value: unknown): Date {
  // Timestamp 契约是 Unix 秒；直接 new Date(String(value)) 会得到 Invalid Date，
  // 让单元格退化成原始秒数。纯数字（数字或数字字符串）一律按秒换算。
  if (typeof value === "number" && Number.isFinite(value)) {
    return new Date(value * 1000);
  }
  if (typeof value === "string" && /^-?\d+(?:\.\d+)?$/.test(value.trim())) {
    return new Date(Number(value) * 1000);
  }
  return new Date(String(value));
}

function formatDate(value: unknown, withTime: boolean): string {
  const date = toDate(value);
  if (Number.isNaN(date.getTime())) return formatCell(value);
  return new Intl.DateTimeFormat(PRODUCT_LOCALE, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    ...(withTime ? { hour: "2-digit", minute: "2-digit", hour12: false } : {}),
  }).format(date);
}

export function resolveCellPresentation(
  column: TableColumnSchema,
  value: unknown,
  relationLabel?: string,
): CellPresentation {
  const kind = inferDisplayKind(column);
  const option = column.display?.options?.find((candidate) =>
    Object.is(candidate.value, value),
  );
  if (option) {
    return {
      kind,
      text: option.label,
      tone: option.tone ?? "neutral",
      tooltip: option.label === String(value) ? undefined : formatCell(value),
    };
  }
  if (kind === "relation" && relationLabel) {
    return {
      kind,
      text: relationLabel,
      tone: "info",
      tooltip: `原始值：${formatCell(value)}`,
    };
  }
  if (kind === "date" || kind === "date_time") {
    return {
      kind,
      text:
        value === null || value === undefined || value === ""
          ? "—"
          : formatDate(value, kind === "date_time"),
      tone: "neutral",
    };
  }
  if (kind === "json") {
    const text = formatCell(value);
    return { kind, text, tone: "neutral", tooltip: text };
  }
  return {
    kind,
    text: formatCell(value),
    tone: kind === "boolean" && value === true ? "positive" : "neutral",
  };
}

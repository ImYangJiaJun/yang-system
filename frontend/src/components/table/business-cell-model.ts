import type {
  TableColumnDisplaySchema,
  TableColumnSchema,
} from "src/contracts/ui-catalog";
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

function formatDate(value: unknown, withTime: boolean): string {
  const date = new Date(String(value));
  if (Number.isNaN(date.getTime())) return formatCell(value);
  return new Intl.DateTimeFormat("zh-CN", {
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

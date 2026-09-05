import { Badge } from "@/shared/ui/badge";
import type { TableColumnSchema } from "@/engine/contracts/ui-catalog";
import { resolveCellPresentation } from "./business-cell-model";

const TONE_VARIANTS = {
  neutral: "secondary",
  info: "default",
  positive: "default",
  warning: "outline",
  negative: "destructive",
} as const;

/// 单元格渲染：展示语义由 business-cell-model（旧 BusinessTableCell 同款）决定。
export function BusinessCell({
  column,
  value,
  relationLabel,
}: {
  column: TableColumnSchema;
  value: unknown;
  relationLabel?: string;
}) {
  const cell = resolveCellPresentation(column, value, relationLabel);
  if (cell.kind === "status" || column.display?.options?.length) {
    return (
      <span title={cell.tooltip}>
        <Badge variant={TONE_VARIANTS[cell.tone]}>{cell.text}</Badge>
      </span>
    );
  }
  return <span title={cell.tooltip}>{cell.text}</span>;
}

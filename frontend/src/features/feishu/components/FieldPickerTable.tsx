/**
 * 向导第三步：把**全表字段**列出来勾选。
 *
 * # 两条刻意的「不做」
 *
 * 1. **不按类型过滤。** 设计决策 D7 的记账里写着：敏感列 allowlist 本次不做，
 *    勾选网格会让「哪些列暴露了」变成一次点击。既然守不住，就不该假装守——
 *    把 30 列原样列出来，由运维自己判断哪一列适合当外部选项的取值来源。
 *    只列「单选/多选」会**静默藏掉**真正该讨论的那些列（目标表里 20 公式 也是
 *    实际在用的取值列）。
 * 2. **不隐藏认不出的类型码。** 官方枚举会加新值，`fieldTypeLabel` 认不出时
 *    仍然把数字码带出来——运维能拿它去对官方文档，而不是对着一列空白猜。
 */

import { Checkbox } from "@/shared/ui/checkbox";
import { Label } from "@/shared/ui/label";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/shared/ui/table";

import type { BitableField } from "../types";
import { fieldTypeLabel } from "../types";

export type FieldPickerTableProps = {
  fields: ReadonlyArray<BitableField>;
  /// 已勾选的 `field_id`。
  selectedIds: ReadonlySet<string>;
  onToggle: (fieldId: string) => void;
  disabled?: boolean;
};

export function FieldPickerTable({
  fields,
  selectedIds,
  onToggle,
  disabled = false,
}: FieldPickerTableProps) {
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead className="w-10">
            <span className="sr-only">勾选</span>
          </TableHead>
          <TableHead>字段名</TableHead>
          <TableHead className="w-32">类型</TableHead>
          <TableHead className="w-40 font-mono text-xs">字段 ID</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {fields.map((field) => {
          const checkboxId = `field-pick-${field.fieldId}`;
          return (
            <TableRow key={field.fieldId} data-slot="field-pick-row">
              <TableCell>
                <Checkbox
                  id={checkboxId}
                  checked={selectedIds.has(field.fieldId)}
                  disabled={disabled}
                  onCheckedChange={() => onToggle(field.fieldId)}
                />
              </TableCell>
              <TableCell>
                {/* Label 的文本就是字段名——勾选框的可访问名等于列名（实测里
                    字段名带 `*` 与斜杠，逐字照抄才不会与真实表名对不上）。 */}
                <Label htmlFor={checkboxId} className="font-normal">
                  {field.fieldName}
                </Label>
              </TableCell>
              <TableCell className="text-xs text-muted-foreground">
                {fieldTypeLabel(field.type)}
              </TableCell>
              <TableCell className="font-mono text-xs text-muted-foreground">
                {field.fieldId}
              </TableCell>
            </TableRow>
          );
        })}
      </TableBody>
    </Table>
  );
}

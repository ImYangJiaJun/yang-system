/**
 * 字段绑定表：一条数据源 = 一张表，这张表就是它的 N 个字段。
 *
 * # 为什么需要它
 *
 * 选项是按**一条绑定**的 `source_key` 索引的，所以详情页原先只能看一个字段——
 * 一条数据源有哪些列、哪几列是级联的父子、哪一列的选项还没落过数据，都没有地方看。
 * 而管理员的判断恰恰发生在整表这一层：「这张表接全了吗、级联通不通、哪一列还是空的」。
 *
 * # 它同时是切换器
 *
 * 点一行就把下面的选项表切到那个字段。**不另做一个下拉**：「像表格一样看整表」
 * 与「切到某个字段」本来就是同一件事的两面，拆成两个控件只会让人在两个地方
 * 各选一次。
 *
 * # 行序是父子相邻，不是按名字排
 *
 * 顺序由 `orderBindingsForDisplay` 给（纯函数、可单测）：父在前、子紧随其后并按
 * 深度缩进。缩进用**内联 padding**而不是空格字符——空格在复制、窄屏与
 * 屏幕阅读器下都会散架。
 */

import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/shared/ui/table";

import type { DatasourceFieldBinding } from "../types";
import { orderBindingsForDisplay } from "../types";
import {
  EncryptBadge,
  DatasourceStatusBadge,
  LocaleBadge,
} from "./StatusBadge";

/// 每一级缩进的宽度（px）。用内联 padding 而不是空格：空格在复制/窄屏/
/// 屏幕阅读器下都会散架，「缩进」这件事必须由布局表达。
const INDENT_PX = 16;

export type FieldBindingsTableProps = {
  bindings: DatasourceFieldBinding[];
  /// 当前正在看选项的那个字段的标识；不在集合里时按「没选」处理。
  selectedSourceKey: string | null;
  onSelect: (sourceKey: string) => void;
};

export function FieldBindingsTable({
  bindings,
  selectedSourceKey,
  onSelect,
}: FieldBindingsTableProps) {
  if (bindings.length === 0) return null;

  // 父字段显示**名字**而不是 `field_id`：这一栏是给人对照飞书那张表看的，
  // 而 `field_id` 在飞书界面上不出现。认不出名字（还没解析出来）时退回 id。
  const nameOf = (fieldId: string): string => {
    const parent = bindings.find((binding) => binding.fieldId === fieldId);
    return parent?.fieldName ?? fieldId;
  };

  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>字段</TableHead>
          <TableHead>标识</TableHead>
          <TableHead>父字段</TableHead>
          <TableHead>加密返回</TableHead>
          <TableHead>默认语言</TableHead>
          <TableHead>状态</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {orderBindingsForDisplay(bindings).map(({ binding, depth }) => {
          const selected = binding.sourceKey === selectedSourceKey;
          return (
            <TableRow
              key={binding.fieldId}
              data-slot="binding-row"
              data-depth={depth}
              data-selected={selected ? "true" : undefined}
              className={selected ? "bg-accent/40" : undefined}
            >
              <TableCell style={{ paddingInlineStart: depth * INDENT_PX }}>
                <button
                  type="button"
                  aria-pressed={selected}
                  title={binding.fieldId}
                  className="rounded-sm text-left font-medium focus-visible:ring-ring/50 focus-visible:ring-[3px] focus-visible:outline-none"
                  onClick={() => onSelect(binding.sourceKey)}
                >
                  {binding.fieldName ?? "（字段名还没解析出来）"}
                </button>
              </TableCell>
              <TableCell className="font-mono text-xs">
                {binding.sourceKey}
              </TableCell>
              <TableCell className="text-xs">
                {binding.parentFieldId === null
                  ? "—"
                  : nameOf(binding.parentFieldId)}
              </TableCell>
              <TableCell>
                {/* 关掉时**什么都不渲染**（不是画一个「未加密」）——那是这一层的
                    真实语义，与列表页原先那两列恒画「—」的假话正好相反。 */}
                <EncryptBadge enabled={binding.encryptEnabled} />
              </TableCell>
              <TableCell>
                <LocaleBadge locale={binding.defaultLocale} />
              </TableCell>
              <TableCell>
                <DatasourceStatusBadge
                  status={binding.enabled ? "active" : "disabled"}
                />
              </TableCell>
            </TableRow>
          );
        })}
      </TableBody>
    </Table>
  );
}

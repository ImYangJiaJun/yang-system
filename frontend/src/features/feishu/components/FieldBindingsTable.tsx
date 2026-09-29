/**
 * 字段树：一条数据源 = 一张表，这张表就是它的 N 个字段，**按树形导航展示**。
 *
 * # 为什么需要它
 *
 * 选项是按**一条绑定**的 `source_key` 索引的，所以详情页原先只能看一个字段——
 * 一条数据源有哪些列、哪几列是级联的父子、哪一列的选项还没落过数据，都没有地方看。
 * 而管理员的判断恰恰发生在整表这一层：「这张表接全了吗、级联通不通、哪一列还是空的」。
 *
 * # 它同时是切换器
 *
 * 点一行就把右边的选项表切到那个字段。**不另做一个下拉**：「像导航一样看整表」
 * 与「切到某个字段」本来就是同一件事的两面，拆成两个控件只会让人在两个地方
 * 各选一次。
 *
 * # 树形而不是表格
 *
 * 曾经它是一张六列表格（字段 / 标识 / 父字段 / 加密返回 / 默认语言 / 状态），
 * 那几列把「切换器」埋进了信息表里，第一眼不知道可以点。现在改为**树形导航**：
 * 每行只留字段名按钮 + 逐字段的状态/加密/语言徽标，父子的层级关系由
 * 缩进 + 连接线表达——级联结构一眼可见，切换动作也回到了页面第二层。
 *
 * # 行序是父子相邻，不是按名字排
 *
 * 顺序由 `orderBindingsForDisplay` 给（纯函数、可单测）：父在前、子紧随其后并按
 * 深度缩进。缩进用**内联 padding**而不是空格字符——空格在复制、窄屏与
 * 屏幕阅读器下都会散架。
 */

import { ChevronRight } from "lucide-react";

import type { DatasourceFieldBinding } from "../types";
import { orderBindingsForDisplay } from "../types";
import {
  EncryptBadge,
  DatasourceStatusBadge,
  LocaleBadge,
} from "./StatusBadge";

/// 每一级缩进的宽度（px）。用内联 padding 而不是空格：空格在复制/窄屏/
/// 屏幕阅读器下都会散架，「缩进」这件事必须由布局表达。
const INDENT_PX = 20;

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

  return (
    <nav aria-label="字段" className="space-y-1">
      {orderBindingsForDisplay(bindings).map(({ binding, depth }) => {
        const selected = binding.sourceKey === selectedSourceKey;
        return (
          <div
            key={binding.fieldId}
            data-slot="binding-row"
            data-depth={depth}
            data-selected={selected ? "true" : undefined}
            className={`group flex items-center gap-2 rounded-md border px-2 py-1.5 transition-colors ${
              selected
                ? "border-accent bg-accent/10"
                : "border-transparent hover:bg-muted/50"
            }`}
          >
            {/* 字段名即切换器：可点击的那一半。层级由缩进表达，父级用点标记。 */}
            <button
              type="button"
              aria-pressed={selected}
              title={binding.fieldId}
              style={{ paddingInlineStart: depth * INDENT_PX }}
              className="flex min-w-0 flex-1 cursor-pointer items-center gap-1 rounded-sm text-left font-medium focus-visible:ring-ring/50 focus-visible:ring-[3px] focus-visible:outline-none"
              onClick={() => onSelect(binding.sourceKey)}
            >
              {depth > 0 ? (
                <span
                  aria-hidden="true"
                  className="text-muted-foreground/70 text-xs leading-none"
                >
                  └
                </span>
              ) : null}
              <span className="truncate">
                {binding.fieldName ?? "（字段名还没解析出来）"}
              </span>
              <ChevronRight
                className={`h-3 w-3 shrink-0 text-muted-foreground transition-transform ${
                  selected ? "rotate-90" : ""
                }`}
                aria-hidden="true"
              />
            </button>
            <span className="flex shrink-0 items-center gap-1.5">
              <EncryptBadge enabled={binding.encryptEnabled} />
              <LocaleBadge locale={binding.defaultLocale} />
              <DatasourceStatusBadge
                status={binding.enabled ? "active" : "disabled"}
              />
            </span>
          </div>
        );
      })}
    </nav>
  );
}

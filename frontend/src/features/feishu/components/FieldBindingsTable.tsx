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
 * 每行只留状态点 + 字段名 + 选项数，父子的层级关系由缩进表达——级联结构一眼可见，
 * 切换动作也回到了页面第二层。
 *
 * # 行序是父子相邻，不是按名字排
 *
 * 顺序由 `orderBindingsForDisplay` 给（纯函数、可单测）：父在前、子紧随其后。
 * 子节点用 `ml-5` 缩进，不用内联 padding 计算——C3/C4 设计采用更简洁的视觉层级。
 */

import type { DatasourceFieldBinding } from "../types";
import { orderBindingsForDisplay } from "../types";

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

  const ordered = orderBindingsForDisplay(bindings);

  return (
    <nav aria-label="字段" className="space-y-1">
      {ordered.map(({ binding, depth }) => {
        const selected = binding.sourceKey === selectedSourceKey;
        return (
          <div
            key={binding.fieldId}
            data-slot="binding-row"
            data-depth={depth}
            data-selected={selected ? "true" : undefined}
            className={`group flex cursor-pointer items-center gap-2 rounded-md px-3 py-2 transition-colors ${
              selected
                ? "bg-blue-50 border-l-[3px] border-blue-500 pl-2.5"
                : "hover:bg-muted/50"
            } ${depth > 0 ? "ml-5" : ""}`}
            onClick={() => onSelect(binding.sourceKey)}
          >
            {/* 状态点 */}
            <span
              className={`h-2 w-2 flex-shrink-0 rounded-full ${
                binding.enabled ? "bg-green-500" : "bg-gray-400"
              }`}
              aria-hidden="true"
            />
            {/* 字段名 */}
            <span
              className={`flex-1 text-sm truncate ${
                selected ? "font-semibold text-blue-700" : "text-foreground"
              }`}
            >
              {binding.fieldName ?? "（字段名还没解析出来）"}
            </span>
            {/* 选项数（这里暂时显示为字段绑定数，实际应该从选项查询获取） */}
            <span className="text-xs text-muted-foreground flex-shrink-0">
              {binding.sourceKey}
            </span>
          </div>
        );
      })}
      {/* 图例 */}
      <div className="mt-4 border-t border-border pt-4">
        <div className="flex items-center gap-3 text-xs text-muted-foreground">
          <div className="flex items-center gap-1.5">
            <span className="h-2 w-2 bg-green-500 rounded-full"></span>
            <span>启用中</span>
          </div>
          <div className="flex items-center gap-1.5">
            <span className="h-2 w-2 bg-yellow-500 rounded-full"></span>
            <span>有问题</span>
          </div>
        </div>
      </div>
    </nav>
  );
}

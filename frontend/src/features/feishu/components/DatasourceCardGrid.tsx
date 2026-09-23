/**
 * 卡片视图：一眼看到「有哪些、哪些停了」。
 *
 * 卡片与台账消费的是**同一份查询状态、同一份数据**（切视图不重新发请求），
 * 所以这里只负责渲染，不持有任何查询状态。
 *
 * 空与加载由本组件自己表达：一个数据源都没有时**不渲染空栅格**（那一屏归四步指引，
 * 由页面负责），加载中按卡片形状出骨架屏。
 */

import { Skeleton } from "@/shared/ui/skeleton";

import type { DatasourceItem } from "../types";
import { identityLabel } from "../types";
import { DatasourceActionsMenu } from "./DatasourceActionsMenu";
import { DatasourceBadgeRow } from "./StatusBadge";

const GRID_CLASS = "grid gap-3 grid-cols-[repeat(auto-fill,minmax(260px,1fr))]";

const SKELETON_COUNT = 6;

export type DatasourceCardGridProps = {
  items: DatasourceItem[];
  /// 有写权限才渲染卡片上的「⋯」菜单（不渲染，不是禁用）。
  canWrite: boolean;
  pending?: boolean;
  onOpen: (item: DatasourceItem) => void;
  onEdit: (item: DatasourceItem) => void;
  onDelete: (item: DatasourceItem) => void;
};

export function DatasourceCardGrid({
  items,
  canWrite,
  pending = false,
  onOpen,
  onEdit,
  onDelete,
}: DatasourceCardGridProps) {
  if (pending) {
    return (
      <div className={GRID_CLASS} aria-busy="true">
        {Array.from({ length: SKELETON_COUNT }, (_, index) => (
          <Skeleton key={index} className="h-28 rounded-xl" />
        ))}
      </div>
    );
  }

  // 空结果集不在这里表达：要么是「一个都没有」（页面渲染四步指引），
  // 要么是「搜索无结果」（页面渲染清除筛选），两者都不该看见空栅格。
  if (items.length === 0) return null;

  return (
    <div className={GRID_CLASS}>
      {items.map((item) => (
        <article
          // `id` 是表级化之后唯一的行身份。解析器会把缺 `id` 的行丢掉，
          // 所以这里的兜底只为满足类型，不会真的撞上。
          key={item.id ?? "—"}
          data-slot="datasource-card"
          className="flex cursor-pointer flex-col gap-2 rounded-xl border border-border bg-card p-4 transition-colors hover:bg-accent/40"
          onClick={() => onOpen(item)}
        >
          <div className="flex items-start justify-between gap-2">
            <button
              type="button"
              className="rounded-sm text-left text-sm font-medium focus-visible:ring-ring/50 focus-visible:ring-[3px] focus-visible:outline-none"
              onClick={(event) => {
                event.stopPropagation();
                onOpen(item);
              }}
            >
              {item.title}
            </button>
            {canWrite ? (
              <DatasourceActionsMenu
                item={item}
                onEdit={onEdit}
                onDelete={onDelete}
              />
            ) : null}
          </div>
          <p className="truncate font-mono text-xs text-muted-foreground">
            {identityLabel(item)}
          </p>
          <DatasourceBadgeRow
            status={item.status}
            // 加密返回 / 默认语言属于**绑定层**，不在这里显示（见 `DatasourceBadgeRow`
            // 的说明）：卡片拿到的表级行上没有这两个键。
          />
        </article>
      ))}
    </div>
  );
}

import type { TableViewSchema } from "@/contracts/ui-catalog";
import {
  buildWhereClause,
  createTableFilters,
  isFilterActive,
  type TableFilters,
} from "./table-view-model";

/**
 * TableView 服务端查询参数契约：与旧前端 useTableQuery 的请求体逐字段一致
 * （page/page_size/search/where/order_by/count_total），纯函数便于单测锁定。
 */

export type SortItem = { field: string; direction: "asc" | "desc" };

export interface TableQueryState {
  page: number;
  pageSize: number;
  search: string;
  filters: TableFilters;
  orderBy: SortItem[];
}

export function initialTableQueryState(view: TableViewSchema): TableQueryState {
  return {
    page: 1,
    pageSize: view.query.default_page_size,
    search: "",
    filters: createTableFilters(
      view.columns.filter((column) =>
        view.query.filter_fields.includes(column.field),
      ),
    ),
    orderBy: view.query.default_sort,
  };
}

export function hasActiveQuery(state: TableQueryState): boolean {
  return (
    Boolean(state.search.trim()) ||
    Object.values(state.filters).some((filter) => isFilterActive(filter))
  );
}

export function buildListActionValues(
  view: TableViewSchema,
  state: TableQueryState,
): Record<string, unknown> {
  // 树视图在无查询条件时拉取整树（上限 max_nodes），有查询条件时回退普通分页。
  const fetchWholeTree = Boolean(view.tree) && !hasActiveQuery(state);
  return {
    page: fetchWholeTree ? 1 : state.page,
    page_size: fetchWholeTree
      ? Math.min(view.tree?.max_nodes ?? 0, view.query.max_page_size)
      : state.pageSize,
    search: state.search.trim() || null,
    // 与旧实现一致：无筛选条件时 where 为 undefined，序列化时整个键省略（不下发 null）。
    where: buildWhereClause(state.filters),
    order_by: state.orderBy.map((item) => ({
      field: item.field,
      direction: item.direction === "desc" ? "Desc" : "Asc",
    })),
    count_total: true,
  };
}

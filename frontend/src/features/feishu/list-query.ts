/**
 * 列表页的查询状态归约：搜索 / 筛选 / 排序 / 分页 / 视图选择。
 *
 * 分两层，都是可单测的纯逻辑 + 一个薄 hook：
 * - 纯函数（`nextStateOnXxx`）负责状态迁移，卡片与台账**共用同一份**状态；
 * - `useListQuery()` 只做 React 接线（useState / 去抖 / localStorage）。
 *
 * 三条硬规则（写错会静默出错数据）：
 * 1. **默认视图是 `ledger`**——预期量级是「可能上百」，台账在找那一条时更强；
 * 2. **默认排序按名称升序 + 唯一键 `id` 收尾**，且请求里恒非空：后端不发排序就没有
 *    确定性全序，翻页会重复或漏行；收尾键必须是**表级行上真的存在**的列
 *    （`id`），发一个不存在的字段名会让整个请求被 `FieldNotFound` 拒掉；
 * 3. **搜索 / 筛选 / 每页条数变化必须回到第 1 页**，否则会停在一个不存在的页码上。
 */

import { useCallback, useEffect, useMemo, useState } from "react";

import type {
  DatasourceListQuery,
  DatasourceStatusFilter,
  DatasourceView,
  ListQueryState,
  OrderByClause,
} from "./types";

/// 视图选择持久化键（照 `shell/density.ts` 的 `yang.density.v1` 先例）。
export const VIEW_STORAGE_KEY = "yang.feishu.datasource.view";

export const DEFAULT_VIEW: DatasourceView = "ledger";

export const PAGE_SIZE_OPTIONS = [10, 20, 50] as const;
export const DEFAULT_PAGE_SIZE = 10;

/// 默认排序：按名称升序（与后端 `list_datasources` 的兜底一致），
/// 再由 [`withStableOrder`] 补上唯一键 `id` 收尾。
export const DEFAULT_ORDER_BY: OrderByClause[] = [
  { field: "title", direction: "Asc" },
];

/// 唯一键收尾用的列，也就是 `feishu_datasource` 表级行的真唯一键。
///
/// **不能是 `source_key`**：表级化之后它属于字段绑定那一层，表级行上没有这一列，
/// 而 `validate_order_field` 对不存在的字段直接 `FieldNotFound`——列表页会整个打不开，
/// 不是「排序不生效」那么轻。
export const STABLE_ORDER_FIELD = "id";

/// 搜索框去抖：搜索词每次按键都会改变结果集，不去抖会把每个字符都打成一次请求。
export const SEARCH_DEBOUNCE_MS = 300;

/// 状态筛选的可选值。
///
/// 放在这里而不是 `components/ListToolbar.tsx` 里，是因为它**不只是一个界面常量**：
/// 「界面上的取值域 ↔ 后端表声明」这条契约要能被对账（`api.test.ts` 的「轴二：枚举
/// 取值域」），所以它必须从一个**非组件模块**导出——react-refresh 的
/// `only-export-components` 不允许组件文件同时导出非组件（`--max-warnings 0` 下
/// 直接失败），而把这个 export 删掉就等于让那条对账失去唯一的取值域来源。
///
/// `all` 是纯界面值（不过滤），不属于后端取值域，对账时要剔掉。
export const STATUS_OPTIONS: ReadonlyArray<{
  value: DatasourceStatusFilter;
  label: string;
}> = [
  { value: "all", label: "全部" },
  { value: "active", label: "启用" },
  { value: "disabled", label: "已停用" },
];

/// 读持久化的视图选择。localStorage 在隐私模式/被禁时访问即抛，必须兜住。
export function loadView(): DatasourceView {
  try {
    const raw = localStorage.getItem(VIEW_STORAGE_KEY);
    return raw === "cards" || raw === "ledger" ? raw : DEFAULT_VIEW;
  } catch {
    return DEFAULT_VIEW;
  }
}

/// 写持久化的视图选择；存储不可用时静默降级为内存态。
export function persistView(view: DatasourceView): void {
  try {
    localStorage.setItem(VIEW_STORAGE_KEY, view);
  } catch {
    // 存储不可用时保持内存态。
  }
}

/// 初始状态。视图从 localStorage 恢复，其余走默认值。
export function initialQueryState(
  view: DatasourceView = loadView(),
): ListQueryState {
  return {
    view,
    page: 1,
    pageSize: DEFAULT_PAGE_SIZE,
    search: "",
    status: "all",
    orderBy: DEFAULT_ORDER_BY,
  };
}

/// 搜索词变化 → 回第 1 页（结果集变了，旧页码可能不存在）。
export function nextStateOnSearch(
  state: ListQueryState,
  search: string,
): ListQueryState {
  return { ...state, search, page: 1 };
}

/// 状态筛选变化 → 回第 1 页。
export function nextStateOnStatus(
  state: ListQueryState,
  status: DatasourceStatusFilter,
): ListQueryState {
  return { ...state, status, page: 1 };
}

/// 每页条数变化 → 回第 1 页（页边界整体变了）。
export function nextStateOnPageSize(
  state: ListQueryState,
  pageSize: number,
): ListQueryState {
  return { ...state, pageSize, page: 1 };
}

/// 翻页。页码合法性由调用方保证（分页控件按 total 夹取）。
export function nextStateOnPage(
  state: ListQueryState,
  page: number,
): ListQueryState {
  return { ...state, page: Math.max(1, page) };
}

/// 切视图：**只换渲染方式**，不动搜索词、页码与排序（设计 §5.4「视图不改变数据」）。
export function nextStateOnView(
  state: ListQueryState,
  view: DatasourceView,
): ListQueryState {
  return { ...state, view };
}

/**
 * 点列头：同一列再点一次反转方向，换一列则从升序开始。
 *
 * **刻意不重置页码**：排序不改变结果集大小，当前页仍然存在；
 * 「回第 1 页」是为结果集变小的场景准备的（搜索/筛选/每页条数）。
 */
export function nextStateOnSort(
  state: ListQueryState,
  field: string,
): ListQueryState {
  const current = state.orderBy.find((clause) => clause.field === field);
  const direction = current?.direction === "Asc" ? "Desc" : "Asc";
  return { ...state, orderBy: [{ field, direction }] };
}

/// 清除搜索与筛选（「清除筛选」动作），视图与排序保持不变。
export function clearFilters(state: ListQueryState): ListQueryState {
  return { ...state, search: "", status: "all", page: 1 };
}

/**
 * 给排序补一个确定性收尾键。
 *
 * 后端只按传入的 `order_by` 排序，不加兜底——`title` 会重名，单键排序下
 * 相同值的行顺序由数据库决定，翻页就会重复或漏行。所以凡是以非唯一列排序，
 * 都在末尾追加唯一键 [`STABLE_ORDER_FIELD`]（`id`）升序。
 */
export function withStableOrder(orderBy: OrderByClause[]): OrderByClause[] {
  const clauses =
    orderBy.length > 0 ? orderBy : (DEFAULT_ORDER_BY as OrderByClause[]);
  const deduped: OrderByClause[] = [];
  for (const clause of clauses) {
    if (!deduped.some((kept) => kept.field === clause.field)) {
      deduped.push(clause);
    }
  }
  if (!deduped.some((clause) => clause.field === STABLE_ORDER_FIELD)) {
    deduped.push({ field: STABLE_ORDER_FIELD, direction: "Asc" });
  }
  return deduped;
}

/// 值去抖：`state.search` 立即更新（输入框要跟手），查询快照滞后一拍。
export function useDebouncedValue<T>(value: T, delayMs: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = window.setTimeout(() => setDebounced(value), delayMs);
    return () => window.clearTimeout(timer);
  }, [value, delayMs]);
  return debounced;
}

export type ListQueryController = {
  /// 界面用的实时状态（搜索框绑定它）。
  state: ListQueryState;
  /// 发请求用的快照：搜索词已去抖，排序已补确定性收尾键。
  query: DatasourceListQuery;
  setSearch: (search: string) => void;
  setStatus: (status: DatasourceStatusFilter) => void;
  setView: (view: DatasourceView) => void;
  setPage: (page: number) => void;
  setPageSize: (pageSize: number) => void;
  setSort: (field: string) => void;
  clearFilters: () => void;
};

/// 列表页查询状态的唯一持有者：卡片视图与台账视图消费同一个实例。
export function useListQuery(): ListQueryController {
  const [state, setState] = useState<ListQueryState>(() => initialQueryState());
  const debouncedSearch = useDebouncedValue(state.search, SEARCH_DEBOUNCE_MS);

  const query = useMemo<DatasourceListQuery>(
    () => ({
      page: state.page,
      pageSize: state.pageSize,
      search: debouncedSearch,
      status: state.status,
      orderBy: withStableOrder(state.orderBy),
    }),
    [state.page, state.pageSize, debouncedSearch, state.status, state.orderBy],
  );

  const setView = useCallback((view: DatasourceView) => {
    persistView(view);
    setState((previous) => nextStateOnView(previous, view));
  }, []);

  const setSearch = useCallback((search: string) => {
    setState((previous) => nextStateOnSearch(previous, search));
  }, []);

  const setStatus = useCallback((status: DatasourceStatusFilter) => {
    setState((previous) => nextStateOnStatus(previous, status));
  }, []);

  const setPage = useCallback((page: number) => {
    setState((previous) => nextStateOnPage(previous, page));
  }, []);

  const setPageSize = useCallback((pageSize: number) => {
    setState((previous) => nextStateOnPageSize(previous, pageSize));
  }, []);

  const setSort = useCallback((field: string) => {
    setState((previous) => nextStateOnSort(previous, field));
  }, []);

  const resetFilters = useCallback(() => {
    setState((previous) => clearFilters(previous));
  }, []);

  return {
    state,
    query,
    setSearch,
    setStatus,
    setView,
    setPage,
    setPageSize,
    setSort,
    clearFilters: resetFilters,
  };
}

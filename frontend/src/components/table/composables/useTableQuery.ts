import {
  computed,
  onScopeDispose,
  ref,
  toValue,
  watch,
  type MaybeRefOrGetter,
} from "vue";
import { ApiError, invokeAction, type SessionContext } from "src/api/client";
import { parseTableData } from "src/contracts/table-data";
import type {
  ActionDemoSchema,
  TableFilterOperator,
  TableViewSchema,
} from "src/contracts/ui-catalog";
import { captureFrontendError } from "src/observability/error-reporter";
import {
  buildWhereClause,
  createTableFilters,
  isFilterActive,
  pageSizeOptions as buildPageSizeOptions,
  type TableFilters,
} from "../table-view-model";

export interface TablePaginationState {
  sortBy: string | null;
  descending: boolean;
  page: number;
  rowsPerPage: number;
}

interface UseTableQueryOptions {
  view: MaybeRefOrGetter<TableViewSchema>;
  dataAction: MaybeRefOrGetter<ActionDemoSchema | undefined>;
  session: MaybeRefOrGetter<SessionContext>;
  invoke?: typeof invokeAction;
  onRowsLoaded?: (rows: Array<Record<string, unknown>>) => void | Promise<void>;
  onLoadError?: () => void;
}

export function useTableQuery(options: UseTableQueryOptions) {
  const initialView = toValue(options.view);
  const invoke = options.invoke ?? invokeAction;
  const rows = ref<Array<Record<string, unknown>>>([]);
  const total = ref(0);
  const page = ref(1);
  const pageSize = ref(initialView.query.default_page_size);
  const search = ref("");
  const filters = ref<TableFilters>(createFilters(initialView));
  const filtersOpen = ref(false);
  const orderBy = ref(initialView.query.default_sort);
  const loading = ref(false);
  const error = ref("");
  const tablePagination = ref<TablePaginationState>(
    initialPagination(initialView),
  );
  let activeRequest: { id: number; controller: AbortController } | undefined;
  let nextRequestId = 0;

  const filterColumns = computed(() => {
    const view = toValue(options.view);
    return view.columns.filter((column) =>
      view.query.filter_fields.includes(column.field),
    );
  });
  const activeFilterColumns = computed(() =>
    filterColumns.value.filter((column) =>
      isFilterActive(filters.value[column.field]),
    ),
  );
  const activeFilterCount = computed(
    () => activeFilterColumns.value.length + (search.value.trim() ? 1 : 0),
  );
  const hasActiveQuery = computed(
    () =>
      Boolean(search.value.trim()) ||
      Object.values(filters.value).some((filter) => isFilterActive(filter)),
  );
  const pageCount = computed(() =>
    Math.max(1, Math.ceil(total.value / pageSize.value)),
  );
  const pageSizeOptions = computed(() =>
    buildPageSizeOptions(toValue(options.view).query.max_page_size),
  );

  async function load() {
    const view = toValue(options.view);
    const dataAction = toValue(options.dataAction);
    if (!dataAction) {
      activeRequest?.controller.abort();
      activeRequest = undefined;
      error.value = `目录未提供数据 Action：${view.data_action}`;
      rows.value = [];
      total.value = 0;
      options.onLoadError?.();
      return;
    }
    activeRequest?.controller.abort();
    const request = {
      id: ++nextRequestId,
      controller: new AbortController(),
    };
    activeRequest = request;
    loading.value = true;
    error.value = "";
    let relatedRequestId: string | undefined;
    try {
      const result = await invoke(
        dataAction,
        {
          page: view.tree && !hasActiveQuery.value ? 1 : page.value,
          page_size:
            view.tree && !hasActiveQuery.value
              ? Math.min(view.tree.max_nodes, view.query.max_page_size)
              : pageSize.value,
          search: search.value.trim() || null,
          where: buildWhereClause(filters.value),
          order_by: orderBy.value.map((item) => ({
            field: item.field,
            direction: item.direction === "desc" ? "Desc" : "Asc",
          })),
          count_total: true,
        },
        { ...toValue(options.session) },
        request.controller.signal,
      );
      relatedRequestId = result.requestId;
      if (activeRequest?.id !== request.id) return;
      if (result.kind !== "json") throw new Error("数据 Action 必须返回 JSON");
      const data = parseTableData(result.data);
      rows.value = data.items;
      total.value = data.total ?? data.items.length;
      void options.onRowsLoaded?.(data.items);
    } catch (cause) {
      if (
        activeRequest?.id !== request.id ||
        (cause instanceof Error && cause.name === "AbortError")
      ) {
        return;
      }
      error.value = cause instanceof Error ? cause.message : String(cause);
      if (!(cause instanceof ApiError)) {
        captureFrontendError(cause, {
          kind: "contract",
          operation: dataAction.operation_id,
          relatedRequestId,
        });
      }
      rows.value = [];
      total.value = 0;
      options.onLoadError?.();
    } finally {
      if (activeRequest?.id === request.id) {
        activeRequest = undefined;
        loading.value = false;
      }
    }
  }

  function applyQuery() {
    page.value = 1;
    void load();
  }

  function changeSort(next: TablePaginationState) {
    const changed =
      next.sortBy !== tablePagination.value.sortBy ||
      next.descending !== tablePagination.value.descending;
    tablePagination.value = next;
    if (!changed) return;
    orderBy.value = next.sortBy
      ? [{ field: next.sortBy, direction: next.descending ? "desc" : "asc" }]
      : toValue(options.view).query.default_sort;
    applyQuery();
  }

  function changePage(next: number) {
    page.value = next;
    void load();
  }

  function changePageSize(next: number | null) {
    if (!next) return;
    pageSize.value = next;
    page.value = 1;
    void load();
  }

  function setFilterOperator(field: string, operator: TableFilterOperator) {
    filters.value[field] = {
      operator,
      value:
        operator === "range" ? [null, null] : operator === "in" ? [] : null,
    };
  }

  function setFilterValue(field: string, value: unknown) {
    const filter = filters.value[field];
    if (filter) filter.value = value;
  }

  function clearFilter(field: string) {
    const filter = filters.value[field];
    if (!filter) return;
    filter.value =
      filter.operator === "range"
        ? [null, null]
        : filter.operator === "in"
          ? []
          : null;
  }

  function clearAllQuery() {
    search.value = "";
    filters.value = createTableFilters(filterColumns.value);
    applyQuery();
  }

  function resetForView(view: TableViewSchema) {
    page.value = 1;
    pageSize.value = view.query.default_page_size;
    search.value = "";
    filters.value = createFilters(view);
    filtersOpen.value = false;
    orderBy.value = view.query.default_sort;
    tablePagination.value = initialPagination(view);
  }

  function dispose() {
    activeRequest?.controller.abort();
    activeRequest = undefined;
  }

  const stopViewWatcher = watch(
    () => toValue(options.view),
    (view) => {
      resetForView(view);
      void load();
    },
    { immediate: true },
  );
  const stopSessionWatcher = watch(
    () => toValue(options.session),
    () => void load(),
    { deep: true },
  );

  onScopeDispose(() => {
    stopViewWatcher();
    stopSessionWatcher();
    dispose();
  });

  return {
    rows,
    total,
    page,
    pageSize,
    search,
    filters,
    filtersOpen,
    orderBy,
    loading,
    error,
    tablePagination,
    filterColumns,
    activeFilterColumns,
    activeFilterCount,
    hasActiveQuery,
    pageCount,
    pageSizeOptions,
    load,
    applyQuery,
    changeSort,
    changePage,
    changePageSize,
    setFilterOperator,
    setFilterValue,
    clearFilter,
    clearAllQuery,
    dispose,
  };
}

function createFilters(view: TableViewSchema) {
  return createTableFilters(
    view.columns.filter((column) =>
      view.query.filter_fields.includes(column.field),
    ),
  );
}

function initialPagination(view: TableViewSchema): TablePaginationState {
  const initialSort = view.query.default_sort[0];
  return {
    sortBy: initialSort?.field ?? null,
    descending: initialSort?.direction === "desc",
    page: 1,
    rowsPerPage: 0,
  };
}

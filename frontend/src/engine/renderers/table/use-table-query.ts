import { useState } from "react";
import {
  keepPreviousData,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";

import { invokeAction } from "@/engine/http/client";
import { useSessionCredentials } from "@/engine/session/use-session";
import { parseTableData } from "@/engine/contracts/table-data";
import type {
  ActionDemoSchema,
  TableViewSchema,
} from "@/engine/contracts/ui-catalog";
import {
  buildListActionValues,
  initialTableQueryState,
  type TableQueryState,
} from "./table-query";
import { pageSizeOptions } from "./table-view-model";

/**
 * TableView 数据查询 hook：状态变化即触发 TanStack Query 重新拉取；
 * 查询参数构造契约见 table-query.ts（与旧 useTableQuery 逐字段一致）。
 */
export function useTableQuery(
  view: TableViewSchema,
  dataAction: ActionDemoSchema | undefined,
) {
  const session = useSessionCredentials();
  const queryClient = useQueryClient();
  const [state, setState] = useState<TableQueryState>(() =>
    initialTableQueryState(view),
  );

  const query = useQuery({
    queryKey: ["table-data", view.view_id, state],
    placeholderData: keepPreviousData,
    queryFn: async ({ signal }) => {
      if (!dataAction) {
        throw new Error(`目录未提供数据 Action：${view.data_action}`);
      }
      const result = await invokeAction(
        dataAction,
        buildListActionValues(view, state),
        session,
        signal,
      );
      if (result.kind !== "json") throw new Error("数据 Action 必须返回 JSON");
      return parseTableData(result.data);
    },
  });

  const rows = query.data?.items ?? [];
  const total = query.data?.total ?? rows.length;
  const pageCount = Math.max(1, Math.ceil(total / state.pageSize));

  const patch = (partial: Partial<TableQueryState>) =>
    setState((prev) => ({ ...prev, ...partial }));

  return {
    state,
    rows,
    total,
    pageCount,
    pageSizeOptions: pageSizeOptions(view.query.max_page_size),
    loading: query.isPending || query.isPlaceholderData,
    error: query.error instanceof Error ? query.error.message : "",
    reload: () =>
      queryClient.invalidateQueries({ queryKey: ["table-data", view.view_id] }),
    applyQuery: () => patch({ page: 1 }),
    changePage: (page: number) => patch({ page }),
    changePageSize: (pageSize: number) => patch({ pageSize, page: 1 }),
    changeSort: (field: string | null, descending: boolean) =>
      patch({
        page: 1,
        orderBy: field
          ? [{ field, direction: descending ? "desc" : "asc" }]
          : view.query.default_sort,
      }),
    setSearch: (search: string) => patch({ search }),
    setFilters: (filters: TableQueryState["filters"]) => patch({ filters }),
  };
}

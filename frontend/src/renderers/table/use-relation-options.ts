import { useQuery } from "@tanstack/react-query";

import { invokeAction } from "@/api/client";
import { useSessionCredentials } from "@/api/use-session";
import { parseRelationOptions } from "@/contracts/table-data";
import type { ActionDemoSchema, TableViewSchema } from "@/contracts/ui-catalog";
import type { SourceRow } from "./table-view-model";

export type RelationOption = { value: string | number; label: string };

/**
 * 关系列标签加载：把当前页行里出现的关系值作为 selected 一次性取回标签，
 * 请求契约与旧 useRelationOptions 一致（search/selected/filter/page/limit）。
 */
export function useRelationOptions(
  view: TableViewSchema,
  actions: ActionDemoSchema[],
  rows: SourceRow[],
) {
  const session = useSessionCredentials();
  const actionById = new Map(
    actions.map((action) => [action.operation_id, action]),
  );

  const requests = view.columns.flatMap((column) => {
    if (!column.relation) return [];
    const selected = new Set<string | number>();
    for (const row of rows) {
      const value = row[column.field];
      if (typeof value === "string" || typeof value === "number") {
        selected.add(value);
      }
    }
    return [
      {
        operationId: column.relation.operation_id,
        action: actionById.get(column.relation.operation_id),
        selected: [...selected],
      },
    ];
  });

  const query = useQuery({
    queryKey: [
      "relation-options",
      view.view_id,
      requests.map((request) => [request.operationId, request.selected]),
    ],
    enabled: requests.length > 0,
    queryFn: async ({ signal }) => {
      const entries = await Promise.all(
        requests.map(async (request) => {
          if (!request.action) {
            throw new Error(`目录缺少关系 Action：${request.operationId}`);
          }
          const result = await invokeAction(
            request.action,
            {
              search: null,
              selected: request.selected,
              filter: {},
              page: 1,
              limit: Math.min(100, Math.max(20, request.selected.length)),
            },
            session,
            signal,
          );
          if (result.kind !== "json")
            throw new Error("关系 Action 必须返回 JSON");
          return [
            request.operationId,
            parseRelationOptions(result.data).items,
          ] as const;
        }),
      );
      return Object.fromEntries(entries) as Record<string, RelationOption[]>;
    },
  });

  const labelFor = (operationId: string, value: unknown): string | undefined =>
    query.data?.[operationId]?.find(
      (candidate) =>
        Object.is(candidate.value, value) ||
        String(candidate.value) === String(value),
    )?.label;

  return { labelFor, loading: query.isPending, error: query.error };
}

import { onScopeDispose, ref, toValue, type MaybeRefOrGetter } from "vue";
import { ApiError, invokeAction, type SessionContext } from "src/api/client";
import { parseRelationOptions } from "src/contracts/table-data";
import type {
  ActionDemoSchema,
  TableViewSchema,
} from "src/contracts/ui-catalog";
import { captureFrontendError } from "src/observability/error-reporter";
import { flattenDisplayRows } from "../table-view-model";

type RelationOption = { value: string | number; label: string };

interface UseRelationOptionsOptions {
  view: MaybeRefOrGetter<TableViewSchema>;
  actions: MaybeRefOrGetter<ActionDemoSchema[]>;
  session: MaybeRefOrGetter<SessionContext>;
  invoke?: typeof invokeAction;
}

export function useRelationOptions(options: UseRelationOptionsOptions) {
  const invoke = options.invoke ?? invokeAction;
  const relationOptions = ref<Record<string, RelationOption[]>>({});
  const relationErrors = ref<string[]>([]);
  let activeRequest: { id: number; controller: AbortController } | undefined;
  let nextRequestId = 0;

  async function load(sourceRows: Array<Record<string, unknown>>) {
    activeRequest?.controller.abort();
    const request = {
      id: ++nextRequestId,
      controller: new AbortController(),
    };
    activeRequest = request;
    const actionById = new Map(
      toValue(options.actions).map((action) => [action.operation_id, action]),
    );
    const flatRows = flattenDisplayRows(sourceRows).map((row) => row.data);
    const requests = new Map<string, Set<string | number>>();
    for (const column of toValue(options.view).columns) {
      if (!column.relation) continue;
      const selected = requests.get(column.relation.operation_id) ?? new Set();
      for (const row of flatRows) {
        const value = row[column.field];
        if (typeof value === "string" || typeof value === "number") {
          selected.add(value);
        }
      }
      requests.set(column.relation.operation_id, selected);
    }
    if (!requests.size) {
      relationOptions.value = {};
      relationErrors.value = [];
      activeRequest = undefined;
      return;
    }
    const results = await Promise.all(
      [...requests].map(async ([operationId, selected]) => {
        const action = actionById.get(operationId);
        if (!action) {
          return {
            operationId,
            error: `目录缺少关系 Action：${operationId}`,
          };
        }
        let relatedRequestId: string | undefined;
        try {
          const result = await invoke(
            action,
            {
              search: null,
              selected: [...selected],
              filter: {},
              page: 1,
              limit: Math.min(100, Math.max(20, selected.size)),
            },
            { ...toValue(options.session) },
            request.controller.signal,
          );
          relatedRequestId = result.requestId;
          if (result.kind !== "json") {
            throw new Error("关系 Action 必须返回 JSON");
          }
          return {
            operationId,
            options: parseRelationOptions(result.data).items,
          };
        } catch (cause) {
          if (cause instanceof Error && cause.name === "AbortError") {
            return { operationId, aborted: true };
          }
          if (!(cause instanceof ApiError)) {
            captureFrontendError(cause, {
              kind: "contract",
              operation: operationId,
              relatedRequestId,
            });
          }
          return {
            operationId,
            error: cause instanceof Error ? cause.message : String(cause),
          };
        }
      }),
    );
    if (activeRequest?.id !== request.id) return;
    relationOptions.value = Object.fromEntries(
      results
        .filter((result) => result.options)
        .map((result) => [result.operationId, result.options!]),
    );
    relationErrors.value = results.flatMap((result) =>
      result.error ? [result.error] : [],
    );
    activeRequest = undefined;
  }

  function labelFor(operationId: string, value: unknown): string | undefined {
    return relationOptions.value[operationId]?.find(
      (candidate) =>
        Object.is(candidate.value, value) ||
        String(candidate.value) === String(value),
    )?.label;
  }

  function clear() {
    activeRequest?.controller.abort();
    activeRequest = undefined;
    relationOptions.value = {};
    relationErrors.value = [];
  }

  onScopeDispose(clear);

  return {
    relationOptions,
    relationErrors,
    load,
    labelFor,
    clear,
  };
}

import {
  asJsonSchema,
  effectiveSchema,
  initialObject,
  type JsonSchemaNode,
} from "@/engine/contracts/json-schema";
import type {
  ActionDemoSchema,
  TableColumnSchema,
} from "@/engine/contracts/ui-catalog";

/**
 * 无视图模块的 primaryAction 数据卡片回退（旧 ModulePage.vue 同名语义平移）：
 * 无 TableView 的模块退化为「主 Action → 通用数据卡片」。
 */

export function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

export function inputFields(action: ActionDemoSchema): string[] {
  const root = asJsonSchema(action.input_schema);
  return Object.keys(effectiveSchema(root, root).properties ?? {});
}

export function outputProperties(
  action: ActionDemoSchema | undefined,
  rowsOnly: boolean,
): Record<string, JsonSchemaNode> {
  if (!action) return {};
  const root = asJsonSchema(action.output_schema);
  const output = effectiveSchema(root, root);
  if (!rowsOnly) return output.properties ?? {};
  const items = output.properties?.items;
  if (!items) return {};
  const collection = effectiveSchema(root, items);
  const item = collection.items
    ? effectiveSchema(root, collection.items)
    : collection;
  return item.properties ?? {};
}

/// 从输出行 Schema 推导列展示契约（旧 ModulePage.vue 的 schemaColumn）。
export function schemaColumn(
  field: string,
  node: JsonSchemaNode | undefined,
): TableColumnSchema {
  const type = Array.isArray(node?.type)
    ? node.type.find((candidate) => candidate !== "null")
    : node?.type;
  const kind =
    node?.format === "date-time"
      ? ("date_time" as const)
      : node?.format === "date"
        ? ("date" as const)
        : type === "boolean"
          ? ("boolean" as const)
          : type === "number" || type === "integer"
            ? ("number" as const)
            : type === "object" || type === "array"
              ? ("json" as const)
              : ("text" as const);
  return {
    field,
    title: node?.title || field,
    description: node?.description || "",
    widget:
      type === "integer"
        ? "integer"
        : type === "number"
          ? "decimal"
          : type === "boolean"
            ? "switch"
            : kind === "date_time" || kind === "date"
              ? "date_time"
              : kind === "json"
                ? "json"
                : "text",
    required: false,
    searchable: false,
    filterable: false,
    sortable: true,
    display: { kind },
  };
}

/// 主 Action 请求参数：initialObject + 声明了才下发的 page/limit/search。
export function buildPrimaryActionValues(
  action: ActionDemoSchema,
  state: { page: number; pageSize: number; search: string },
): Record<string, unknown> {
  const fields = new Set(inputFields(action));
  return {
    ...initialObject(action.input_schema),
    ...(fields.has("page") ? { page: state.page } : {}),
    ...(fields.has("limit") ? { limit: state.pageSize } : {}),
    ...(fields.has("search") && state.search.trim()
      ? { search: state.search.trim() }
      : {}),
  };
}

/// 面板无法自动补全的必填参数：既不是自动下发的 page/limit/search，输入契约里
/// 也没有默认值。这类参数必须人工填写（查询条件条），且主 Action 不能自动调用——
/// 否则每次进页都发一个注定 400 的请求（如要求必填路径参数的直授查询，
/// 2026-10-05 回归）。
export function manualPrimaryParams(
  action: ActionDemoSchema | undefined,
): string[] {
  if (!action) return [];
  const autoSupplied = new Set(["page", "limit", "search"]);
  const root = asJsonSchema(action.input_schema);
  const resolved = effectiveSchema(root, root);
  const properties = resolved.properties ?? {};
  const needsManual = (name: string): boolean => {
    if (autoSupplied.has(name)) return false;
    const property = properties[name];
    return !(
      property !== undefined &&
      effectiveSchema(root, property).default !== undefined
    );
  };
  const names = new Set<string>();
  for (const parameter of action.params ?? []) {
    if (parameter.required && needsManual(parameter.name))
      names.add(parameter.name);
  }
  for (const name of resolved.required ?? []) {
    if (needsManual(name)) names.add(name);
  }
  return [...names].sort();
}

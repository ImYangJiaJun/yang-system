import { z } from "zod";
import { ContractError, type TableViewSchema } from "./ui-catalog";

const rowSchema = z.record(z.string(), z.unknown());

const tableDataSchema = z.object({
  items: z.array(rowSchema),
  page: z.number().int().positive(),
  page_size: z.number().int().positive(),
  total: z.number().int().nonnegative().nullable().optional(),
});

const relationOptionsResponseSchema = z.object({
  items: z.array(
    z.object({
      value: z.union([z.string(), z.number()]),
      label: z.string(),
    }),
  ),
  page: z.number().int().positive(),
  limit: z.number().int().positive(),
  total: z.number().int().nonnegative().nullable().optional(),
});

export type TableData = z.infer<typeof tableDataSchema>;
export type RelationOptionsResponse = z.infer<
  typeof relationOptionsResponseSchema
>;
export type TreeRow = Record<string, unknown> & { children?: TreeRow[] };

function parseContract<T>(
  schema: z.ZodType<T>,
  payload: unknown,
  label: string,
): T {
  const parsed = schema.safeParse(payload);
  if (!parsed.success) {
    throw new ContractError(`${label}契约校验失败`, [
      ...parsed.error.issues.map(
        (issue) => `${issue.path.join(".") || "<root>"}: ${issue.message}`,
      ),
    ]);
  }
  return parsed.data;
}

export function parseTableData(payload: unknown): TableData {
  return parseContract(tableDataSchema, payload, "TableView 数据");
}

export function parseRelationOptions(
  payload: unknown,
): RelationOptionsResponse {
  return parseContract(relationOptionsResponseSchema, payload, "关系 options ");
}

function stableKey(value: unknown): string | undefined {
  if (typeof value === "string") return `s:${value}`;
  if (typeof value === "number" && Number.isFinite(value)) return `n:${value}`;
  return undefined;
}

export function buildTreeRows(
  rows: Array<Record<string, unknown>>,
  tree: NonNullable<TableViewSchema["tree"]>,
): TreeRow[] {
  if (rows.length > tree.max_nodes) {
    throw new ContractError(
      `TreeView 返回 ${rows.length} 个节点，超过契约上限 ${tree.max_nodes}`,
    );
  }

  const nodes = new Map<string, TreeRow>();
  for (const row of rows) {
    const key = stableKey(row[tree.id_field]);
    if (!key) throw new ContractError(`TreeView 节点缺少有效 ${tree.id_field}`);
    if (nodes.has(key))
      throw new ContractError(`TreeView 节点 ID 重复：${key}`);
    nodes.set(key, { ...row });
  }

  const roots: TreeRow[] = [];
  const parents = new Map<string, string>();
  for (const [key, node] of nodes) {
    const parentValue = node[tree.parent_field];
    if (
      parentValue === null ||
      parentValue === undefined ||
      parentValue === ""
    ) {
      roots.push(node);
      continue;
    }
    const parentKey = stableKey(parentValue);
    const parent = parentKey ? nodes.get(parentKey) : undefined;
    if (!parent || !parentKey) {
      throw new ContractError(`TreeView 节点 ${key} 引用了不存在的父节点`);
    }
    parents.set(key, parentKey);
    const children = (parent.children ??= []);
    children.push(node);
  }

  for (const key of nodes.keys()) {
    const visited = new Set<string>();
    let cursor: string | undefined = key;
    while (cursor) {
      if (visited.has(cursor)) {
        throw new ContractError(`TreeView 存在循环父子关系：${key}`);
      }
      visited.add(cursor);
      cursor = parents.get(cursor);
    }
  }
  return roots;
}

import {
  asJsonSchema,
  effectiveSchema,
  initialObject,
} from "src/contracts/json-schema";
import type {
  ActionDemoSchema,
  FormFieldSchema,
} from "src/contracts/ui-catalog";

export type SourceRow = Record<string, unknown>;

export type DisplayRow = {
  data: SourceRow;
  depth: number;
  key: string;
};

export function flattenDisplayRows(
  source: SourceRow[],
  depth = 0,
  path = "root",
): DisplayRow[] {
  return source.flatMap((row, index) => {
    const rowPath = `${path}.${index}`;
    const children = Array.isArray(row.children)
      ? (row.children as SourceRow[])
      : [];
    return [
      { data: row, depth, key: rowPath },
      ...flattenDisplayRows(children, depth + 1, rowPath),
    ];
  });
}

function parseFilterValue(value: string): unknown {
  const trimmed = value.trim();
  if (!trimmed) return undefined;
  try {
    return JSON.parse(trimmed);
  } catch {
    return trimmed;
  }
}

export function buildWhereClause(filters: Record<string, string>): unknown {
  const conditions = Object.entries(filters)
    .map(([field, value]) => ({ field, value: parseFilterValue(value) }))
    .filter((item) => item.value !== undefined)
    .map((item) => ({ type: "eq", field: item.field, value: item.value }));
  if (conditions.length === 0) return undefined;
  return conditions.length === 1 ? conditions[0] : { type: "and", conditions };
}

export function buildActionInitialValues(
  action: ActionDemoSchema,
  fields: FormFieldSchema[],
  row?: SourceRow,
): Record<string, unknown> {
  const initial = initialObject(action.input_schema);
  if (!row) return initial;
  const rootSchema = asJsonSchema(action.input_schema);
  const inputFields = Object.keys(
    effectiveSchema(rootSchema, rootSchema).properties ?? {},
  );
  const writeOnly = new Set(
    fields.filter((field) => field.write_only).map((field) => field.field),
  );
  const readableRow = Object.fromEntries(
    Object.entries(row).filter(([name]) => !writeOnly.has(name)),
  );
  for (const name of inputFields) {
    if (name in readableRow) initial[name] = readableRow[name];
  }
  if (
    initial.data &&
    typeof initial.data === "object" &&
    !Array.isArray(initial.data)
  ) {
    initial.data = { ...initial.data, ...readableRow };
  }
  return initial;
}

export function pageSizeOptions(maxPageSize: number) {
  return Array.from(new Set([10, 20, 50, maxPageSize]))
    .sort((left, right) => left - right)
    .map((value) => ({ label: `${value} / 页`, value }));
}

export function formatCell(value: unknown): string {
  if (value === null || value === undefined || value === "") return "—";
  if (typeof value === "boolean") return value ? "是" : "否";
  return typeof value === "object" ? JSON.stringify(value) : String(value);
}

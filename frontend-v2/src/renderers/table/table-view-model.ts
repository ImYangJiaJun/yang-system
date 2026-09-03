import {
  asJsonSchema,
  effectiveSchema,
  initialObject,
} from "@/contracts/json-schema";
import type {
  ActionDemoSchema,
  ActionPresentationSchema,
  FormFieldSchema,
  TableColumnSchema,
  TableFilterOperator,
} from "@/contracts/ui-catalog";

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

export type TableFilterEntry = {
  operator: TableFilterOperator;
  value: unknown;
};

export type TableFilters = Record<string, TableFilterEntry>;

export type PresentedActionGroups = {
  primary?: ActionPresentationSchema;
  secondary: ActionPresentationSchema[];
  overflow: ActionPresentationSchema[];
};

function parseFilterValue(value: unknown): unknown {
  if (value === null || value === undefined) return undefined;
  if (typeof value !== "string") return value;
  const trimmed = value.trim();
  if (!trimmed) return undefined;
  try {
    return JSON.parse(trimmed);
  } catch {
    return trimmed;
  }
}

export function createTableFilters(columns: TableColumnSchema[]): TableFilters {
  return Object.fromEntries(
    columns.map((column) => [
      column.field,
      {
        operator: column.filter?.default_operator ?? "eq",
        value:
          column.filter?.default_operator === "range" ? [null, null] : null,
      },
    ]),
  );
}

export function isFilterActive(filter: TableFilterEntry | undefined): boolean {
  if (!filter) return false;
  if (filter.operator === "range") {
    return (
      Array.isArray(filter.value) &&
      filter.value.length >= 2 &&
      parseFilterValue(filter.value[0]) !== undefined &&
      parseFilterValue(filter.value[1]) !== undefined
    );
  }
  if (filter.operator === "in") {
    return (
      Array.isArray(filter.value) &&
      filter.value.some((value) => parseFilterValue(value) !== undefined)
    );
  }
  return parseFilterValue(filter.value) !== undefined;
}

export function buildWhereClause(filters: TableFilters): unknown {
  const conditions: Array<Record<string, unknown>> = [];
  for (const [field, filter] of Object.entries(filters)) {
    if (!isFilterActive(filter)) continue;
    if (filter.operator === "contains") {
      conditions.push({
        type: "like",
        field,
        pattern: `%${String(filter.value).trim()}%`,
      });
      continue;
    }
    if (filter.operator === "in") {
      const values = (filter.value as unknown[])
        .map(parseFilterValue)
        .filter((value) => value !== undefined);
      conditions.push({ type: "in", field, values });
      continue;
    }
    if (filter.operator === "range") {
      const [lo, hi] = filter.value as unknown[];
      conditions.push({
        type: "between",
        field,
        lo: parseFilterValue(lo),
        hi: parseFilterValue(hi),
      });
      continue;
    }
    conditions.push({
      type: "eq",
      field,
      value: parseFilterValue(filter.value),
    });
  }
  if (conditions.length === 0) return undefined;
  return conditions.length === 1 ? conditions[0] : { type: "and", conditions };
}

export function groupPresentedActions(
  actions: ActionPresentationSchema[],
  directLimit: number,
): PresentedActionGroups {
  const sorted = actions
    .map((action, index) => ({ action, index }))
    .sort(
      (left, right) =>
        (left.action.appearance?.order ?? 0) -
          (right.action.appearance?.order ?? 0) || left.index - right.index,
    )
    .map(({ action }) => action);
  const inferredOverflow = (action: ActionPresentationSchema) =>
    action.appearance?.overflow === true ||
    (action.appearance?.overflow !== false &&
      (action.appearance?.emphasis === "danger" ||
        Boolean(action.confirmation)));
  const directCandidates = sorted.filter((action) => !inferredOverflow(action));
  const explicitPrimary = directCandidates.find(
    (action) => action.appearance?.emphasis === "primary",
  );
  const primary = explicitPrimary ?? directCandidates[0];
  const direct = [
    ...(primary ? [primary] : []),
    ...directCandidates.filter((action) => action !== primary),
  ].slice(0, Math.max(0, directLimit));
  return {
    primary,
    secondary: direct.filter((action) => action !== primary),
    overflow: sorted.filter((action) => !direct.includes(action)),
  };
}

export function buildActionInitialValues(
  action: ActionDemoSchema,
  fields: FormFieldSchema[],
  row?: SourceRow,
  recordParameter?: string | null,
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
  if (recordParameter && row.id !== undefined) {
    initial[recordParameter] = row.id;
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

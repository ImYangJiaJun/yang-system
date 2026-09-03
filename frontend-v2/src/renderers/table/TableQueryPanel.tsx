import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import type {
  TableFilterOperator,
  TableViewSchema,
} from "@/contracts/ui-catalog";
import type { TableQueryState } from "./table-query";
import { isFilterActive, type TableFilters } from "./table-view-model";

/**
 * 查询面板：搜索 + 按 view.query.filter_fields 声明的筛选字段。
 * 操作符与取值契约见 table-view-model.ts（buildWhereClause）。
 */

const OPERATOR_LABELS: Record<TableFilterOperator, string> = {
  eq: "等于",
  contains: "包含",
  in: "属于",
  range: "区间",
};

interface TableQueryPanelProps {
  view: TableViewSchema;
  state: TableQueryState;
  dense: boolean;
  onDenseChange: (dense: boolean) => void;
  onStateChange: (patch: Partial<TableQueryState>) => void;
  onApply: () => void;
  onReset: () => void;
}

export function TableQueryPanel({
  view,
  state,
  dense,
  onDenseChange,
  onStateChange,
  onApply,
  onReset,
}: TableQueryPanelProps) {
  const filterColumns = view.columns.filter((column) =>
    view.query.filter_fields.includes(column.field),
  );
  const activeFilterCount =
    filterColumns.filter((column) =>
      isFilterActive(state.filters[column.field]),
    ).length + (state.search.trim() ? 1 : 0);

  const setFilter = (field: string, entry: TableFilters[string]) =>
    onStateChange({ filters: { ...state.filters, [field]: entry } });

  return (
    <div className="mb-3 space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        {view.query.search_fields.length > 0 && (
          <Input
            className="w-64"
            placeholder={`搜索 ${view.query.search_fields.join(" / ")}`}
            value={state.search}
            aria-label="搜索"
            onChange={(event) => onStateChange({ search: event.target.value })}
            onKeyDown={(event) => {
              if (event.key === "Enter") onApply();
            }}
          />
        )}
        <Button variant="outline" size="sm" onClick={onApply}>
          查询
        </Button>
        <Button variant="ghost" size="sm" onClick={onReset}>
          重置
        </Button>
        {activeFilterCount > 0 && (
          <span className="text-xs text-muted-foreground">
            {activeFilterCount} 个活动条件
          </span>
        )}
        <label className="ml-auto flex items-center gap-2 text-sm text-muted-foreground">
          <Checkbox
            checked={dense}
            onCheckedChange={(checked) => onDenseChange(checked === true)}
            aria-label="紧凑模式"
          />
          紧凑
        </label>
      </div>
      {filterColumns.length > 0 && (
        <div className="flex flex-wrap items-end gap-3 rounded-md border border-border p-3">
          {filterColumns.map((column) => {
            const filter = state.filters[column.field] ?? {
              operator: column.filter?.default_operator ?? "eq",
              value: null,
            };
            const operators = column.filter?.operators ?? ["eq"];
            return (
              <div key={column.field} className="flex items-end gap-2">
                <div className="space-y-1">
                  <Label className="text-xs">
                    {column.title || column.field}
                  </Label>
                  <div className="flex items-center gap-1">
                    {operators.length > 1 && (
                      <select
                        className="h-9 rounded-md border border-input bg-transparent px-2 text-sm"
                        aria-label={`${column.title || column.field} 操作符`}
                        value={filter.operator}
                        onChange={(event) =>
                          setFilter(column.field, {
                            operator: event.target.value as TableFilterOperator,
                            value:
                              event.target.value === "range"
                                ? [null, null]
                                : event.target.value === "in"
                                  ? []
                                  : null,
                          })
                        }
                      >
                        {operators.map((operator) => (
                          <option key={operator} value={operator}>
                            {OPERATOR_LABELS[operator]}
                          </option>
                        ))}
                      </select>
                    )}
                    {filter.operator === "range" ? (
                      <>
                        <Input
                          className="w-28"
                          aria-label={`${column.title || column.field} 下限`}
                          value={
                            Array.isArray(filter.value)
                              ? String(filter.value[0] ?? "")
                              : ""
                          }
                          onChange={(event) => {
                            const [, hi] = Array.isArray(filter.value)
                              ? filter.value
                              : [null, null];
                            setFilter(column.field, {
                              operator: "range",
                              value: [event.target.value || null, hi],
                            });
                          }}
                        />
                        <Input
                          className="w-28"
                          aria-label={`${column.title || column.field} 上限`}
                          value={
                            Array.isArray(filter.value)
                              ? String(filter.value[1] ?? "")
                              : ""
                          }
                          onChange={(event) => {
                            const [lo] = Array.isArray(filter.value)
                              ? filter.value
                              : [null, null];
                            setFilter(column.field, {
                              operator: "range",
                              value: [lo, event.target.value || null],
                            });
                          }}
                        />
                      </>
                    ) : filter.operator === "in" ? (
                      <Input
                        className="w-40"
                        placeholder="逗号分隔多个值"
                        aria-label={`${column.title || column.field} 筛选值`}
                        value={
                          Array.isArray(filter.value)
                            ? filter.value.map(String).join(",")
                            : ""
                        }
                        onChange={(event) =>
                          setFilter(column.field, {
                            operator: "in",
                            value: event.target.value
                              .split(",")
                              .map((item) => item.trim())
                              .filter(Boolean),
                          })
                        }
                      />
                    ) : (
                      <Input
                        className="w-40"
                        aria-label={`${column.title || column.field} 筛选值`}
                        value={
                          filter.value === null || filter.value === undefined
                            ? ""
                            : String(filter.value)
                        }
                        onChange={(event) =>
                          setFilter(column.field, {
                            operator: filter.operator,
                            value: event.target.value,
                          })
                        }
                      />
                    )}
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

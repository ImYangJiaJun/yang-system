import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import { Label } from "@/shared/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/shared/ui/select";
import type {
  TableColumnSchema,
  TableFilterOperator,
  TableViewSchema,
} from "@/engine/contracts/ui-catalog";
import type { TableQueryState } from "./table-query";
import { isFilterActive, type TableFilters } from "./table-view-model";

/**
 * 查询面板：搜索 + 按 view.query.filter_fields 声明的筛选字段。
 * 操作符与取值契约见 table-view-model.ts（buildWhereClause）。
 *
 * 取值控件按列语义选择，而不是一律文本框：日期区间用日期选择器（提交 Unix 秒，
 * 与展示侧同一口径），数值区间用数字输入，枚举列给出可选值。以前日期区间是两个
 * 裸文本框，用户得自己猜该填秒还是 ISO 字符串。
 */

const OPERATOR_LABELS: Record<TableFilterOperator, string> = {
  eq: "等于",
  contains: "包含",
  in: "属于",
  range: "区间",
};

/// 列是否按时间语义筛选：display.kind 优先，其次 widget 提示。
function isTimeColumn(column: TableColumnSchema): boolean {
  return (
    column.display?.kind === "date" ||
    column.display?.kind === "date_time" ||
    column.widget === "date_time"
  );
}

function isNumberColumn(column: TableColumnSchema): boolean {
  return (
    column.display?.kind === "number" ||
    column.widget === "integer" ||
    column.widget === "decimal"
  );
}

function toEpochSeconds(value: string): number | null {
  if (!value) return null;
  const date = new Date(value);
  return Number.isNaN(date.getTime())
    ? null
    : Math.floor(date.getTime() / 1000);
}

function fromEpochSeconds(value: unknown): string {
  if (typeof value === "number" && Number.isFinite(value)) {
    const date = new Date(value * 1000);
    const pad = (input: number) => String(input).padStart(2, "0");
    return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
  }
  return typeof value === "string" ? value : "";
}

interface TableQueryPanelProps {
  view: TableViewSchema;
  state: TableQueryState;
  onStateChange: (patch: Partial<TableQueryState>) => void;
  onApply: () => void;
  onReset: () => void;
}

export function TableQueryPanel({
  view,
  state,
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
                      <Select
                        value={filter.operator}
                        onValueChange={(next) =>
                          setFilter(column.field, {
                            operator: next as TableFilterOperator,
                            value:
                              next === "range"
                                ? [null, null]
                                : next === "in"
                                  ? []
                                  : null,
                          })
                        }
                      >
                        <SelectTrigger
                          size="sm"
                          className="w-24"
                          aria-label={`${column.title || column.field} 操作符`}
                        >
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          {operators.map((operator) => (
                            <SelectItem key={operator} value={operator}>
                              {OPERATOR_LABELS[operator]}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    )}
                    {filter.operator === "range" ? (
                      <>
                        <FilterValueInput
                          column={column}
                          ariaLabel={`${column.title || column.field} 下限`}
                          value={
                            Array.isArray(filter.value) ? filter.value[0] : null
                          }
                          onChange={(next) => {
                            const [, hi] = Array.isArray(filter.value)
                              ? filter.value
                              : [null, null];
                            setFilter(column.field, {
                              operator: "range",
                              value: [next, hi],
                            });
                          }}
                        />
                        <FilterValueInput
                          column={column}
                          ariaLabel={`${column.title || column.field} 上限`}
                          value={
                            Array.isArray(filter.value) ? filter.value[1] : null
                          }
                          onChange={(next) => {
                            const [lo] = Array.isArray(filter.value)
                              ? filter.value
                              : [null, null];
                            setFilter(column.field, {
                              operator: "range",
                              value: [lo, next],
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
                      <FilterValueInput
                        column={column}
                        ariaLabel={`${column.title || column.field} 筛选值`}
                        value={filter.value}
                        onChange={(next) =>
                          setFilter(column.field, {
                            operator: filter.operator,
                            value: next ?? null,
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

/// 单个筛选值的输入控件：按列语义选择，避免让用户猜格式。
/// - 时间列：日期选择器，提交/回显统一走 Unix 秒；
/// - 数值列：数字输入，提交 number；
/// - 其余：文本输入。
function FilterValueInput({
  column,
  ariaLabel,
  value,
  onChange,
}: {
  column: TableColumnSchema;
  ariaLabel: string;
  value: unknown;
  onChange: (value: unknown) => void;
}) {
  if (isTimeColumn(column)) {
    return (
      <Input
        type="date"
        className="w-36"
        aria-label={ariaLabel}
        value={fromEpochSeconds(value)}
        onChange={(event) => onChange(toEpochSeconds(event.target.value))}
      />
    );
  }
  if (isNumberColumn(column)) {
    return (
      <Input
        type="number"
        className="w-28"
        aria-label={ariaLabel}
        value={
          typeof value === "number" || typeof value === "string"
            ? String(value)
            : ""
        }
        onChange={(event) => {
          const raw = event.target.value;
          onChange(raw === "" ? null : Number(raw));
        }}
      />
    );
  }
  return (
    <Input
      className="w-40"
      aria-label={ariaLabel}
      value={value === null || value === undefined ? "" : String(value)}
      onChange={(event) => onChange(event.target.value || null)}
    />
  );
}

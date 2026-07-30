<script setup lang="ts">
import type {
  TableColumnSchema,
  TableFilterOperator,
  TableViewSchema,
} from "src/contracts/ui-catalog";
import type { TableFilters } from "./table-view-model";

type RelationOption = { value: string | number; label: string };

const props = defineProps<{
  view: TableViewSchema;
  search: string;
  filters: TableFilters;
  filtersOpen: boolean;
  activeFilterCount: number;
  activeFilterColumns: TableColumnSchema[];
  hasActiveQuery: boolean;
  relationOptions: Record<string, RelationOption[]>;
  visibleColumnNames: string[];
  denseTable: boolean;
}>();

const emit = defineEmits<{
  "update:search": [value: string];
  "update:filtersOpen": [value: boolean];
  "update:denseTable": [value: boolean];
  apply: [];
  setFilterOperator: [field: string, operator: TableFilterOperator];
  setFilterValue: [field: string, value: unknown];
  clearFilter: [field: string];
  clearAll: [];
  setColumnVisible: [field: string, visible: boolean];
}>();

const filterOperatorLabels: Record<TableFilterOperator, string> = {
  eq: "等于",
  contains: "包含",
  in: "任一值",
  range: "区间",
};

function filterOperators(column: TableColumnSchema) {
  return column.filter?.operators ?? (["eq"] as TableFilterOperator[]);
}

function filterOperatorOptions(column: TableColumnSchema) {
  return filterOperators(column).map((value) => ({
    label: filterOperatorLabels[value],
    value,
  }));
}

function filterWidget(column: TableColumnSchema) {
  return column.filter?.widget ?? column.widget;
}

function filterInputType(column: TableColumnSchema) {
  const widget = filterWidget(column);
  if (widget === "integer" || widget === "decimal") return "number";
  if (widget === "date_time") return "datetime-local";
  return "text";
}

function filterValueOptions(column: TableColumnSchema) {
  if (column.relation) {
    return props.relationOptions[column.relation.operation_id] ?? [];
  }
  if (column.display?.options?.length) {
    return column.display.options.map((option) => ({
      label: option.label,
      value: option.value,
    }));
  }
  if (filterWidget(column) === "switch") {
    return [
      { label: "是", value: true },
      { label: "否", value: false },
    ];
  }
  return [];
}

function usesOptionSelect(column: TableColumnSchema) {
  return (
    Boolean(column.relation) ||
    filterValueOptions(column).length > 0 ||
    filterWidget(column) === "radio"
  );
}

function scalarFilterValue(field: string): string | number | null {
  const value = props.filters[field]?.value;
  return typeof value === "string" || typeof value === "number" ? value : null;
}

function rangeValue(field: string, index: number) {
  const value = props.filters[field]?.value;
  return Array.isArray(value) ? value[index] : null;
}

function setRangeValue(field: string, index: number, value: unknown) {
  const current = props.filters[field];
  if (!current) return;
  const range = Array.isArray(current.value)
    ? [...current.value]
    : [null, null];
  range[index] = value;
  emit("setFilterValue", field, range);
}

function filterSummary(column: TableColumnSchema) {
  const filter = props.filters[column.field];
  if (!filter) return column.title || column.field;
  const value = Array.isArray(filter.value)
    ? filter.value.filter((item) => item !== null && item !== "").join(" ～ ")
    : String(filter.value);
  return `${column.title || column.field} ${filterOperatorLabels[filter.operator]} ${value}`;
}
</script>

<template>
  <q-card flat bordered class="query-card">
    <q-card-section class="table-query-section">
      <div class="table-query-bar">
        <q-input
          v-if="view.query.search_fields.length"
          :model-value="search"
          dense
          outlined
          clearable
          class="table-search-input"
          :placeholder="`搜索 ${view.query.search_fields.join('、')}`"
          @update:model-value="emit('update:search', String($event ?? ''))"
          @keyup.enter="emit('apply')"
        >
          <template #prepend><q-icon name="search" /></template>
        </q-input>
        <q-btn
          v-if="view.query.filter_fields.length"
          outline
          color="primary"
          icon="tune"
          :label="activeFilterCount ? `筛选 ${activeFilterCount}` : '筛选'"
          :aria-expanded="filtersOpen"
          @click="emit('update:filtersOpen', !filtersOpen)"
        />
        <q-btn unelevated color="primary" label="查询" @click="emit('apply')" />
        <q-space />
        <q-btn flat round color="grey-7" icon="view_column" aria-label="列设置">
          <q-menu>
            <q-list class="column-settings-list">
              <q-item-label header>显示字段</q-item-label>
              <q-item
                v-for="column in view.columns"
                :key="column.field"
                tag="label"
                clickable
              >
                <q-item-section side>
                  <q-checkbox
                    :model-value="visibleColumnNames.includes(column.field)"
                    :aria-label="`显示${column.title || column.field}列`"
                    :disable="
                      visibleColumnNames.length === 1 &&
                      visibleColumnNames.includes(column.field)
                    "
                    @update:model-value="
                      emit('setColumnVisible', column.field, Boolean($event))
                    "
                  />
                </q-item-section>
                <q-item-section>{{
                  column.title || column.field
                }}</q-item-section>
              </q-item>
              <q-separator />
              <q-item tag="label" clickable>
                <q-item-section side>
                  <q-toggle
                    :model-value="denseTable"
                    aria-label="紧凑行高"
                    @update:model-value="
                      emit('update:denseTable', Boolean($event))
                    "
                  />
                </q-item-section>
                <q-item-section>紧凑行高</q-item-section>
              </q-item>
            </q-list>
          </q-menu>
        </q-btn>
      </div>

      <q-slide-transition>
        <div v-show="filtersOpen" class="advanced-filter-panel">
          <div
            v-for="column in view.columns.filter((item) =>
              view.query.filter_fields.includes(item.field),
            )"
            :key="column.field"
            class="filter-control"
          >
            <label>{{ column.title || column.field }}</label>
            <div class="filter-control-fields">
              <q-select
                v-if="filterOperators(column).length > 1"
                :model-value="filters[column.field]?.operator"
                :options="filterOperatorOptions(column)"
                dense
                outlined
                emit-value
                map-options
                class="filter-operator-select"
                aria-label="筛选方式"
                @update:model-value="
                  emit('setFilterOperator', column.field, $event)
                "
              />
              <template v-if="filters[column.field]?.operator === 'range'">
                <q-input
                  :model-value="rangeValue(column.field, 0)"
                  :type="filterInputType(column)"
                  dense
                  outlined
                  clearable
                  placeholder="起始值"
                  :aria-label="`${column.title || column.field}筛选起始值`"
                  @update:model-value="setRangeValue(column.field, 0, $event)"
                  @keyup.enter="emit('apply')"
                />
                <span class="range-separator">至</span>
                <q-input
                  :model-value="rangeValue(column.field, 1)"
                  :type="filterInputType(column)"
                  dense
                  outlined
                  clearable
                  placeholder="结束值"
                  :aria-label="`${column.title || column.field}筛选结束值`"
                  @update:model-value="setRangeValue(column.field, 1, $event)"
                  @keyup.enter="emit('apply')"
                />
              </template>
              <q-select
                v-else-if="filters[column.field]?.operator === 'in'"
                :model-value="filters[column.field]?.value"
                :options="filterValueOptions(column)"
                dense
                outlined
                clearable
                multiple
                use-chips
                use-input
                emit-value
                map-options
                new-value-mode="add-unique"
                :placeholder="column.filter?.placeholder || '输入后按回车添加'"
                :aria-label="`${column.title || column.field}筛选值`"
                @update:model-value="
                  emit('setFilterValue', column.field, $event)
                "
              />
              <q-select
                v-else-if="usesOptionSelect(column)"
                :model-value="filters[column.field]?.value"
                :options="filterValueOptions(column)"
                dense
                outlined
                clearable
                emit-value
                map-options
                :placeholder="column.filter?.placeholder || '请选择'"
                :aria-label="`${column.title || column.field}筛选值`"
                @update:model-value="
                  emit('setFilterValue', column.field, $event)
                "
              />
              <q-input
                v-else
                :model-value="scalarFilterValue(column.field)"
                :type="filterInputType(column)"
                dense
                outlined
                clearable
                :placeholder="column.filter?.placeholder || '输入筛选值'"
                :aria-label="`${column.title || column.field}筛选值`"
                @update:model-value="
                  emit('setFilterValue', column.field, $event)
                "
                @keyup.enter="emit('apply')"
              />
            </div>
          </div>
        </div>
      </q-slide-transition>

      <div v-if="hasActiveQuery" class="active-filter-row">
        <span>当前条件</span>
        <q-chip
          v-if="search.trim()"
          removable
          color="blue-grey-1"
          text-color="blue-grey-9"
          @remove="emit('update:search', '')"
        >
          关键词：{{ search.trim() }}
        </q-chip>
        <q-chip
          v-for="column in activeFilterColumns"
          :key="column.field"
          removable
          color="blue-grey-1"
          text-color="blue-grey-9"
          @remove="emit('clearFilter', column.field)"
        >
          {{ filterSummary(column) }}
        </q-chip>
        <q-btn
          flat
          dense
          color="primary"
          label="清除全部"
          @click="emit('clearAll')"
        />
      </div>
    </q-card-section>
  </q-card>
</template>

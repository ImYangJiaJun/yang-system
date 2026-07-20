<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { Dialog, Notify, type QTableColumn } from "quasar";
import {
  invokeAction,
  type InvocationResult,
  type SessionContext,
} from "src/api/client";
import { buildTreeRows, parseTableData } from "src/contracts/table-data";
import type {
  ActionDemoSchema,
  ActionPresentationSchema,
  TableColumnSchema,
  TableFilterOperator,
  TableViewSchema,
} from "src/contracts/ui-catalog";
import JsonSchemaForm from "components/form/JsonSchemaForm.vue";
import {
  buildActionInitialValues,
  buildWhereClause,
  createTableFilters,
  flattenDisplayRows,
  formatCell,
  groupPresentedActions,
  isFilterActive,
  pageSizeOptions as buildPageSizeOptions,
  type DisplayRow,
  type TableFilters,
} from "./table-view-model";

const props = defineProps<{
  view: TableViewSchema;
  actions: ActionDemoSchema[];
  session: SessionContext;
}>();
const emit = defineEmits<{
  customAction: [
    presentation: ActionPresentationSchema,
    row?: Record<string, unknown>,
  ];
}>();

const rows = ref<Array<Record<string, unknown>>>([]);
const total = ref(0);
const page = ref(1);
const pageSize = ref(props.view.query.default_page_size);
const search = ref("");
const filters = ref<TableFilters>(
  createTableFilters(
    props.view.columns.filter((column) =>
      props.view.query.filter_fields.includes(column.field),
    ),
  ),
);
const filtersOpen = ref(false);
const visibleColumnNames = ref(
  props.view.columns.map((column) => column.field),
);
const denseTable = ref(false);
const orderBy = ref(props.view.query.default_sort);
const selectedDisplayRows = ref<DisplayRow[]>([]);
const loading = ref(false);
const error = ref("");
const actionDialog = ref(false);
const actionLoading = ref(false);
const activePresentation = ref<ActionPresentationSchema>();
const activeAction = ref<ActionDemoSchema>();
const actionValues = ref<Record<string, unknown>>({});
const tablePagination = ref({
  sortBy: null as string | null,
  descending: false,
  page: 1,
  rowsPerPage: 0,
});
let controller: AbortController | undefined;
let actionController: AbortController | undefined;

const actionById = computed(
  () => new Map(props.actions.map((action) => [action.operation_id, action])),
);
const dataAction = computed(() => actionById.value.get(props.view.data_action));
const presentations = computed(() =>
  props.view.action_presentations.filter(
    (item) => item.availability?.state !== "hidden",
  ),
);
const toolbarActions = computed(() =>
  presentations.value.filter((item) => item.placement === "toolbar"),
);
const rowActions = computed(() =>
  presentations.value.filter((item) => item.placement === "row"),
);
const bulkActions = computed(() =>
  presentations.value.filter((item) => item.placement === "bulk"),
);
const toolbarActionGroups = computed(() =>
  groupPresentedActions(toolbarActions.value, 2),
);
const directToolbarActions = computed(() => [
  ...(toolbarActionGroups.value.primary
    ? [toolbarActionGroups.value.primary]
    : []),
  ...toolbarActionGroups.value.secondary,
]);
const rowActionGroups = computed(() =>
  groupPresentedActions(rowActions.value, 1),
);
const directRowActions = computed(() =>
  rowActionGroups.value.primary ? [rowActionGroups.value.primary] : [],
);
const filterColumns = computed(() =>
  props.view.columns.filter((column) =>
    props.view.query.filter_fields.includes(column.field),
  ),
);
const activeFilterColumns = computed(() =>
  filterColumns.value.filter((column) =>
    isFilterActive(filters.value[column.field]),
  ),
);
const activeFilterCount = computed(
  () => activeFilterColumns.value.length + (search.value.trim() ? 1 : 0),
);
const hasActiveQuery = computed(
  () =>
    Boolean(search.value.trim()) ||
    Object.values(filters.value).some((filter) => isFilterActive(filter)),
);
const treeResult = computed(() => {
  if (!props.view.tree || hasActiveQuery.value)
    return { rows: rows.value, warning: "" };
  try {
    return { rows: buildTreeRows(rows.value, props.view.tree), warning: "" };
  } catch (cause) {
    return {
      rows: rows.value,
      warning: `${cause instanceof Error ? cause.message : String(cause)}；已安全降级为普通表格`,
    };
  }
});
const displayRows = computed(() => treeResult.value.rows);
const selectedRows = computed(() =>
  selectedDisplayRows.value.map((row) => row.data),
);
const qTableRows = computed(() => {
  return flattenDisplayRows(displayRows.value);
});
const tableColumns = computed<QTableColumn<DisplayRow>[]>(() => {
  const columns: QTableColumn<DisplayRow>[] = props.view.columns.map(
    (column) => ({
      name: column.field,
      label: column.title || column.field,
      field: (row) => row.data[column.field],
      align: column.display?.align ?? "left",
      sortable: column.sortable,
      format: (value) => formatCell(value),
      style: column.display?.width
        ? `width: ${column.display.width}px`
        : column.display?.min_width
          ? `min-width: ${column.display.min_width}px`
          : undefined,
      headerStyle: column.display?.width
        ? `width: ${column.display.width}px`
        : column.display?.min_width
          ? `min-width: ${column.display.min_width}px`
          : undefined,
    }),
  );
  if (rowActions.value.length) {
    columns.push({
      name: "__actions",
      label: "操作",
      field: () => "",
      align: "right",
    });
  }
  return columns;
});
const firstColumnName = computed(() => props.view.columns[0]?.field);
const pageCount = computed(() =>
  Math.max(1, Math.ceil(total.value / pageSize.value)),
);
const pageSizeOptions = computed(() =>
  buildPageSizeOptions(props.view.query.max_page_size),
);
const visibleColumns = computed(() => [
  ...visibleColumnNames.value,
  ...(rowActions.value.length ? ["__actions"] : []),
]);

const filterOperatorLabels: Record<TableFilterOperator, string> = {
  eq: "等于",
  contains: "包含",
  in: "任一值",
  range: "区间",
};

function actionLabel(presentation: ActionPresentationSchema) {
  return presentation.title || presentation.operation_id;
}

function isDangerAction(presentation: ActionPresentationSchema) {
  return (
    presentation.appearance?.emphasis === "danger" ||
    Boolean(presentation.confirmation)
  );
}

function actionColor(presentation: ActionPresentationSchema) {
  return isDangerAction(presentation) ? "negative" : "primary";
}

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
    filterValueOptions(column).length > 0 || filterWidget(column) === "radio"
  );
}

function setFilterOperator(field: string, operator: TableFilterOperator) {
  filters.value[field] = {
    operator,
    value: operator === "range" ? [null, null] : operator === "in" ? [] : null,
  };
}

function setFilterValue(field: string, value: unknown) {
  const filter = filters.value[field];
  if (filter) filter.value = value;
}

function rangeValue(field: string, index: number) {
  const value = filters.value[field]?.value;
  return Array.isArray(value) ? value[index] : null;
}

function setRangeValue(field: string, index: number, value: unknown) {
  const current = filters.value[field];
  if (!current) return;
  const range = Array.isArray(current.value)
    ? [...current.value]
    : [null, null];
  range[index] = value;
  current.value = range;
}

function clearFilter(field: string) {
  const filter = filters.value[field];
  if (!filter) return;
  filter.value =
    filter.operator === "range"
      ? [null, null]
      : filter.operator === "in"
        ? []
        : null;
}

function filterSummary(column: TableColumnSchema) {
  const filter = filters.value[column.field];
  if (!filter) return column.title || column.field;
  const value = Array.isArray(filter.value)
    ? filter.value.filter((item) => item !== null && item !== "").join(" ～ ")
    : String(filter.value);
  return `${column.title || column.field} ${filterOperatorLabels[filter.operator]} ${value}`;
}

function clearAllQuery() {
  search.value = "";
  filters.value = createTableFilters(filterColumns.value);
  applyQuery();
}

function setColumnVisible(field: string, visible: boolean) {
  if (visible && !visibleColumnNames.value.includes(field)) {
    visibleColumnNames.value.push(field);
    return;
  }
  if (!visible && visibleColumnNames.value.length > 1) {
    visibleColumnNames.value = visibleColumnNames.value.filter(
      (name) => name !== field,
    );
  }
}

async function load() {
  if (!dataAction.value) {
    error.value = `目录未提供数据 Action：${props.view.data_action}`;
    rows.value = [];
    return;
  }
  controller?.abort();
  controller = new AbortController();
  loading.value = true;
  error.value = "";
  try {
    const result = await invokeAction(
      dataAction.value,
      {
        page: props.view.tree && !hasActiveQuery.value ? 1 : page.value,
        page_size:
          props.view.tree && !hasActiveQuery.value
            ? Math.min(
                props.view.tree.max_nodes,
                props.view.query.max_page_size,
              )
            : pageSize.value,
        search: search.value.trim() || null,
        where: buildWhereClause(filters.value),
        order_by: orderBy.value,
        count_total: true,
      },
      props.session,
      controller.signal,
    );
    if (result.kind !== "json") throw new Error("数据 Action 必须返回 JSON");
    const data = parseTableData(result.data);
    rows.value = data.items;
    total.value = data.total ?? data.items.length;
  } catch (cause) {
    if (cause instanceof Error && cause.name === "AbortError") return;
    error.value = cause instanceof Error ? cause.message : String(cause);
    rows.value = [];
    total.value = 0;
  } finally {
    loading.value = false;
  }
}

async function openAction(
  presentation: ActionPresentationSchema,
  row?: Record<string, unknown>,
) {
  if (presentation.interaction === "custom") {
    emit("customAction", presentation, row);
    return;
  }
  const action = actionById.value.get(presentation.operation_id);
  if (!action) {
    Notify.create({
      type: "negative",
      message: `目录缺少 Action：${presentation.operation_id}`,
    });
    return;
  }
  activePresentation.value = presentation;
  activeAction.value = action;
  actionValues.value = buildActionInitialValues(
    action,
    props.view.form.fields,
    row,
  );
  if (presentation.placement === "bulk") {
    actionValues.value.selected = selectedRows.value;
  }
  if (presentation.interaction === "form") {
    actionDialog.value = true;
    return;
  }
  await submitAction();
}

async function confirmAction(
  presentation: ActionPresentationSchema,
): Promise<boolean> {
  if (!presentation.confirmation) return true;
  return new Promise((resolve) => {
    Dialog.create({
      title: presentation.confirmation?.title,
      message: presentation.confirmation?.message ?? "",
      ok: { label: "确认", color: "negative" },
      cancel: { label: "取消", flat: true },
      persistent: true,
    })
      .onOk(() => resolve(true))
      .onCancel(() => resolve(false))
      .onDismiss(() => resolve(false));
  });
}

function handleAttachment(result: InvocationResult) {
  if (!result.blobUrl) return;
  if (result.kind === "preview") {
    window.open(result.blobUrl, "_blank", "noopener,noreferrer");
    return;
  }
  const anchor = document.createElement("a");
  anchor.href = result.blobUrl;
  anchor.download = result.filename ?? "download";
  anchor.click();
  window.setTimeout(() => URL.revokeObjectURL(result.blobUrl!), 0);
}

async function submitAction() {
  const action = activeAction.value;
  const presentation = activePresentation.value;
  if (!action || !presentation || actionLoading.value) return;
  actionLoading.value = true;
  actionController?.abort();
  actionController = new AbortController();
  try {
    if (!(await confirmAction(presentation))) return;
    const result = await invokeAction(
      action,
      actionValues.value,
      props.session,
      actionController.signal,
    );
    handleAttachment(result);
    if (result.kind === "redirect" && result.location) {
      window.location.assign(result.location);
    }
    Notify.create({ type: "positive", message: result.message || "操作成功" });
    actionDialog.value = false;
    await load();
  } catch (cause) {
    if (cause instanceof Error && cause.name === "AbortError") return;
    Notify.create({
      type: "negative",
      message: cause instanceof Error ? cause.message : String(cause),
    });
  } finally {
    actionLoading.value = false;
  }
}

function applyQuery() {
  page.value = 1;
  void load();
}

function changeSort(next: typeof tablePagination.value) {
  const changed =
    next.sortBy !== tablePagination.value.sortBy ||
    next.descending !== tablePagination.value.descending;
  tablePagination.value = next;
  if (!changed) return;
  orderBy.value = next.sortBy
    ? [{ field: next.sortBy, direction: next.descending ? "desc" : "asc" }]
    : props.view.query.default_sort;
  applyQuery();
}

function changePage(next: number) {
  page.value = next;
  void load();
}

function changePageSize(next: number | null) {
  if (!next) return;
  pageSize.value = next;
  page.value = 1;
  void load();
}

watch(
  () => props.view,
  (view) => {
    page.value = 1;
    pageSize.value = view.query.default_page_size;
    search.value = "";
    filters.value = createTableFilters(
      view.columns.filter((column) =>
        view.query.filter_fields.includes(column.field),
      ),
    );
    filtersOpen.value = false;
    visibleColumnNames.value = view.columns.map((column) => column.field);
    orderBy.value = view.query.default_sort;
    const initialSort = view.query.default_sort[0];
    tablePagination.value = {
      sortBy: initialSort?.field ?? null,
      descending: initialSort?.direction === "desc",
      page: 1,
      rowsPerPage: 0,
    };
    selectedDisplayRows.value = [];
    void load();
  },
  { immediate: true },
);
watch(
  () => props.session,
  () => void load(),
  { deep: true },
);
onBeforeUnmount(() => {
  controller?.abort();
  actionController?.abort();
});
</script>

<template>
  <section class="table-view">
    <header class="table-view-heading">
      <div>
        <q-badge outline color="primary">{{ view.view_id }}</q-badge>
        <h2>{{ view.title || view.table }}</h2>
        <p>
          {{ view.columns.length }} 个可见字段 · 数据源 {{ view.data_action }}
        </p>
      </div>
      <div class="view-actions">
        <q-btn
          v-for="presentation in directToolbarActions"
          :key="presentation.operation_id"
          :disabled="presentation.availability?.state === 'disabled'"
          :title="presentation.availability?.reason"
          :outline="presentation !== toolbarActionGroups.primary"
          :color="actionColor(presentation)"
          :icon="presentation.appearance?.icon"
          :label="actionLabel(presentation)"
          @click="openAction(presentation)"
        />
        <q-btn
          v-if="toolbarActionGroups.overflow.length"
          flat
          round
          color="primary"
          icon="more_horiz"
          aria-label="更多工具操作"
        >
          <q-menu auto-close>
            <q-list class="action-menu-list">
              <q-item
                v-for="presentation in toolbarActionGroups.overflow"
                :key="presentation.operation_id"
                clickable
                :disable="presentation.availability?.state === 'disabled'"
                @click="openAction(presentation)"
              >
                <q-item-section avatar>
                  <q-icon
                    :name="presentation.appearance?.icon || 'arrow_forward'"
                    :color="actionColor(presentation)"
                  />
                </q-item-section>
                <q-item-section>{{ actionLabel(presentation) }}</q-item-section>
              </q-item>
            </q-list>
          </q-menu>
        </q-btn>
        <q-btn
          flat
          round
          color="primary"
          icon="refresh"
          aria-label="刷新数据"
          :loading="loading"
          @click="load"
        />
      </div>
    </header>

    <q-card flat bordered class="query-card">
      <q-card-section class="table-query-section">
        <div class="table-query-bar">
          <q-input
            v-if="view.query.search_fields.length"
            v-model="search"
            dense
            outlined
            clearable
            class="table-search-input"
            :placeholder="`搜索 ${view.query.search_fields.join('、')}`"
            @keyup.enter="applyQuery"
          >
            <template #prepend><q-icon name="search" /></template>
          </q-input>
          <q-btn
            v-if="filterColumns.length"
            outline
            color="primary"
            icon="tune"
            :label="activeFilterCount ? `筛选 ${activeFilterCount}` : '筛选'"
            :aria-expanded="filtersOpen"
            @click="filtersOpen = !filtersOpen"
          />
          <q-btn unelevated color="primary" label="查询" @click="applyQuery" />
          <q-space />
          <q-btn
            flat
            round
            color="grey-7"
            icon="view_column"
            aria-label="列设置"
          >
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
                        setColumnVisible(column.field, Boolean($event))
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
                    <q-toggle v-model="denseTable" aria-label="紧凑行高" />
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
              v-for="column in filterColumns"
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
                  @update:model-value="setFilterOperator(column.field, $event)"
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
                    @keyup.enter="applyQuery"
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
                    @keyup.enter="applyQuery"
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
                  :placeholder="
                    column.filter?.placeholder || '输入后按回车添加'
                  "
                  :aria-label="`${column.title || column.field}筛选值`"
                  @update:model-value="setFilterValue(column.field, $event)"
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
                  @update:model-value="setFilterValue(column.field, $event)"
                />
                <q-input
                  v-else
                  :model-value="filters[column.field]?.value"
                  :type="filterInputType(column)"
                  dense
                  outlined
                  clearable
                  :placeholder="column.filter?.placeholder || '输入筛选值'"
                  :aria-label="`${column.title || column.field}筛选值`"
                  @update:model-value="setFilterValue(column.field, $event)"
                  @keyup.enter="applyQuery"
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
            @remove="search = ''"
          >
            关键词：{{ search.trim() }}
          </q-chip>
          <q-chip
            v-for="column in activeFilterColumns"
            :key="column.field"
            removable
            color="blue-grey-1"
            text-color="blue-grey-9"
            @remove="clearFilter(column.field)"
          >
            {{ filterSummary(column) }}
          </q-chip>
          <q-btn
            flat
            dense
            color="primary"
            label="清除全部"
            @click="clearAllQuery"
          />
        </div>
      </q-card-section>
    </q-card>

    <q-banner v-if="error" rounded class="table-error bg-red-1 text-negative">
      <template #avatar><q-icon name="error" /></template>
      {{ error }}
    </q-banner>
    <q-banner
      v-else-if="treeResult.warning"
      rounded
      class="table-error bg-orange-1 text-warning"
    >
      <template #avatar><q-icon name="warning" /></template>
      {{ treeResult.warning }}
    </q-banner>

    <div v-if="bulkActions.length" class="bulk-actions">
      <span>已选 {{ selectedRows.length }} 项</span>
      <q-btn
        v-for="presentation in bulkActions"
        :key="presentation.operation_id"
        dense
        outline
        color="primary"
        :disabled="
          selectedRows.length === 0 ||
          presentation.availability?.state === 'disabled'
        "
        :title="presentation.availability?.reason"
        :label="presentation.title || presentation.operation_id"
        @click="openAction(presentation)"
      />
    </div>

    <q-table
      v-model:selected="selectedDisplayRows"
      :rows="qTableRows"
      :columns="tableColumns"
      :visible-columns="visibleColumns"
      :loading="loading"
      :pagination="tablePagination"
      :rows-per-page-options="[0]"
      row-key="key"
      :selection="bulkActions.length ? 'multiple' : 'none'"
      flat
      bordered
      :dense="denseTable"
      hide-pagination
      binary-state-sort
      class="business-table"
      @update:pagination="changeSort"
    >
      <template #body-cell="slotProps">
        <q-td :props="slotProps">
          <span
            :style="
              slotProps.col.name === firstColumnName
                ? { paddingLeft: `${slotProps.row.depth * 20}px` }
                : undefined
            "
          >
            {{ slotProps.value }}
          </span>
        </q-td>
      </template>
      <template #body-cell-__actions="slotProps">
        <q-td :props="slotProps" class="table-row-actions">
          <q-btn
            v-for="presentation in directRowActions"
            :key="presentation.operation_id"
            flat
            dense
            :color="actionColor(presentation)"
            :icon="presentation.appearance?.icon"
            :disabled="presentation.availability?.state === 'disabled'"
            :title="presentation.availability?.reason"
            :label="actionLabel(presentation)"
            @click="openAction(presentation, slotProps.row.data)"
          />
          <q-btn
            v-if="rowActionGroups.overflow.length"
            flat
            round
            dense
            color="grey-7"
            icon="more_horiz"
            aria-label="更多操作"
          >
            <q-menu auto-close anchor="bottom right" self="top right">
              <q-list class="action-menu-list">
                <q-item
                  v-for="presentation in rowActionGroups.overflow"
                  :key="presentation.operation_id"
                  clickable
                  :disable="presentation.availability?.state === 'disabled'"
                  @click="openAction(presentation, slotProps.row.data)"
                >
                  <q-item-section avatar>
                    <q-icon
                      :name="presentation.appearance?.icon || 'arrow_forward'"
                      :color="actionColor(presentation)"
                    />
                  </q-item-section>
                  <q-item-section>{{
                    actionLabel(presentation)
                  }}</q-item-section>
                </q-item>
              </q-list>
            </q-menu>
          </q-btn>
        </q-td>
      </template>
    </q-table>

    <div v-if="!view.tree || hasActiveQuery" class="table-pagination">
      <span>共 {{ total }} 项</span>
      <q-select
        :model-value="pageSize"
        :options="pageSizeOptions"
        dense
        outlined
        emit-value
        map-options
        class="page-size-select"
        aria-label="每页数量"
        @update:model-value="changePageSize"
      />
      <q-pagination
        :model-value="page"
        :max="pageCount"
        boundary-numbers
        direction-links
        @update:model-value="changePage"
      />
    </div>

    <q-dialog v-model="actionDialog">
      <q-card class="action-dialog-card">
        <q-card-section class="row items-center">
          <div class="text-h6">
            {{ activePresentation?.title || activeAction?.title }}
          </div>
          <q-space />
          <q-btn
            v-close-popup
            flat
            round
            dense
            icon="close"
            aria-label="关闭"
          />
        </q-card-section>
        <q-separator />
        <q-card-section class="scroll action-dialog-content">
          <JsonSchemaForm
            v-if="activeAction"
            v-model="actionValues"
            :schema="activeAction.input_schema"
            :params="activeAction.params"
            :business-fields="view.form.fields"
            :actions="actions"
            :session="session"
            :multipart="activeAction.multipart"
          />
        </q-card-section>
        <q-separator />
        <q-card-actions align="right">
          <q-btn v-close-popup flat label="取消" />
          <q-btn
            color="primary"
            label="提交"
            :loading="actionLoading"
            @click="submitAction"
          />
        </q-card-actions>
      </q-card>
    </q-dialog>
  </section>
</template>

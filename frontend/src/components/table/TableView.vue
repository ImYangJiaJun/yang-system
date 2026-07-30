<script setup lang="ts">
import { computed } from "vue";
import { type QTableColumn } from "quasar";
import type { SessionContext } from "src/api/client";
import { buildTreeRows } from "src/contracts/table-data";
import type {
  ActionDemoSchema,
  ActionPresentationSchema,
  TableColumnSchema,
  TableViewSchema,
} from "src/contracts/ui-catalog";
import TableActionDialog from "./TableActionDialog.vue";
import TableDataGrid from "./TableDataGrid.vue";
import TableQueryPanel from "./TableQueryPanel.vue";
import { flattenDisplayRows, type DisplayRow } from "./table-view-model";
import { useColumnPreferences } from "./composables/useColumnPreferences";
import { useRelationOptions } from "./composables/useRelationOptions";
import { useTableActions } from "./composables/useTableActions";
import { useTableQuery } from "./composables/useTableQuery";
import { useTableSelection } from "./composables/useTableSelection";

const props = defineProps<{
  view: TableViewSchema;
  actions: ActionDemoSchema[];
  session: SessionContext;
  developer?: boolean;
}>();
const emit = defineEmits<{
  customAction: [
    presentation: ActionPresentationSchema,
    row?: Record<string, unknown>,
  ];
}>();

const actionById = computed(
  () => new Map(props.actions.map((action) => [action.operation_id, action])),
);
const dataAction = computed(() => actionById.value.get(props.view.data_action));
const relationState = useRelationOptions({
  view: () => props.view,
  actions: () => props.actions,
  session: () => props.session,
});
const { relationOptions, relationErrors } = relationState;
const queryState = useTableQuery({
  view: () => props.view,
  dataAction,
  session: () => props.session,
  onRowsLoaded: relationState.load,
  onLoadError: relationState.clear,
});
const {
  rows,
  total,
  page,
  pageSize,
  search,
  filters,
  filtersOpen,
  loading,
  error,
  tablePagination,
  activeFilterColumns,
  activeFilterCount,
  hasActiveQuery,
  pageCount,
  pageSizeOptions,
  load,
  applyQuery,
  changeSort,
  changePage,
  changePageSize,
  setFilterOperator,
  setFilterValue,
  clearFilter,
  clearAllQuery,
} = queryState;
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
const qTableRows = computed(() => flattenDisplayRows(displayRows.value));
const selectionState = useTableSelection(qTableRows);
const { selectedDisplayRows, selectedRows } = selectionState;
const tableActions = useTableActions({
  view: () => props.view,
  actions: () => props.actions,
  session: () => props.session,
  selectedRows,
  reload: load,
  emitCustom: (presentation, row) => {
    emit("customAction", presentation, row);
  },
});
const {
  actionDialog,
  actionLoading,
  activePresentation,
  activeAction,
  actionValues,
  rowActions,
  bulkActions,
  toolbarActionGroups,
  directToolbarActions,
  rowActionGroups,
  directRowActions,
  openAction,
  submitAction,
} = tableActions;
const columnPreferences = useColumnPreferences(
  () => props.view,
  () => rowActions.value.length > 0,
);
const { visibleColumnNames, visibleColumns, denseTable, setColumnVisible } =
  columnPreferences;
const tableColumns = computed<QTableColumn<DisplayRow>[]>(() => {
  const columns: QTableColumn<DisplayRow>[] = props.view.columns.map(
    (column) => ({
      name: column.field,
      label: column.title || column.field,
      field: (row) => row.data[column.field],
      align: column.display?.align ?? "left",
      sortable: column.sortable,
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
const columnByField = computed(
  () => new Map(props.view.columns.map((column) => [column.field, column])),
);
const firstColumnName = computed(() => props.view.columns[0]?.field);

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

function relationLabel(column: TableColumnSchema, value: unknown) {
  if (!column.relation) return undefined;
  return relationState.labelFor(column.relation.operation_id, value);
}
</script>

<template>
  <section class="table-view">
    <header class="table-view-heading">
      <div>
        <q-badge v-if="developer" outline color="primary">
          {{ view.view_id }}
        </q-badge>
        <h2>{{ view.title || view.table }}</h2>
        <p v-if="developer">
          {{ view.columns.length }} 个可见字段 · 数据源 {{ view.data_action }}
        </p>
        <p v-else>
          共 {{ total }} 项 · 支持搜索、筛选、排序和批量处理
          <q-icon
            v-if="relationErrors.length"
            name="warning_amber"
            color="warning"
            size="16px"
          >
            <q-tooltip>{{ relationErrors.join("；") }}</q-tooltip>
          </q-icon>
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

    <TableQueryPanel
      :view="view"
      :search="search"
      :filters="filters"
      :filters-open="filtersOpen"
      :active-filter-count="activeFilterCount"
      :active-filter-columns="activeFilterColumns"
      :has-active-query="hasActiveQuery"
      :relation-options="relationOptions"
      :visible-column-names="visibleColumnNames"
      :dense-table="denseTable"
      @update:search="search = $event"
      @update:filters-open="filtersOpen = $event"
      @update:dense-table="denseTable = $event"
      @apply="applyQuery"
      @set-filter-operator="setFilterOperator"
      @set-filter-value="setFilterValue"
      @clear-filter="clearFilter"
      @clear-all="clearAllQuery"
      @set-column-visible="setColumnVisible"
    />

    <TableDataGrid
      v-model:selected="selectedDisplayRows"
      :view="view"
      :error="error"
      :tree-warning="treeResult.warning"
      :bulk-actions="bulkActions"
      :selected-rows="selectedRows"
      :rows="qTableRows"
      :columns="tableColumns"
      :visible-columns="visibleColumns"
      :loading="loading"
      :pagination="tablePagination"
      :dense="denseTable"
      :direct-row-actions="directRowActions"
      :row-action-groups="rowActionGroups"
      :column-by-field="columnByField"
      :first-column-name="firstColumnName"
      :total="total"
      :page-size="pageSize"
      :page-size-options="pageSizeOptions"
      :page="page"
      :page-count="pageCount"
      :has-active-query="hasActiveQuery"
      :relation-label="relationLabel"
      @open-action="openAction"
      @update-pagination="changeSort"
      @change-page-size="changePageSize"
      @change-page="changePage"
    />

    <TableActionDialog
      v-model="actionDialog"
      v-model:values="actionValues"
      :active-presentation="activePresentation"
      :active-action="activeAction"
      :view="view"
      :actions="actions"
      :session="session"
      :loading="actionLoading"
      @submit="submitAction"
    />
  </section>
</template>

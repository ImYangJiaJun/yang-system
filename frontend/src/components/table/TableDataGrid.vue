<script setup lang="ts">
import type { QTableColumn } from "quasar";
import type {
  ActionPresentationSchema,
  TableColumnSchema,
  TableViewSchema,
} from "src/contracts/ui-catalog";
import BusinessTableCell from "./BusinessTableCell.vue";
import type { DisplayRow } from "./table-view-model";
import type { TablePaginationState } from "./composables/useTableQuery";

const props = defineProps<{
  view: TableViewSchema;
  error: string;
  treeWarning: string;
  bulkActions: ActionPresentationSchema[];
  selectedRows: Array<Record<string, unknown>>;
  rows: DisplayRow[];
  columns: QTableColumn<DisplayRow>[];
  visibleColumns: string[];
  loading: boolean;
  pagination: TablePaginationState;
  dense: boolean;
  directRowActions: ActionPresentationSchema[];
  rowActionGroups: {
    primary?: ActionPresentationSchema;
    secondary: ActionPresentationSchema[];
    overflow: ActionPresentationSchema[];
  };
  columnByField: Map<string, TableColumnSchema>;
  firstColumnName?: string;
  total: number;
  pageSize: number;
  pageSizeOptions: Array<{ label: string; value: number }>;
  page: number;
  pageCount: number;
  hasActiveQuery: boolean;
  relationLabel: (
    column: TableColumnSchema,
    value: unknown,
  ) => string | undefined;
}>();

const selected = defineModel<DisplayRow[]>("selected", { required: true });
const emit = defineEmits<{
  openAction: [
    presentation: ActionPresentationSchema,
    row?: Record<string, unknown>,
  ];
  updatePagination: [pagination: TablePaginationState];
  changePageSize: [pageSize: number | null];
  changePage: [page: number];
}>();

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
</script>

<template>
  <q-banner v-if="error" rounded class="table-error bg-red-1 text-negative">
    <template #avatar><q-icon name="error" /></template>
    {{ error }}
  </q-banner>
  <q-banner
    v-else-if="treeWarning"
    rounded
    class="table-error bg-orange-1 text-warning"
  >
    <template #avatar><q-icon name="warning" /></template>
    {{ treeWarning }}
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
      @click="emit('openAction', presentation)"
    />
  </div>

  <q-table
    v-model:selected="selected"
    :rows="rows"
    :columns="columns"
    :visible-columns="visibleColumns"
    :loading="loading"
    :pagination="pagination"
    :rows-per-page-options="[0]"
    row-key="key"
    :selection="bulkActions.length ? 'multiple' : 'none'"
    flat
    bordered
    :dense="dense"
    hide-pagination
    binary-state-sort
    class="business-table"
    @update:pagination="emit('updatePagination', $event)"
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
          <BusinessTableCell
            v-if="columnByField.get(slotProps.col.name)"
            :column="columnByField.get(slotProps.col.name)!"
            :value="slotProps.row.data[slotProps.col.name]"
            :relation-label="
              props.relationLabel(
                columnByField.get(slotProps.col.name)!,
                slotProps.row.data[slotProps.col.name],
              )
            "
          />
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
          @click="emit('openAction', presentation, slotProps.row.data)"
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
                @click="emit('openAction', presentation, slotProps.row.data)"
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
      @update:model-value="emit('changePageSize', $event)"
    />
    <q-pagination
      :model-value="page"
      :max="pageCount"
      boundary-numbers
      direction-links
      @update:model-value="emit('changePage', $event)"
    />
  </div>
</template>

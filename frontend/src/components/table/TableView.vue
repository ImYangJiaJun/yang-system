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
  TableViewSchema,
} from "src/contracts/ui-catalog";
import JsonSchemaForm from "components/form/JsonSchemaForm.vue";
import {
  buildActionInitialValues,
  buildWhereClause,
  flattenDisplayRows,
  formatCell,
  pageSizeOptions as buildPageSizeOptions,
  type DisplayRow,
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
const filters = ref<Record<string, string>>({});
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
const filterColumns = computed(() =>
  props.view.columns.filter((column) =>
    props.view.query.filter_fields.includes(column.field),
  ),
);
const hasActiveQuery = computed(
  () =>
    Boolean(search.value.trim()) ||
    Object.values(filters.value).some((value) => Boolean(value.trim())),
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
      align: "left",
      sortable: column.sortable,
      format: (value) => formatCell(value),
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
    filters.value = {};
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
          v-for="presentation in toolbarActions"
          :key="presentation.operation_id"
          :disabled="presentation.availability?.state === 'disabled'"
          :title="presentation.availability?.reason"
          color="primary"
          :label="presentation.title || presentation.operation_id"
          @click="openAction(presentation)"
        />
        <q-btn
          outline
          color="primary"
          label="刷新"
          :loading="loading"
          @click="load"
        />
      </div>
    </header>

    <q-card flat bordered class="query-card">
      <q-card-section>
        <div class="query-grid">
          <q-input
            v-if="view.query.search_fields.length"
            v-model="search"
            dense
            outlined
            clearable
            :placeholder="`搜索 ${view.query.search_fields.join('、')}`"
            @keyup.enter="applyQuery"
          />
          <q-input
            v-for="column in filterColumns"
            :key="column.field"
            v-model="filters[column.field]"
            dense
            outlined
            clearable
            :placeholder="`${column.title || column.field} 精确筛选`"
            @keyup.enter="applyQuery"
          />
          <q-btn color="primary" label="查询" @click="applyQuery" />
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
      :loading="loading"
      :pagination="tablePagination"
      :rows-per-page-options="[0]"
      row-key="key"
      :selection="bulkActions.length ? 'multiple' : 'none'"
      flat
      bordered
      hide-pagination
      binary-state-sort
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
            v-for="presentation in rowActions"
            :key="presentation.operation_id"
            flat
            dense
            color="primary"
            :disabled="presentation.availability?.state === 'disabled'"
            :title="presentation.availability?.reason"
            :label="presentation.title || presentation.operation_id"
            @click="openAction(presentation, slotProps.row.data)"
          />
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

<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import {
  invokeAction,
  type InvocationResult,
  type SessionContext,
} from "@/api/client";
import { initialObject } from "@/contracts/json-schema";
import { buildTreeRows, parseTableData } from "@/contracts/table-data";
import type {
  ActionDemoSchema,
  ActionPresentationSchema,
  TableViewSchema,
} from "@/contracts/ui-catalog";
import JsonSchemaForm from "./JsonSchemaForm.vue";

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
const selectedRows = ref<Array<Record<string, unknown>>>([]);
const loading = ref(false);
const error = ref("");
const actionDialog = ref(false);
const actionLoading = ref(false);
const activePresentation = ref<ActionPresentationSchema>();
const activeAction = ref<ActionDemoSchema>();
const actionValues = ref<Record<string, unknown>>({});
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

function parseFilterValue(value: string): unknown {
  const trimmed = value.trim();
  if (!trimmed) return undefined;
  try {
    return JSON.parse(trimmed);
  } catch {
    return trimmed;
  }
}

function whereClause(): unknown {
  const conditions = Object.entries(filters.value)
    .map(([field, value]) => ({ field, value: parseFilterValue(value) }))
    .filter((item) => item.value !== undefined)
    .map((item) => ({ type: "eq", field: item.field, value: item.value }));
  if (conditions.length === 0) return undefined;
  return conditions.length === 1 ? conditions[0] : { type: "and", conditions };
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
        where: whereClause(),
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

function actionInitialValues(
  action: ActionDemoSchema,
  row?: Record<string, unknown>,
): Record<string, unknown> {
  const initial = initialObject(action.input_schema);
  if (!row) return initial;
  const readableRow = Object.fromEntries(
    Object.entries(row).filter(
      ([name]) =>
        !props.view.form.fields.find((field) => field.field === name)
          ?.write_only,
    ),
  );
  for (const name of Object.keys(initial)) {
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
    ElMessage.error(`目录缺少 Action：${presentation.operation_id}`);
    return;
  }
  activePresentation.value = presentation;
  activeAction.value = action;
  actionValues.value = actionInitialValues(action, row);
  if (presentation.placement === "bulk") {
    actionValues.value.selected = selectedRows.value;
  }
  if (presentation.interaction === "form") {
    actionDialog.value = true;
    return;
  }
  await submitAction();
}

async function confirmAction(presentation: ActionPresentationSchema) {
  if (!presentation.confirmation) return;
  await ElMessageBox.confirm(
    presentation.confirmation.message,
    presentation.confirmation.title,
    { type: "warning", confirmButtonText: "确认", cancelButtonText: "取消" },
  );
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
    await confirmAction(presentation);
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
    ElMessage.success(result.message || "操作成功");
    actionDialog.value = false;
    await load();
  } catch (cause) {
    if (cause === "cancel" || cause === "close") return;
    if (cause instanceof Error && cause.name === "AbortError") return;
    ElMessage.error(cause instanceof Error ? cause.message : String(cause));
  } finally {
    actionLoading.value = false;
  }
}

function applyQuery() {
  page.value = 1;
  void load();
}

function changeSort({
  prop,
  order,
}: {
  prop?: string | null;
  order?: string | null;
}) {
  orderBy.value =
    prop && order
      ? [{ field: prop, direction: order === "descending" ? "desc" : "asc" }]
      : props.view.query.default_sort;
  applyQuery();
}

function formatCell(value: unknown): string {
  if (value === null || value === undefined || value === "") return "—";
  if (typeof value === "boolean") return value ? "是" : "否";
  return typeof value === "object" ? JSON.stringify(value) : String(value);
}

watch(
  () => props.view,
  (view) => {
    page.value = 1;
    pageSize.value = view.query.default_page_size;
    search.value = "";
    filters.value = {};
    orderBy.value = view.query.default_sort;
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
        <el-tag size="small" effect="plain">{{ view.view_id }}</el-tag>
        <h2>{{ view.title || view.table }}</h2>
        <p>
          {{ view.columns.length }} 个可见字段 · 数据源 {{ view.data_action }}
        </p>
      </div>
      <div class="view-actions">
        <el-button
          v-for="presentation in toolbarActions"
          :key="presentation.operation_id"
          :disabled="presentation.availability?.state === 'disabled'"
          :title="presentation.availability?.reason"
          type="primary"
          @click="openAction(presentation)"
        >
          {{ presentation.title || presentation.operation_id }}
        </el-button>
        <el-button :loading="loading" @click="load">刷新</el-button>
      </div>
    </header>

    <el-card shadow="never" class="query-card">
      <div class="query-grid">
        <el-input
          v-if="view.query.search_fields.length"
          v-model="search"
          clearable
          :placeholder="`搜索 ${view.query.search_fields.join('、')}`"
          @keyup.enter="applyQuery"
        />
        <el-input
          v-for="column in filterColumns"
          :key="column.field"
          v-model="filters[column.field]"
          clearable
          :placeholder="`${column.title || column.field} 精确筛选`"
          @keyup.enter="applyQuery"
        />
        <el-button type="primary" @click="applyQuery">查询</el-button>
      </div>
    </el-card>

    <el-alert
      v-if="error"
      type="error"
      :title="error"
      :closable="false"
      show-icon
      class="table-error"
    />
    <el-alert
      v-else-if="treeResult.warning"
      type="warning"
      :title="treeResult.warning"
      :closable="false"
      show-icon
      class="table-error"
    />

    <div v-if="bulkActions.length" class="bulk-actions">
      <span>已选 {{ selectedRows.length }} 项</span>
      <el-button
        v-for="presentation in bulkActions"
        :key="presentation.operation_id"
        size="small"
        :disabled="
          selectedRows.length === 0 ||
          presentation.availability?.state === 'disabled'
        "
        :title="presentation.availability?.reason"
        @click="openAction(presentation)"
      >
        {{ presentation.title || presentation.operation_id }}
      </el-button>
    </div>

    <el-table
      v-loading="loading"
      :data="displayRows"
      :row-key="
        (row: Record<string, unknown>) =>
          String(row[view.tree?.id_field || view.columns[0]?.field || 'id'])
      "
      border
      default-expand-all
      @selection-change="selectedRows = $event"
      @sort-change="changeSort"
    >
      <el-table-column v-if="bulkActions.length" type="selection" width="48" />
      <el-table-column
        v-for="column in view.columns"
        :key="column.field"
        :prop="column.field"
        :label="column.title || column.field"
        :sortable="column.sortable ? 'custom' : false"
        min-width="140"
        show-overflow-tooltip
      >
        <template #default="scope">
          {{ formatCell(scope.row[column.field]) }}
        </template>
      </el-table-column>
      <el-table-column
        v-if="rowActions.length"
        label="操作"
        fixed="right"
        min-width="160"
      >
        <template #default="scope">
          <el-button
            v-for="presentation in rowActions"
            :key="presentation.operation_id"
            link
            type="primary"
            :disabled="presentation.availability?.state === 'disabled'"
            :title="presentation.availability?.reason"
            @click="openAction(presentation, scope.row)"
          >
            {{ presentation.title || presentation.operation_id }}
          </el-button>
        </template>
      </el-table-column>
    </el-table>

    <el-pagination
      v-if="!view.tree || hasActiveQuery"
      v-model:current-page="page"
      v-model:page-size="pageSize"
      :total="total"
      :page-sizes="[10, 20, 50, view.query.max_page_size]"
      layout="total, sizes, prev, pager, next"
      class="table-pagination"
      @change="load"
    />

    <el-dialog
      v-model="actionDialog"
      :title="activePresentation?.title || activeAction?.title"
      width="min(720px, 86vw)"
      destroy-on-close
    >
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
      <template #footer>
        <el-button @click="actionDialog = false">取消</el-button>
        <el-button
          type="primary"
          :loading="actionLoading"
          @click="submitAction"
        >
          提交
        </el-button>
      </template>
    </el-dialog>
  </section>
</template>

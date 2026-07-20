<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { storeToRefs } from "pinia";
import { useRoute } from "vue-router";
import { type QTableColumn } from "quasar";
import ActionDemo from "components/action/ActionDemo.vue";
import TableView from "components/table/TableView.vue";
import { invokeAction } from "src/api/client";
import { asJsonSchema, initialObject } from "src/contracts/json-schema";
import type { ActionDemoSchema } from "src/contracts/ui-catalog";
import { buildAccountModulePages } from "src/module-pages";
import { useCatalogStore } from "stores/catalog";

const route = useRoute();
const store = useCatalogStore();
const {
  catalog,
  loading: catalogLoading,
  selectedOrganization,
  session,
} = storeToRefs(store);
const activeAction = ref<ActionDemoSchema>();
const activeInitialValues = ref<Record<string, unknown>>({});
const actionDialogOpen = ref(false);
const data = ref<unknown>();
const dataLoading = ref(false);
const dataError = ref("");
const search = ref("");
const page = ref(1);
const pageSize = 20;
let controller: AbortController | undefined;

const moduleId = computed(() => String(route.params.moduleId ?? ""));
const modulePage = computed(() =>
  buildAccountModulePages(catalog.value).find(
    (candidate) => candidate.id === moduleId.value,
  ),
);
const primaryAction = computed(() => {
  if (modulePage.value?.views.length) return undefined;
  return (
    modulePage.value?.actions.find((action) =>
      action.operation_id.endsWith(".list"),
    ) ??
    modulePage.value?.actions.find((action) =>
      action.operation_id.endsWith(".me"),
    )
  );
});
const secondaryActions = computed(() =>
  (modulePage.value?.actions ?? []).filter(
    (action) =>
      action !== primaryAction.value &&
      !action.operation_id.endsWith(".select") &&
      !action.operation_id.endsWith(".login") &&
      !action.operation_id.endsWith(".refresh"),
  ),
);

function inputFields(action: ActionDemoSchema): string[] {
  return Object.keys(asJsonSchema(action.input_schema).properties ?? {});
}

const rowActions = computed(() =>
  secondaryActions.value.filter((action) => inputFields(action).includes("id")),
);
const toolbarActions = computed(() =>
  secondaryActions.value.filter(
    (action) => !inputFields(action).includes("id"),
  ),
);
const resultRecord = computed<Record<string, unknown> | undefined>(() =>
  isRecord(data.value) ? data.value : undefined,
);
const rows = computed<Record<string, unknown>[]>(() => {
  const items = resultRecord.value?.items;
  return Array.isArray(items) ? items.filter(isRecord) : [];
});
const detail = computed<Record<string, unknown> | undefined>(() =>
  resultRecord.value && !Array.isArray(resultRecord.value.items)
    ? resultRecord.value
    : undefined,
);
const total = computed(() => numericValue(resultRecord.value?.total));
const totalPages = computed(() =>
  Math.max(1, Math.ceil(total.value / pageSize)),
);
const supportsSearch = computed(() =>
  primaryAction.value
    ? inputFields(primaryAction.value).includes("search")
    : false,
);
const columns = computed<QTableColumn[]>(() => {
  const keys = Array.from(
    new Set(rows.value.flatMap((row) => Object.keys(row))),
  );
  const values: QTableColumn[] = keys.map((key) => ({
    name: key,
    label: fieldLabel(key),
    field: key,
    align: "left",
    sortable: true,
    format: (value) => formatValue(key, value),
  }));
  if (rowActions.value.length || modulePage.value?.id === "org.tenant") {
    values.push({
      name: "actions",
      label: "操作",
      field: () => undefined,
      align: "right",
    });
  }
  return values;
});

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function numericValue(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

function fieldLabel(field: string): string {
  return (
    {
      id: "ID",
      user_user: "用户 ID",
      username: "用户名",
      name: "名称",
      code: "企业编号",
      position: "职务",
      status: "状态",
      admin: "管理员",
      created_at: "创建时间",
      updated_at: "更新时间",
    }[field] ?? field
  );
}

function formatValue(field: string, value: unknown): string {
  if (typeof value === "boolean") return value ? "是" : "否";
  if (field === "status" && value === "active") return "启用";
  if (field === "status" && value === "disabled") return "停用";
  if (
    (field === "created_at" || field === "updated_at") &&
    typeof value === "number"
  ) {
    const milliseconds = value < 1_000_000_000_000 ? value * 1000 : value;
    return new Date(milliseconds).toLocaleString();
  }
  if (value === null || value === undefined || value === "") return "—";
  return typeof value === "object" ? JSON.stringify(value) : String(value);
}

function actionValues(): Record<string, unknown> {
  const action = primaryAction.value;
  if (!action) return {};
  const fields = new Set(inputFields(action));
  return {
    ...initialObject(action.input_schema),
    ...(fields.has("page") ? { page: page.value } : {}),
    ...(fields.has("limit") ? { limit: pageSize } : {}),
    ...(fields.has("search") && search.value.trim()
      ? { search: search.value.trim() }
      : {}),
  };
}

async function loadPrimary() {
  const action = primaryAction.value;
  if (!action) {
    data.value = undefined;
    dataError.value = "";
    return;
  }
  controller?.abort();
  controller = new AbortController();
  dataLoading.value = true;
  dataError.value = "";
  try {
    const result = await invokeAction(
      action,
      actionValues(),
      session.value,
      controller.signal,
    );
    if (result.kind !== "json")
      throw new Error("模块主数据 Action 必须返回 JSON");
    data.value = result.data;
  } catch (cause) {
    if (cause instanceof Error && cause.name === "AbortError") return;
    data.value = undefined;
    dataError.value = cause instanceof Error ? cause.message : String(cause);
  } finally {
    dataLoading.value = false;
  }
}

function openAction(action: ActionDemoSchema, row?: Record<string, unknown>) {
  activeAction.value = action;
  activeInitialValues.value =
    row && inputFields(action).includes("id") ? { id: row.id } : {};
  actionDialogOpen.value = true;
}

function selectOrganizationRow(row: Record<string, unknown>) {
  if (
    typeof row.id !== "number" ||
    typeof row.name !== "string" ||
    typeof row.code !== "string"
  ) {
    dataError.value = "企业列表缺少 id、name 或 code";
    return;
  }
  store.selectOrganization({ id: row.id, name: row.name, code: row.code });
}

function refreshFromFirstPage() {
  page.value = 1;
  void loadPrimary();
}

function handleCompleted() {
  void loadPrimary();
  if (modulePage.value?.id === "org.tenant") {
    void store.loadOrganizations();
  }
}

watch(
  [modulePage, session],
  () => {
    page.value = 1;
    search.value = "";
    void loadPrimary();
  },
  { immediate: true, deep: true },
);
watch(page, () => void loadPrimary());
onBeforeUnmount(() => controller?.abort());
</script>

<template>
  <q-page padding class="module-page relative-position">
    <template v-if="modulePage">
      <header class="module-page-heading">
        <div class="row items-center no-wrap q-gutter-md">
          <q-avatar
            size="58px"
            color="primary"
            text-color="white"
            :icon="modulePage.icon"
          />
          <div>
            <div class="module-id">{{ modulePage.id }}</div>
            <h1>{{ modulePage.title }}</h1>
            <p>{{ modulePage.description }}</p>
          </div>
        </div>
        <div class="row q-gutter-sm">
          <q-btn
            v-for="action in toolbarActions"
            :key="action.operation_id"
            color="primary"
            unelevated
            :icon="action.method === 'POST' ? 'add' : 'tune'"
            :label="action.title"
            @click="openAction(action)"
          />
        </div>
      </header>

      <TableView
        v-if="modulePage.views[0] && catalog"
        :view="modulePage.views[0]"
        :actions="catalog.actions"
        :session="session"
      />

      <q-card v-else-if="primaryAction" flat bordered class="module-data-card">
        <q-card-section class="module-data-toolbar">
          <div>
            <div class="text-subtitle1 text-weight-medium">
              {{ primaryAction.title }}
            </div>
            <div class="text-caption text-grey-7">
              {{ primaryAction.description }}
            </div>
          </div>
          <q-space />
          <q-input
            v-if="supportsSearch"
            v-model="search"
            dense
            outlined
            clearable
            debounce="250"
            placeholder="搜索账号"
            @update:model-value="refreshFromFirstPage"
          >
            <template #prepend><q-icon name="search" /></template>
          </q-input>
          <q-btn
            flat
            round
            color="primary"
            icon="refresh"
            aria-label="刷新页面"
            :loading="dataLoading"
            @click="loadPrimary"
          />
        </q-card-section>
        <q-separator />

        <q-banner v-if="dataError" class="bg-red-1 text-negative">
          <template #avatar><q-icon name="error" /></template>
          {{ dataError }}
        </q-banner>

        <q-table
          v-else-if="rows.length"
          flat
          :rows="rows"
          :columns="columns"
          row-key="id"
          :loading="dataLoading"
          hide-pagination
        >
          <template #body-cell-actions="props">
            <q-td :props="props">
              <q-btn
                v-if="modulePage.id === 'org.tenant'"
                flat
                dense
                color="primary"
                :disable="selectedOrganization?.id === props.row.id"
                :label="
                  selectedOrganization?.id === props.row.id
                    ? '当前企业'
                    : '进入企业'
                "
                @click="selectOrganizationRow(props.row)"
              />
              <q-btn-dropdown
                v-if="rowActions.length"
                flat
                dense
                color="primary"
                label="管理"
              >
                <q-list>
                  <q-item
                    v-for="action in rowActions"
                    :key="action.operation_id"
                    v-close-popup
                    clickable
                    @click="openAction(action, props.row)"
                  >
                    <q-item-section>{{ action.title }}</q-item-section>
                  </q-item>
                </q-list>
              </q-btn-dropdown>
            </q-td>
          </template>
        </q-table>

        <q-list v-else-if="detail" separator class="module-detail-list">
          <q-item v-for="(value, field) in detail" :key="field">
            <q-item-section>
              <q-item-label caption>{{ fieldLabel(field) }}</q-item-label>
              <q-item-label>{{ formatValue(field, value) }}</q-item-label>
            </q-item-section>
          </q-item>
        </q-list>

        <div v-else-if="!dataLoading" class="module-data-empty">
          <q-icon name="inbox" size="42px" />
          <span>当前模块暂无数据</span>
        </div>

        <q-card-actions v-if="rows.length && totalPages > 1" align="right">
          <q-pagination
            v-model="page"
            :max="totalPages"
            :max-pages="7"
            boundary-numbers
            color="primary"
          />
        </q-card-actions>
      </q-card>

      <q-card
        v-else-if="secondaryActions.length"
        flat
        bordered
        class="module-data-card"
      >
        <q-list separator>
          <q-item
            v-for="action in secondaryActions"
            :key="action.operation_id"
            clickable
            @click="openAction(action)"
          >
            <q-item-section avatar>
              <q-icon color="primary" name="tune" />
            </q-item-section>
            <q-item-section>
              <q-item-label>{{ action.title }}</q-item-label>
              <q-item-label caption>{{ action.description }}</q-item-label>
            </q-item-section>
            <q-item-section side
              ><q-icon name="chevron_right"
            /></q-item-section>
          </q-item>
        </q-list>
      </q-card>
    </template>

    <div v-else-if="!catalogLoading" class="module-page-empty">
      <q-icon name="lock" size="52px" />
      <h2>当前身份无法访问该模块</h2>
      <p>页面只会为服务端已授权的 Module 生成。</p>
      <q-btn outline color="primary" label="返回应用中心" to="/" />
    </div>

    <q-dialog v-model="actionDialogOpen">
      <q-card class="account-action-dialog">
        <q-card-section class="row items-center q-pb-none">
          <div class="text-h6">{{ activeAction?.title }}</div>
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
        <q-card-section class="scroll account-action-dialog-body">
          <ActionDemo
            v-if="activeAction"
            :action="activeAction"
            :session="session"
            :initial-values="activeInitialValues"
            formal
            @completed="handleCompleted"
          />
        </q-card-section>
      </q-card>
    </q-dialog>

    <q-inner-loading :showing="catalogLoading || dataLoading">
      <q-spinner color="primary" size="48px" />
    </q-inner-loading>
  </q-page>
</template>

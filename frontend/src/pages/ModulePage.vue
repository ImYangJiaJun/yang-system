<script setup lang="ts">
import {
  computed,
  onBeforeUnmount,
  ref,
  shallowRef,
  watch,
  type Component,
} from "vue";
import { storeToRefs } from "pinia";
import { type QTableColumn, useQuasar } from "quasar";
import { useRoute, useRouter } from "vue-router";
import BusinessTableCell from "components/table/BusinessTableCell.vue";
import TableActionDialog from "components/table/TableActionDialog.vue";
import TableView from "components/table/TableView.vue";
import { invokeAction } from "src/api/client";
import { useApplicationSession } from "src/composables/useApplicationSession";
import { usePresentedActions } from "components/table/composables/useTableActions";
import {
  asJsonSchema,
  effectiveSchema,
  initialObject,
  type JsonSchemaNode,
} from "src/contracts/json-schema";
import type {
  ActionDemoSchema,
  ActionPresentationSchema,
  TableColumnSchema,
} from "src/contracts/ui-catalog";
import { resolveCustomView } from "src/custom/registry";
import { buildAccountModulePages, moduleView } from "src/module-pages";
import { formatCell } from "components/table/table-view-model";
import { useCatalogStore } from "stores/catalog";
import { useIdentityStore } from "stores/identity";

const route = useRoute();
const router = useRouter();
const $q = useQuasar();
const catalogStore = useCatalogStore();
const identityStore = useIdentityStore();
const { session } = useApplicationSession();
const { catalog, loading: catalogLoading } = storeToRefs(catalogStore);
const { accountIdentity } = storeToRefs(identityStore);
const activeViewId = ref("");
const selectedRows = ref<Record<string, unknown>[]>([]);
const data = ref<unknown>();
const dataLoading = ref(false);
const dataError = ref("");
const search = ref("");
const page = ref(1);
const pageSize = 20;
const customLoading = ref(false);
const customComponent = shallowRef<Component>();
const customPresentation = shallowRef<ActionPresentationSchema>();
let controller: AbortController | undefined;

const moduleId = computed(() => String(route.params.moduleId ?? ""));
const availableModule = computed(() =>
  buildAccountModulePages(catalog.value).find(
    (candidate) => candidate.id === moduleId.value,
  ),
);
const modulePage = computed(() =>
  availableModule.value?.identity === accountIdentity.value
    ? availableModule.value
    : undefined,
);
const effectiveView = computed(() => {
  const definition = modulePage.value;
  return definition ? moduleView(definition, activeViewId.value) : undefined;
});
const primaryAction = computed(() =>
  effectiveView.value ? undefined : modulePage.value?.primaryAction,
);

async function openCustomAction(presentation: ActionPresentationSchema) {
  const loader = resolveCustomView(presentation.view_id);
  if (!loader) {
    $q.notify({
      type: "warning",
      message: `自定义页面 ${presentation.view_id ?? "未声明"} 未注册，已保留通用模块页`,
    });
    return;
  }
  customLoading.value = true;
  try {
    customComponent.value = (await loader()).default;
    customPresentation.value = presentation;
  } catch (cause) {
    customComponent.value = undefined;
    customPresentation.value = undefined;
    $q.notify({
      type: "negative",
      message: `自定义页面加载失败，已回退通用模块页：${
        cause instanceof Error ? cause.message : String(cause)
      }`,
    });
  } finally {
    customLoading.value = false;
  }
}

async function reloadModule() {
  await loadPrimary();
}

const moduleActions = usePresentedActions({
  presentations: () => modulePage.value?.actionPresentations ?? [],
  businessFields: () => [],
  actions: () => catalog.value?.actions ?? [],
  session,
  selectedRows,
  reload: reloadModule,
  emitCustom: (presentation) => void openCustomAction(presentation),
});
const {
  actionDialog,
  actionLoading,
  activePresentation,
  activeAction,
  actionValues,
  bulkActions,
  toolbarActionGroups,
  directToolbarActions,
  rowActionGroups,
  directRowActions,
  openAction,
  submitAction,
} = moduleActions;

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
const rowSchemaProperties = computed(() =>
  outputProperties(primaryAction.value, true),
);
const detailSchemaProperties = computed(() =>
  outputProperties(primaryAction.value, false),
);
const primaryColumns = computed<QTableColumn[]>(() => {
  const keys = Array.from(
    new Set(rows.value.flatMap((row) => Object.keys(row))),
  );
  const values: QTableColumn[] = keys.map((key) => ({
    name: key,
    label: schemaColumn(key, rowSchemaProperties.value[key]).title,
    field: key,
    align: "left",
    sortable: true,
  }));
  if (directRowActions.value.length || rowActionGroups.value.overflow.length) {
    values.push({
      name: "__actions",
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

function inputFields(action: ActionDemoSchema): string[] {
  const root = asJsonSchema(action.input_schema);
  return Object.keys(effectiveSchema(root, root).properties ?? {});
}

function outputProperties(
  action: ActionDemoSchema | undefined,
  rowsOnly: boolean,
): Record<string, JsonSchemaNode> {
  if (!action) return {};
  const root = asJsonSchema(action.output_schema);
  const output = effectiveSchema(root, root);
  if (!rowsOnly) return output.properties ?? {};
  const items = output.properties?.items;
  if (!items) return {};
  const collection = effectiveSchema(root, items);
  const item = collection.items
    ? effectiveSchema(root, collection.items)
    : collection;
  return item.properties ?? {};
}

function schemaColumn(
  field: string,
  node: JsonSchemaNode | undefined,
): TableColumnSchema {
  const type = Array.isArray(node?.type)
    ? node.type.find((candidate) => candidate !== "null")
    : node?.type;
  const kind =
    node?.format === "date-time"
      ? "date_time"
      : node?.format === "date"
        ? "date"
        : type === "boolean"
          ? "boolean"
          : type === "number" || type === "integer"
            ? "number"
            : type === "object" || type === "array"
              ? "json"
              : "text";
  return {
    field,
    title: node?.title || field,
    description: node?.description || "",
    widget:
      type === "integer"
        ? "integer"
        : type === "number"
          ? "decimal"
          : type === "boolean"
            ? "switch"
            : kind === "date_time" || kind === "date"
              ? "date_time"
              : kind === "json"
                ? "json"
                : "text",
    required: false,
    searchable: false,
    filterable: false,
    sortable: true,
    display: { kind },
  };
}

function actionValuesForPrimary(): Record<string, unknown> {
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
  selectedRows.value = [];
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
      actionValuesForPrimary(),
      session.value,
      controller.signal,
    );
    if (result.kind !== "json") {
      throw new Error("模块主数据 Action 必须返回 JSON");
    }
    data.value = result.data;
  } catch (cause) {
    if (cause instanceof Error && cause.name === "AbortError") return;
    data.value = undefined;
    dataError.value = cause instanceof Error ? cause.message : String(cause);
  } finally {
    dataLoading.value = false;
  }
}

function closeCustomAction() {
  customComponent.value = undefined;
  customPresentation.value = undefined;
}

function refreshFromFirstPage() {
  page.value = 1;
  void loadPrimary();
}

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

watch(
  modulePage,
  (definition) => {
    if (
      !definition?.views.some(
        (candidate) => candidate.view_id === activeViewId.value,
      )
    ) {
      activeViewId.value = definition?.views[0]?.view_id ?? "";
    }
    page.value = 1;
    search.value = "";
    closeCustomAction();
    void loadPrimary();
  },
  { immediate: true },
);
watch(session, () => void loadPrimary(), { deep: true });
watch(activeViewId, () => closeCustomAction());
watch(
  [availableModule, catalog, catalogLoading],
  ([pageDefinition, currentCatalog, loading]) => {
    if (currentCatalog && !loading && !pageDefinition) {
      void router.replace("/roles");
    }
  },
  { immediate: true },
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
            <h1>{{ modulePage.title }}</h1>
            <p>{{ modulePage.description }}</p>
          </div>
        </div>
        <div v-if="!effectiveView" class="row q-gutter-sm">
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
            aria-label="更多模块操作"
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
                  <q-item-section>{{
                    actionLabel(presentation)
                  }}</q-item-section>
                </q-item>
              </q-list>
            </q-menu>
          </q-btn>
        </div>
      </header>

      <q-tabs
        v-if="modulePage.views.length > 1"
        v-model="activeViewId"
        dense
        align="left"
        active-color="primary"
        indicator-color="primary"
        class="module-view-tabs"
      >
        <q-tab
          v-for="view in modulePage.views"
          :key="view.view_id"
          :name="view.view_id"
          :label="view.title || view.table"
        />
      </q-tabs>

      <component
        :is="customComponent"
        v-if="customComponent && customPresentation && catalog"
        :presentation="customPresentation"
        :actions="catalog.actions"
        :session="session"
        @close="closeCustomAction"
      />

      <TableView
        v-else-if="effectiveView && catalog"
        :key="effectiveView.view_id"
        :view="effectiveView"
        :actions="catalog.actions"
        :session="session"
        presentation-submit-label
        @custom-action="openCustomAction"
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
            placeholder="搜索"
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
            :label="actionLabel(presentation)"
            @click="openAction(presentation)"
          />
        </div>

        <q-table
          v-if="rows.length"
          v-model:selected="selectedRows"
          flat
          :rows="rows"
          :columns="primaryColumns"
          row-key="id"
          :selection="bulkActions.length ? 'multiple' : 'none'"
          :loading="dataLoading"
          hide-pagination
        >
          <template #body-cell="props">
            <q-td :props="props">
              <BusinessTableCell
                v-if="props.col.name !== '__actions'"
                :column="
                  schemaColumn(
                    props.col.name,
                    rowSchemaProperties[props.col.name],
                  )
                "
                :value="props.row[props.col.name]"
              />
            </q-td>
          </template>
          <template #body-cell-__actions="props">
            <q-td :props="props">
              <q-btn
                v-for="presentation in directRowActions"
                :key="presentation.operation_id"
                flat
                dense
                :color="actionColor(presentation)"
                :disabled="presentation.availability?.state === 'disabled'"
                :title="presentation.availability?.reason"
                :label="actionLabel(presentation)"
                @click="openAction(presentation, props.row)"
              />
              <q-btn-dropdown
                v-if="rowActionGroups.overflow.length"
                flat
                dense
                color="primary"
                label="更多"
              >
                <q-list>
                  <q-item
                    v-for="presentation in rowActionGroups.overflow"
                    :key="presentation.operation_id"
                    v-close-popup
                    clickable
                    :disable="presentation.availability?.state === 'disabled'"
                    @click="openAction(presentation, props.row)"
                  >
                    <q-item-section>{{
                      actionLabel(presentation)
                    }}</q-item-section>
                  </q-item>
                </q-list>
              </q-btn-dropdown>
            </q-td>
          </template>
        </q-table>

        <q-list v-else-if="detail" separator class="module-detail-list">
          <q-item v-for="(value, field) in detail" :key="field">
            <q-item-section>
              <q-item-label caption>
                {{ schemaColumn(field, detailSchemaProperties[field]).title }}
              </q-item-label>
              <q-item-label>{{ formatCell(value) }}</q-item-label>
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
        v-else-if="modulePage.actionPresentations.length"
        flat
        bordered
        class="module-data-card module-data-empty"
      >
        <q-icon name="touch_app" size="42px" />
        <span>请从模块页头选择操作</span>
      </q-card>
    </template>

    <div v-else-if="!catalogLoading" class="module-page-empty">
      <q-icon name="lock" size="52px" />
      <h2>当前身份无法访问该模块</h2>
      <p>页面只会为服务端已授权的 Module 生成。</p>
      <q-btn outline color="primary" label="返回应用中心" to="/" />
    </div>

    <TableActionDialog
      v-model="actionDialog"
      v-model:values="actionValues"
      :active-presentation="activePresentation"
      :active-action="activeAction"
      :business-fields="[]"
      :actions="catalog?.actions ?? []"
      :session="session"
      :loading="actionLoading"
      :submit-label="activePresentation?.title"
      @submit="submitAction"
    />

    <q-inner-loading :showing="catalogLoading || dataLoading || customLoading">
      <q-spinner color="primary" size="48px" />
    </q-inner-loading>
  </q-page>
</template>

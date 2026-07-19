<script setup lang="ts">
import {
  computed,
  defineAsyncComponent,
  onBeforeUnmount,
  onMounted,
  ref,
  shallowRef,
  watch,
  type Component,
} from "vue";
import { ElMessage } from "element-plus";
import { fetchUiCatalog, type SessionContext } from "@/api/client";
import { CatalogCache } from "@/api/catalog-cache";
import {
  ContractError,
  type ActionDemoSchema,
  type UiCatalog,
} from "@/contracts/ui-catalog";
import type { ActionPresentationSchema } from "@/contracts/ui-catalog";
import { resolveCustomView } from "@/custom/registry";

const ActionDemo = defineAsyncComponent(
  () => import("@/components/ActionDemo.vue"),
);
const TableView = defineAsyncComponent(
  () => import("@/components/TableView.vue"),
);

const token = ref(sessionStorage.getItem("yang.token") ?? "");
const tenantId = ref(sessionStorage.getItem("yang.tenant-id") ?? "");
const query = ref("");
const catalog = ref<UiCatalog>();
const selectedOperationId = ref("");
const selectedViewId = ref("");
const navigationMode = ref<"views" | "actions">("views");
const navigationOptions = [
  { label: "业务页面", value: "views" },
  { label: "接口演示", value: "actions" },
];
const loading = ref(false);
const customLoading = ref(false);
const customComponent = shallowRef<Component>();
const customPresentation = ref<ActionPresentationSchema>();
const catalogError = ref<{ message: string; details?: string[] }>();
let controller: AbortController | undefined;
let sessionReloadTimer: number | undefined;
const catalogCache = new CatalogCache();

const session = computed<SessionContext>(() => ({
  token: token.value || undefined,
  tenantId: tenantId.value || undefined,
}));
const actions = computed(() => {
  const keyword = query.value.trim().toLocaleLowerCase();
  if (!keyword) return catalog.value?.actions ?? [];
  return (catalog.value?.actions ?? []).filter((action) =>
    [action.operation_id, action.title, action.description, action.path]
      .join(" ")
      .toLocaleLowerCase()
      .includes(keyword),
  );
});
const views = computed(() => {
  const keyword = query.value.trim().toLocaleLowerCase();
  if (!keyword) return catalog.value?.table_views ?? [];
  return (catalog.value?.table_views ?? []).filter((view) =>
    [view.view_id, view.title, view.table, view.data_action]
      .join(" ")
      .toLocaleLowerCase()
      .includes(keyword),
  );
});
const selectedView = computed(() => {
  const all = catalog.value?.table_views ?? [];
  return all.find((view) => view.view_id === selectedViewId.value) ?? all[0];
});
const selectedAction = computed<ActionDemoSchema | undefined>(() => {
  const all = catalog.value?.actions ?? [];
  return (
    all.find((action) => action.operation_id === selectedOperationId.value) ??
    all[0]
  );
});

watch([token, tenantId], ([nextToken, nextTenant]) => {
  sessionStorage.setItem("yang.token", nextToken);
  sessionStorage.setItem("yang.tenant-id", nextTenant);
  customComponent.value = undefined;
  if (sessionReloadTimer !== undefined) window.clearTimeout(sessionReloadTimer);
  sessionReloadTimer = window.setTimeout(() => void loadCatalog(), 400);
});

async function loadCatalog() {
  controller?.abort();
  controller = new AbortController();
  loading.value = true;
  catalogError.value = undefined;
  try {
    const fetched = await fetchUiCatalog(session.value, controller.signal);
    catalog.value = catalogCache.accept(session.value, fetched);
    const currentStillExists = catalog.value.actions.some(
      (action) => action.operation_id === selectedOperationId.value,
    );
    if (!currentStillExists)
      selectedOperationId.value = catalog.value.actions[0]?.operation_id ?? "";
    const currentViewStillExists = catalog.value.table_views.some(
      (view) => view.view_id === selectedViewId.value,
    );
    if (!currentViewStillExists)
      selectedViewId.value = catalog.value.table_views[0]?.view_id ?? "";
    if (!catalog.value.table_views.length) navigationMode.value = "actions";
  } catch (cause) {
    if (cause instanceof Error && cause.name === "AbortError") return;
    catalog.value = undefined;
    catalogError.value =
      cause instanceof ContractError
        ? { message: cause.message, details: cause.details }
        : { message: cause instanceof Error ? cause.message : String(cause) };
  } finally {
    loading.value = false;
  }
}

async function openCustomAction(presentation: ActionPresentationSchema) {
  const loader = resolveCustomView(presentation.view_id);
  if (!loader) {
    ElMessage.warning(
      `自定义页面 ${presentation.view_id ?? "未声明"} 未注册，已保留通用 TableView`,
    );
    return;
  }
  customLoading.value = true;
  try {
    customComponent.value = (await loader()).default;
    customPresentation.value = presentation;
  } catch (cause) {
    customComponent.value = undefined;
    customPresentation.value = undefined;
    ElMessage.error(
      `自定义页面加载失败，已回退通用 TableView：${cause instanceof Error ? cause.message : String(cause)}`,
    );
  } finally {
    customLoading.value = false;
  }
}

onMounted(loadCatalog);
onBeforeUnmount(() => {
  controller?.abort();
  if (sessionReloadTimer !== undefined) window.clearTimeout(sessionReloadTimer);
});
</script>

<template>
  <el-container class="app-shell">
    <el-header class="app-header">
      <div class="brand">
        <span class="brand-mark">Y</span>
        <div>
          <strong>YANG 接口工作台</strong>
          <small>后端注册即可演示，复杂场景允许覆盖</small>
        </div>
      </div>
      <div class="session-settings">
        <el-input
          v-model="tenantId"
          placeholder="租户 ID（可选）"
          clearable
          class="tenant-input"
        />
        <el-input
          v-model="token"
          placeholder="Bearer Token（仅本会话）"
          type="password"
          show-password
          clearable
        />
        <el-button :loading="loading" @click="loadCatalog">刷新目录</el-button>
      </div>
    </el-header>

    <el-container class="workspace">
      <el-aside width="320px" class="action-sidebar">
        <div class="sidebar-tools">
          <el-segmented
            v-model="navigationMode"
            :options="navigationOptions"
            block
            class="navigation-mode"
          />
          <el-input
            v-model="query"
            :placeholder="
              navigationMode === 'views'
                ? '搜索业务页面'
                : '搜索 Action、路径或说明'
            "
            clearable
          />
          <div class="catalog-meta">
            <span v-if="navigationMode === 'views'"
              >{{ catalog?.table_views.length ?? 0 }} Views</span
            >
            <span v-else>{{ catalog?.actions.length ?? 0 }} Actions</span>
            <el-tag v-if="catalog" size="small" effect="plain"
              >schema {{ catalog.schema_version }}</el-tag
            >
          </div>
        </div>
        <el-scrollbar class="action-list">
          <button
            v-for="view in navigationMode === 'views' ? views : []"
            :key="view.view_id"
            type="button"
            class="action-item"
            :class="{ active: selectedView?.view_id === view.view_id }"
            @click="selectedViewId = view.view_id"
          >
            <span class="action-item-title">{{
              view.title || view.table
            }}</span>
            <code>{{ view.view_id }} · {{ view.columns.length }} columns</code>
          </button>
          <button
            v-for="action in navigationMode === 'actions' ? actions : []"
            :key="action.operation_id"
            type="button"
            class="action-item"
            :class="{
              active: selectedAction?.operation_id === action.operation_id,
            }"
            @click="selectedOperationId = action.operation_id"
          >
            <span class="action-item-title">{{
              action.title || action.operation_id
            }}</span>
            <code>{{ action.method }} {{ action.path }}</code>
          </button>
          <el-empty
            v-if="
              !loading &&
              (navigationMode === 'views'
                ? views.length === 0
                : actions.length === 0)
            "
            :description="
              navigationMode === 'views'
                ? '没有匹配的业务页面'
                : '没有匹配的 Action'
            "
            :image-size="72"
          />
        </el-scrollbar>
      </el-aside>

      <el-main class="main-panel" v-loading="loading">
        <el-alert
          v-if="catalogError"
          type="error"
          :title="catalogError.message"
          :description="catalogError.details?.join('\n')"
          :closable="false"
          show-icon
        />
        <component
          :is="customComponent"
          v-else-if="customComponent && customPresentation && catalog"
          :presentation="customPresentation"
          :actions="catalog.actions"
          :session="session"
          @close="customComponent = undefined"
        />
        <TableView
          v-else-if="navigationMode === 'views' && selectedView && catalog"
          v-loading="customLoading"
          :view="selectedView"
          :actions="catalog.actions"
          :session="session"
          @custom-action="openCustomAction"
        />
        <ActionDemo
          v-else-if="navigationMode === 'actions' && selectedAction"
          :action="selectedAction"
          :session="session"
        />
        <el-empty
          v-else-if="!loading"
          description="后端目录中没有当前身份可访问的 Action"
        />
      </el-main>
    </el-container>
  </el-container>
</template>

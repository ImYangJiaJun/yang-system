<script setup lang="ts">
import { computed, ref } from "vue";
import { storeToRefs } from "pinia";
import { useRouter } from "vue-router";
import { useQuasar } from "quasar";
import { useApplicationSession } from "src/composables/useApplicationSession";
import { useApplicationLifecycleStore } from "stores/application-lifecycle";
import { useCatalogStore } from "stores/catalog";
import {
  navigationOptions,
  useCatalogNavigationStore,
} from "stores/catalog-navigation";
import { useSessionStore } from "stores/session";
import { useTenantStore } from "stores/tenant";

const drawerOpen = ref(true);
const router = useRouter();
const $q = useQuasar();
const catalogStore = useCatalogStore();
const navigationStore = useCatalogNavigationStore();
const sessionStore = useSessionStore();
const tenantStore = useTenantStore();
const lifecycleStore = useApplicationLifecycleStore();
const applicationSession = useApplicationSession();
const { catalog, loading } = storeToRefs(catalogStore);
const {
  actions,
  navigationMode,
  query,
  selectedAction,
  selectedOperationId,
  selectedView,
  selectedViewId,
  views,
} = storeToRefs(navigationStore);
const { loggedIn } = storeToRefs(sessionStore);
const { tenantId } = storeToRefs(tenantStore);

const emptyMessage = computed(() =>
  navigationMode.value === "views" ? "没有匹配的业务页面" : "没有匹配的 Action",
);
async function endSession() {
  await applicationSession.endSession();
  await router.push("/login");
}

// 切换深浅色并持久化偏好，启动时由 boot/theme.ts 恢复
function toggleDark() {
  $q.dark.toggle();
  localStorage.setItem("ys-theme", $q.dark.isActive ? "dark" : "light");
}
</script>

<template>
  <q-layout view="hHh Lpr fFf" class="app-shell">
    <q-header class="app-header">
      <q-toolbar class="app-toolbar">
        <q-btn
          flat
          dense
          round
          icon="menu"
          aria-label="切换导航"
          @click="drawerOpen = !drawerOpen"
        />
        <div class="brand">
          <span class="brand-mark">Y</span>
          <div>
            <strong>YANG 接口工作台</strong>
            <small>后端注册即可演示，复杂场景允许覆盖</small>
          </div>
        </div>
        <q-badge outline color="primary" label="开发工具" />
        <q-space />
        <q-btn
          flat
          dense
          round
          :icon="$q.dark.isActive ? 'light_mode' : 'dark_mode'"
          aria-label="切换深浅色"
          @click="toggleDark"
        />
        <div class="session-settings">
          <q-input
            :model-value="tenantId"
            dense
            outlined
            placeholder="租户 ID（可选）"
            clearable
            class="tenant-input"
            @update:model-value="tenantStore.setTenantId(String($event ?? ''))"
          />
          <q-btn flat color="primary" icon="home" label="正式控制台" to="/" />
          <q-btn
            outline
            color="primary"
            label="刷新目录"
            :loading="loading"
            @click="lifecycleStore.reloadCatalog"
          />
          <q-btn
            v-if="loggedIn"
            flat
            color="primary"
            label="退出全部设备"
            @click="endSession"
          />
          <q-btn v-else unelevated color="primary" label="登录" to="/login" />
        </div>
      </q-toolbar>
    </q-header>

    <q-drawer v-model="drawerOpen" show-if-above bordered :width="320">
      <aside class="action-sidebar">
        <div class="sidebar-tools">
          <q-option-group
            v-model="navigationMode"
            :options="navigationOptions"
            type="radio"
            inline
            dense
            color="primary"
            class="navigation-mode"
          />
          <q-input
            v-model="query"
            dense
            outlined
            clearable
            :placeholder="
              navigationMode === 'views'
                ? '搜索业务页面'
                : '搜索 Action、路径或说明'
            "
          >
            <template #prepend><q-icon name="search" /></template>
          </q-input>
          <div class="catalog-meta">
            <span v-if="navigationMode === 'views'">
              {{ catalog?.table_views.length ?? 0 }} Views
            </span>
            <span v-else>{{ catalog?.actions.length ?? 0 }} Actions</span>
            <q-badge v-if="catalog" outline color="primary">
              schema {{ catalog.schema_version }}
            </q-badge>
          </div>
        </div>

        <q-scroll-area class="action-list">
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
            <span class="action-item-title">
              {{ action.title || action.operation_id }}
            </span>
            <code>{{ action.method }} {{ action.path }}</code>
          </button>
          <div
            v-if="
              !loading &&
              (navigationMode === 'views'
                ? views.length === 0
                : actions.length === 0)
            "
            class="empty-state"
          >
            <q-icon name="search_off" size="40px" />
            <span>{{ emptyMessage }}</span>
          </div>
        </q-scroll-area>
      </aside>
    </q-drawer>

    <q-page-container>
      <router-view />
    </q-page-container>
  </q-layout>
</template>

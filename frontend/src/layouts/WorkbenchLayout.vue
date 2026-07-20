<script setup lang="ts">
import { computed, ref } from "vue";
import { storeToRefs } from "pinia";
import { useRouter } from "vue-router";
import { navigationOptions, useCatalogStore } from "stores/catalog";

const drawerOpen = ref(true);
const router = useRouter();
const store = useCatalogStore();
const {
  actions,
  catalog,
  loading,
  navigationMode,
  query,
  selectedAction,
  selectedOperationId,
  selectedView,
  selectedViewId,
  tenantId,
  token,
  views,
} = storeToRefs(store);

const emptyMessage = computed(() =>
  navigationMode.value === "views" ? "没有匹配的业务页面" : "没有匹配的 Action",
);
const loggedIn = computed(() => Boolean(token.value.trim()));

async function endSession() {
  store.clearSession();
  await router.push("/login");
}

store.start();
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
        <q-badge outline color="cyan-2" text-color="cyan-2" label="开发工具" />
        <q-space />
        <div class="session-settings">
          <q-input
            v-model="tenantId"
            dense
            standout="bg-white text-dark"
            placeholder="租户 ID（可选）"
            clearable
            class="tenant-input"
          />
          <q-btn flat color="white" icon="home" label="正式控制台" to="/" />
          <q-btn
            outline
            color="white"
            label="刷新目录"
            :loading="loading"
            @click="store.loadCatalog"
          />
          <q-btn
            v-if="loggedIn"
            flat
            color="white"
            label="退出"
            @click="endSession"
          />
          <q-btn
            v-else
            unelevated
            color="white"
            text-color="primary"
            label="登录"
            to="/login"
          />
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

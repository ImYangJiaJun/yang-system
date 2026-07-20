<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { storeToRefs } from "pinia";
import { useRoute, useRouter } from "vue-router";
import AccountSwitcher from "components/account/AccountSwitcher.vue";
import {
  buildAccountModulePages,
  modulesForIdentity,
  unassignedViews,
  visibleAccountIdentities,
  type AccountIdentity,
} from "src/module-pages";
import { useCatalogStore } from "stores/catalog";

const drawerOpen = ref(true);
const route = useRoute();
const router = useRouter();
const store = useCatalogStore();
const {
  accountIdentity,
  catalog,
  error,
  loading,
  selectedView,
  selectedViewId,
  token,
  views,
} = storeToRefs(store);

const loggedIn = computed(() => Boolean(token.value.trim()));
const businessMode = computed(() => route.path === "/business");
const moduleMode = computed(() => route.path.startsWith("/module/"));
const navigationMode = computed(() => businessMode.value || moduleMode.value);
const catalogRevision = computed(() => catalog.value?.revision || "尚未加载");
const modulePages = computed(() => buildAccountModulePages(catalog.value));
const businessViews = computed(() => unassignedViews(catalog.value));
const currentModule = computed(() =>
  modulePages.value.find(
    (module) => module.id === String(route.params.moduleId ?? ""),
  ),
);
const activeIdentity = computed<AccountIdentity>(
  () => currentModule.value?.identity ?? accountIdentity.value,
);
const currentIdentityModules = computed(() =>
  modulesForIdentity(modulePages.value, activeIdentity.value),
);
const identityTabs = computed(() =>
  visibleAccountIdentities(modulePages.value).flatMap((identity) => {
    const first = modulePages.value.find(
      (module) => module.identity === identity.id,
    );
    return first ? [{ ...identity, to: `/module/${first.id}` }] : [];
  }),
);

async function openBusinessView(viewId: string) {
  selectedViewId.value = viewId;
  await router.push("/business");
}

async function openModule(moduleId: string) {
  await router.push(`/module/${moduleId}`);
}

async function endSession() {
  store.clearSession();
  await router.push("/login");
}

watch(
  currentModule,
  (module) => {
    if (module && module.identity !== accountIdentity.value) {
      store.selectAccountIdentity(module.identity);
    }
  },
  { immediate: true },
);

store.start();
</script>

<template>
  <q-layout view="hHh LpR fFf" class="formal-shell">
    <q-header elevated class="formal-header">
      <q-toolbar class="formal-toolbar">
        <q-btn
          flat
          dense
          round
          icon="menu"
          aria-label="切换导航"
          @click="drawerOpen = !drawerOpen"
        />
        <router-link to="/" class="formal-brand" aria-label="返回应用中心">
          <strong>YANG System</strong>
        </router-link>
        <q-tabs
          inline-label
          stretch
          shrink
          active-color="white"
          indicator-color="transparent"
          class="formal-addon-tabs text-blue-grey-3"
        >
          <q-route-tab exact to="/" icon="apps" label="应用中心" />
          <q-route-tab
            v-for="identity in identityTabs"
            :key="identity.id"
            :to="identity.to"
            :icon="identity.icon"
            :label="identity.title"
          />
          <q-route-tab
            v-if="businessViews.length"
            to="/business"
            icon="business"
            label="业务空间"
          />
        </q-tabs>
        <q-space />
        <AccountSwitcher v-if="loggedIn" @logout="endSession" />
        <q-btn
          v-else
          flat
          color="white"
          icon="login"
          label="登录"
          to="/login"
        />
      </q-toolbar>
    </q-header>

    <q-drawer
      v-if="navigationMode"
      v-model="drawerOpen"
      show-if-above
      :width="200"
      class="formal-drawer"
    >
      <aside class="formal-navigation">
        <q-list padding class="formal-nav-list">
          <q-item
            v-ripple
            clickable
            exact
            to="/"
            active-class="formal-nav-active"
          >
            <q-item-section avatar
              ><q-icon name="space_dashboard"
            /></q-item-section>
            <q-item-section>
              <q-item-label>控制台</q-item-label>
            </q-item-section>
          </q-item>

          <template v-if="moduleMode">
            <q-item-label header class="formal-nav-heading">
              {{
                identityTabs.find((item) => item.id === activeIdentity)?.title
              }}模块
            </q-item-label>
            <q-item
              v-for="module in currentIdentityModules"
              :key="module.id"
              v-ripple
              clickable
              :active="currentModule?.id === module.id"
              active-class="formal-nav-active"
              @click="openModule(module.id)"
            >
              <q-item-section avatar>
                <q-icon :name="module.icon" />
              </q-item-section>
              <q-item-section>
                <q-item-label>{{ module.title }}</q-item-label>
                <q-item-label caption>{{ module.id }}</q-item-label>
              </q-item-section>
            </q-item>
          </template>

          <template v-else>
            <q-item-label header class="formal-nav-heading">
              业务菜单
            </q-item-label>
            <q-item
              v-for="view in views"
              :key="view.view_id"
              v-ripple
              clickable
              :active="selectedView?.view_id === view.view_id"
              active-class="formal-nav-active"
              @click="openBusinessView(view.view_id)"
            >
              <q-item-section avatar
                ><q-icon name="view_list"
              /></q-item-section>
              <q-item-section>
                <q-item-label>{{ view.title || view.table }}</q-item-label>
              </q-item-section>
            </q-item>
          </template>
          <q-item v-if="loading" class="nav-loading">
            <q-item-section avatar
              ><q-spinner color="primary" size="22px"
            /></q-item-section>
            <q-item-section>正在加载业务目录</q-item-section>
          </q-item>
          <q-item
            v-else-if="
              moduleMode ? !currentIdentityModules.length : !views.length
            "
            class="nav-empty"
          >
            <q-item-section avatar><q-icon name="inbox" /></q-item-section>
            <q-item-section>
              <q-item-label>暂无可访问页面</q-item-label>
              <q-item-label caption>{{
                error?.message || "后端尚未投影当前身份的 Module"
              }}</q-item-label>
            </q-item-section>
          </q-item>
        </q-list>

        <div class="formal-drawer-footer">
          <span>目录版本</span>
          <code>{{ catalogRevision }}</code>
          <div class="drawer-status">
            <span class="status-dot" :class="{ online: Boolean(catalog) }" />
            {{ catalog ? "目录已连接" : "等待目录" }}
          </div>
        </div>
      </aside>
    </q-drawer>

    <q-page-container>
      <router-view />
    </q-page-container>
  </q-layout>
</template>

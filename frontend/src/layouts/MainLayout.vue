<script setup lang="ts">
import { computed, ref } from "vue";
import { storeToRefs } from "pinia";
import { useRoute, useRouter } from "vue-router";
import { useQuasar } from "quasar";
import AccountSwitcher from "components/account/AccountSwitcher.vue";
import {
  buildAccountModulePages,
  modulesForIdentity,
  visibleAccountIdentities,
} from "src/module-pages";
import { useApplicationSession } from "src/composables/useApplicationSession";
import { useCatalogStore } from "stores/catalog";
import { useCatalogNavigationStore } from "stores/catalog-navigation";
import { useIdentityStore } from "stores/identity";
import { useSessionStore } from "stores/session";

const drawerOpen = ref(true);
const route = useRoute();
const router = useRouter();
const $q = useQuasar();
const catalogStore = useCatalogStore();
const navigationStore = useCatalogNavigationStore();
const identityStore = useIdentityStore();
const sessionStore = useSessionStore();
const applicationSession = useApplicationSession();
const { catalog, error, loading } = storeToRefs(catalogStore);
const { selectedView, selectedViewId, views } = storeToRefs(navigationStore);
const { accountIdentity } = storeToRefs(identityStore);
const { loggedIn } = storeToRefs(sessionStore);

const businessMode = computed(() => route.path === "/business");
const moduleMode = computed(() => route.path.startsWith("/module/"));
const navigationMode = computed(() => businessMode.value || moduleMode.value);
const modulePages = computed(() => buildAccountModulePages(catalog.value));
const identities = computed(() =>
  visibleAccountIdentities(modulePages.value, catalog.value),
);
const currentModule = computed(() =>
  modulePages.value.find(
    (module) => module.id === String(route.params.moduleId ?? ""),
  ),
);
const currentIdentityModules = computed(() =>
  modulesForIdentity(modulePages.value, accountIdentity.value),
);
const homeTarget = computed(() => {
  if (businessMode.value) return "/business";
  const firstModule = currentIdentityModules.value[0];
  return firstModule ? `/module/${firstModule.id}` : "/roles";
});
const activeIdentityTitle = computed(
  () =>
    identities.value.find((identity) => identity.id === accountIdentity.value)
      ?.title ?? "未选择角色",
);

async function openBusinessView(viewId: string) {
  selectedViewId.value = viewId;
  await router.push("/business");
}

async function openModule(moduleId: string) {
  await router.push(`/module/${moduleId}`);
}

async function endSession() {
  await applicationSession.endSession();
  await router.push("/login");
}

function confirmDisableAccount() {
  $q.dialog({
    title: "停用帐号",
    message: "停用后当前账号的全部会话都会失效，且不能自行恢复。确定继续吗？",
    cancel: true,
    persistent: true,
    ok: { color: "negative", label: "确认停用" },
  }).onOk(() => {
    void disableAccount();
  });
}

async function disableAccount() {
  try {
    const disabled = await applicationSession.disableAccount();
    if (disabled) await router.push("/login");
  } catch (error: unknown) {
    $q.notify({
      type: "negative",
      message: error instanceof Error ? error.message : "停用帐号失败",
    });
  }
}

// 切换深浅色并持久化偏好，启动时由 boot/theme.ts 恢复
function toggleDark() {
  $q.dark.toggle();
  localStorage.setItem("ys-theme", $q.dark.isActive ? "dark" : "light");
}
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
        <router-link
          :to="homeTarget"
          class="formal-brand"
          aria-label="返回当前角色首页"
        >
          <span class="formal-brand-mark">Y</span>
          <strong>YANG System</strong>
        </router-link>
        <div class="formal-context" aria-live="polite">
          <span>{{ activeIdentityTitle }}</span>
          <q-icon name="chevron_right" size="16px" />
          <strong>{{
            currentModule?.title || selectedView?.title || "业务页面"
          }}</strong>
        </div>
        <q-space />
        <q-btn
          flat
          dense
          round
          :icon="$q.dark.isActive ? 'light_mode' : 'dark_mode'"
          aria-label="切换深浅色"
          @click="toggleDark"
        />
        <AccountSwitcher
          v-if="loggedIn"
          @disable="confirmDisableAccount"
          @logout="endSession"
        />
        <q-btn
          v-else
          flat
          color="primary"
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
      :width="240"
      class="formal-drawer"
    >
      <aside class="formal-navigation">
        <div class="workspace-summary">
          <span class="workspace-label">当前角色</span>
          <strong>{{ activeIdentityTitle }}</strong>
          <small>选择模块开始处理业务</small>
        </div>
        <nav
          class="formal-nav-list"
          :aria-label="moduleMode ? '业务模块' : '业务菜单'"
          :data-testid="moduleMode ? 'module-navigation' : undefined"
        >
          <q-list padding role="none">
            <template v-if="moduleMode">
              <q-item-label header class="formal-nav-heading">
                业务模块
              </q-item-label>
              <q-item
                v-for="module in currentIdentityModules"
                :key="module.id"
                v-ripple
                clickable
                :active="currentModule?.id === module.id"
                :data-testid="`module-nav-${module.id}`"
                active-class="formal-nav-active"
                @click="openModule(module.id)"
              >
                <q-item-section avatar>
                  <q-icon :name="module.icon" />
                </q-item-section>
                <q-item-section>
                  <q-item-label>{{ module.title }}</q-item-label>
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
        </nav>

        <div class="formal-drawer-footer">
          <div class="drawer-status">
            <span class="status-dot" :class="{ online: Boolean(catalog) }" />
            {{ catalog ? "业务服务已连接" : "正在连接业务服务" }}
          </div>
        </div>
      </aside>
    </q-drawer>

    <q-page-container>
      <router-view />
    </q-page-container>
  </q-layout>
</template>

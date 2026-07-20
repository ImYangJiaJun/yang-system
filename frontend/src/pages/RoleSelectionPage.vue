<script setup lang="ts">
import { computed } from "vue";
import { storeToRefs } from "pinia";
import { useRouter } from "vue-router";
import {
  buildAccountModulePages,
  visibleAccountIdentities,
  type AccountIdentity,
} from "src/module-pages";
import { useCatalogStore } from "stores/catalog";

const router = useRouter();
const store = useCatalogStore();
const { catalog, error, loading } = storeToRefs(store);

const modulePages = computed(() => buildAccountModulePages(catalog.value));
const roles = computed(() => visibleAccountIdentities(modulePages.value));

async function selectRole(identity: AccountIdentity) {
  const firstModule = modulePages.value.find(
    (module) => module.identity === identity,
  );
  if (!firstModule) return;
  store.selectAccountIdentity(identity);
  await router.replace(`/module/${firstModule.id}`);
}

async function logout() {
  store.clearSession();
  await router.replace("/login");
}

store.start();
</script>

<template>
  <main class="role-selection-page">
    <header class="role-selection-header">
      <router-link to="/roles" class="role-selection-brand">
        <q-avatar size="38px" color="white" text-color="primary">Y</q-avatar>
        <strong>YANG System</strong>
      </router-link>
      <q-btn
        flat
        color="white"
        icon="logout"
        label="退出登录"
        @click="logout"
      />
    </header>

    <section class="role-selection-content">
      <div class="role-selection-heading">
        <span>ACCOUNT ROLE</span>
        <h1>选择本次使用的角色</h1>
        <p>角色决定本次会话可进入的业务模块，之后仍可从账号菜单切换。</p>
      </div>

      <div v-if="loading && !catalog" class="role-selection-status">
        <q-spinner color="primary" size="42px" />
        <span>正在读取当前账号的可用角色</span>
      </div>

      <q-banner
        v-else-if="error"
        rounded
        class="role-selection-error bg-red-1 text-negative"
      >
        <template #avatar><q-icon name="error" /></template>
        <strong>角色目录加载失败</strong>
        <div>{{ error.message }}</div>
        <template #action>
          <q-btn
            flat
            color="negative"
            label="重试"
            @click="store.loadCatalog"
          />
        </template>
      </q-banner>

      <div v-else-if="roles.length" class="role-selection-grid">
        <q-card
          v-for="role in roles"
          :key="role.id"
          flat
          bordered
          class="role-selection-card"
          :data-testid="`role-option-${role.id}`"
        >
          <q-card-section>
            <q-avatar size="58px" color="teal-1" text-color="primary">
              <q-icon :name="role.icon" size="30px" />
            </q-avatar>
            <h2>{{ role.title }}</h2>
            <p>
              可使用
              {{
                modulePages.filter((module) => module.identity === role.id)
                  .length
              }}
              个模块
            </p>
          </q-card-section>
          <q-card-actions>
            <q-btn
              unelevated
              color="primary"
              class="full-width"
              :label="`以${role.title}进入`"
              :aria-label="`选择${role.title}角色`"
              @click="selectRole(role.id)"
            />
          </q-card-actions>
        </q-card>
      </div>

      <div v-else class="role-selection-status">
        <q-icon name="person_off" color="blue-grey-5" size="48px" />
        <strong>当前账号没有可用角色</strong>
        <span>请联系管理员配置角色与模块权限。</span>
      </div>
    </section>
  </main>
</template>

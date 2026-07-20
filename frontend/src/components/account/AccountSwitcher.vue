<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { storeToRefs } from "pinia";
import { useRoute, useRouter } from "vue-router";
import {
  buildAccountModulePages,
  visibleAccountIdentities,
  type AccountIdentity,
} from "src/module-pages";
import type { OrganizationSummary } from "src/contracts/account-data";
import { useCatalogStore } from "stores/catalog";

const emit = defineEmits<{ logout: [] }>();
const route = useRoute();
const router = useRouter();
const store = useCatalogStore();
const {
  catalog,
  organizations,
  organizationsError,
  organizationsLoading,
  selectedOrganization,
  tenantId,
} = storeToRefs(store);
const menuOpen = ref(false);

const modulePages = computed(() => buildAccountModulePages(catalog.value));
const identities = computed(() => visibleAccountIdentities(modulePages.value));
const currentModule = computed(() =>
  modulePages.value.find(
    (module) => module.id === String(route.params.moduleId ?? ""),
  ),
);
const activeIdentity = computed<AccountIdentity>(
  () => currentModule.value?.identity ?? "user",
);
const activeIdentityTitle = computed(
  () =>
    identities.value.find((identity) => identity.id === activeIdentity.value)
      ?.title ?? "个人账户",
);
const contextLabel = computed(() =>
  activeIdentity.value === "org" && selectedOrganization.value
    ? selectedOrganization.value.name
    : activeIdentityTitle.value,
);

async function switchIdentity(identity: AccountIdentity) {
  const first = modulePages.value.find(
    (module) => module.identity === identity,
  );
  if (!first) return;
  menuOpen.value = false;
  await router.push(`/module/${first.id}`);
}

async function switchOrganization(organization: OrganizationSummary) {
  store.selectOrganization(organization);
  menuOpen.value = false;
  await router.push("/module/org.access");
}

async function openOrganizationManagement() {
  menuOpen.value = false;
  await router.push("/module/org.access");
}

watch(menuOpen, (open) => {
  if (open) void store.loadOrganizations();
});
</script>

<template>
  <div class="account-switcher">
    <span class="account-context-label">{{ contextLabel }}</span>
    <q-btn flat round dense aria-label="账号菜单">
      <q-avatar size="32px" color="white" text-color="primary">Y</q-avatar>
      <q-menu
        v-model="menuOpen"
        fit
        :offset="[0, 10]"
        class="account-menu account-switcher-menu"
      >
        <q-card flat>
          <q-item dense>
            <q-item-section />
            <q-item-section side>
              <q-btn
                v-close-popup
                flat
                round
                dense
                icon="close"
                aria-label="关闭"
              />
            </q-item-section>
          </q-item>

          <q-card-section class="text-center q-pt-none">
            <q-avatar size="56px" color="primary" text-color="white"
              >Y</q-avatar
            >
            <div class="text-subtitle1 text-weight-medium q-mt-sm">
              {{ contextLabel }}
            </div>
            <div class="text-caption text-grey-7">已建立安全会话</div>
            <q-btn
              rounded
              outline
              color="primary"
              label="管理您的账户"
              class="q-mt-md"
              @click="switchIdentity('user')"
            />
          </q-card-section>

          <q-separator inset />
          <q-list padding>
            <q-item-label header>账号身份</q-item-label>
            <q-item
              v-for="identity in identities.filter((item) => item.id !== 'org')"
              :key="identity.id"
              tag="label"
              clickable
            >
              <q-item-section avatar>
                <q-radio
                  :model-value="activeIdentity"
                  :val="identity.id"
                  @update:model-value="switchIdentity(identity.id)"
                />
              </q-item-section>
              <q-item-section>
                <q-item-label>{{ identity.title }}</q-item-label>
              </q-item-section>
              <q-item-section v-if="identity.id === 'admin'" side>
                <q-badge outline color="primary" label="平台" />
              </q-item-section>
            </q-item>
          </q-list>

          <q-card-section
            v-if="identities.some((identity) => identity.id === 'org')"
            class="q-pt-none"
          >
            <q-item-label header class="q-px-none">企业账户</q-item-label>
            <q-list bordered separator class="organization-switch-list">
              <q-item v-if="organizationsLoading">
                <q-item-section avatar
                  ><q-spinner color="primary"
                /></q-item-section>
                <q-item-section>正在加载我的企业</q-item-section>
              </q-item>
              <q-item v-else-if="organizationsError">
                <q-item-section avatar>
                  <q-icon name="error" color="negative" />
                </q-item-section>
                <q-item-section>
                  <q-item-label>企业列表加载失败</q-item-label>
                  <q-item-label caption>{{ organizationsError }}</q-item-label>
                </q-item-section>
                <q-item-section side>
                  <q-btn
                    flat
                    color="primary"
                    label="重试"
                    @click="store.loadOrganizations"
                  />
                </q-item-section>
              </q-item>
              <q-item
                v-for="organization in organizations"
                v-else
                :key="organization.id"
                tag="label"
                clickable
              >
                <q-item-section avatar>
                  <q-radio
                    :model-value="tenantId"
                    :val="String(organization.id)"
                    @update:model-value="switchOrganization(organization)"
                  />
                </q-item-section>
                <q-item-section>
                  <q-item-label>{{ organization.name }}</q-item-label>
                  <q-item-label caption>{{ organization.code }}</q-item-label>
                </q-item-section>
              </q-item>
              <q-item v-if="!organizationsLoading && !organizations.length">
                <q-item-section avatar
                  ><q-icon name="domain_add"
                /></q-item-section>
                <q-item-section>
                  <q-item-label>尚未加入企业</q-item-label>
                  <q-item-label caption
                    >进入我的企业页面创建企业账户</q-item-label
                  >
                </q-item-section>
              </q-item>
            </q-list>
            <q-btn
              flat
              color="primary"
              icon="settings"
              label="管理企业账户"
              class="full-width q-mt-sm"
              @click="openOrganizationManagement"
            />
          </q-card-section>

          <q-separator inset />
          <q-item v-close-popup clickable to="/workbench">
            <q-item-section avatar>
              <q-icon color="blue-grey-7" name="terminal" />
            </q-item-section>
            <q-item-section>
              <q-item-label>开发工作台</q-item-label>
              <q-item-label caption>查看完整 Action 与目录契约</q-item-label>
            </q-item-section>
          </q-item>
          <q-separator inset />
          <q-card-actions align="center">
            <q-btn
              flat
              color="negative"
              icon="logout"
              label="退出帐号"
              @click="emit('logout')"
            />
          </q-card-actions>
        </q-card>
      </q-menu>
    </q-btn>
  </div>
</template>

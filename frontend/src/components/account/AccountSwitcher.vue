<script setup lang="ts">
import { computed, ref } from "vue";
import { storeToRefs } from "pinia";
import { useRouter } from "vue-router";
import {
  buildAccountModulePages,
  visibleAccountIdentities,
  type AccountIdentity,
} from "src/module-pages";
import { useCatalogStore } from "stores/catalog";
import { useIdentityStore } from "stores/identity";

const emit = defineEmits<{ disable: []; logout: [] }>();
const router = useRouter();
const catalogStore = useCatalogStore();
const identityStore = useIdentityStore();
const { catalog } = storeToRefs(catalogStore);
const { accountIdentity } = storeToRefs(identityStore);
const menuOpen = ref(false);

const modulePages = computed(() => buildAccountModulePages(catalog.value));
const identities = computed(() =>
  visibleAccountIdentities(modulePages.value, catalog.value),
);
const activeIdentity = computed(() => accountIdentity.value);
const contextLabel = computed(
  () =>
    identities.value.find((identity) => identity.id === activeIdentity.value)
      ?.title ?? "未选择角色",
);

async function switchIdentity(identity: AccountIdentity) {
  const first = modulePages.value.find(
    (module) => module.identity === identity,
  );
  if (!first) return;
  identityStore.select(identity);
  menuOpen.value = false;
  await router.push(`/module/${first.id}`);
}
</script>

<template>
  <div class="account-switcher">
    <span class="account-context-label">{{ contextLabel }}</span>
    <q-btn flat round dense aria-label="账号菜单">
      <q-avatar size="32px" color="primary" text-color="white">Y</q-avatar>
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
            <q-item-label header>切换角色</q-item-label>
            <q-item
              v-for="identity in identities"
              :key="identity.id"
              clickable
              :active="activeIdentity === identity.id"
              active-class="account-switcher-active"
              @click="switchIdentity(identity.id)"
            >
              <q-item-section avatar>
                <q-icon :name="identity.icon" />
              </q-item-section>
              <q-item-section>
                <q-item-label>{{ identity.title }}</q-item-label>
              </q-item-section>
              <q-item-section side>
                <q-icon
                  v-if="activeIdentity === identity.id"
                  name="check_circle"
                  color="primary"
                />
              </q-item-section>
            </q-item>
          </q-list>

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
              icon="person_off"
              label="停用帐号"
              @click="emit('disable')"
            />
            <q-btn
              flat
              color="negative"
              icon="logout"
              label="退出全部设备"
              @click="emit('logout')"
            />
          </q-card-actions>
        </q-card>
      </q-menu>
    </q-btn>
  </div>
</template>

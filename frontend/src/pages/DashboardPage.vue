<script setup lang="ts">
import { computed, ref } from "vue";
import { storeToRefs } from "pinia";
import { useRouter } from "vue-router";
import {
  buildAccountModulePages,
  modulesForIdentity,
  unassignedViews,
} from "src/module-pages";
import { useApplicationLifecycleStore } from "stores/application-lifecycle";
import { useCatalogStore } from "stores/catalog";
import { useCatalogNavigationStore } from "stores/catalog-navigation";
import { useIdentityStore } from "stores/identity";

const router = useRouter();
const catalogStore = useCatalogStore();
const navigationStore = useCatalogNavigationStore();
const identityStore = useIdentityStore();
const lifecycleStore = useApplicationLifecycleStore();
const { catalog, error, loading } = storeToRefs(catalogStore);
const { selectedViewId } = storeToRefs(navigationStore);
const { accountIdentity } = storeToRefs(identityStore);
const query = ref("");

const modulePages = computed(() => buildAccountModulePages(catalog.value));
const identityModules = computed(() =>
  modulesForIdentity(modulePages.value, accountIdentity.value),
);
const businessViews = computed(() => unassignedViews(catalog.value));
const filteredModules = computed(() => {
  const keyword = query.value.trim().toLocaleLowerCase();
  if (!keyword) return identityModules.value;
  return identityModules.value.filter((module) =>
    [module.id, module.title, module.description]
      .join(" ")
      .toLocaleLowerCase()
      .includes(keyword),
  );
});
const filteredViews = computed(() => {
  const keyword = query.value.trim().toLocaleLowerCase();
  if (!keyword) return businessViews.value;
  return businessViews.value.filter((view) =>
    [view.title, view.table, view.view_id]
      .filter(Boolean)
      .some((value) => value.toLocaleLowerCase().includes(keyword)),
  );
});

async function openBusinessView(viewId: string) {
  selectedViewId.value = viewId;
  await router.push("/business");
}

async function openModule(moduleId: string) {
  await router.push(`/module/${moduleId}`);
}
</script>

<template>
  <q-page padding class="dashboard-page">
    <div class="dashboard-title-row">
      <div>
        <h1>应用中心</h1>
        <p>选择当前账号可访问的业务模块</p>
      </div>
      <q-chip
        square
        outline
        color="primary"
        :icon="catalog ? 'cloud_done' : 'cloud_off'"
      >
        {{ catalog ? "服务已连接" : "等待服务" }}
      </q-chip>
    </div>

    <q-banner
      v-if="error"
      rounded
      class="dashboard-alert bg-red-1 text-negative"
    >
      <template #avatar><q-icon name="cloud_off" /></template>
      {{ error.message }}
      <template #action>
        <q-btn
          flat
          color="negative"
          label="重新加载"
          @click="lifecycleStore.reloadCatalog"
        />
      </template>
    </q-banner>

    <div
      v-if="identityModules.length + businessViews.length > 1"
      class="row no-gutters q-mb-lg"
    >
      <q-input
        v-model="query"
        outlined
        clearable
        label="搜索功能"
        class="col-12 col-md-6 offset-md-3"
      >
        <template #prepend><q-icon name="search" /></template>
      </q-input>
    </div>

    <div class="q-col-gutter-md row">
      <div
        v-for="module in filteredModules"
        :key="module.id"
        class="col-12 col-sm-6 col-md-4 col-lg-3"
      >
        <q-card
          flat
          bordered
          class="application-card cursor-pointer full-height"
          :data-testid="`module-page-${module.id}`"
          @click="openModule(module.id)"
        >
          <q-card-section horizontal>
            <q-card-section class="row items-center">
              <q-avatar
                color="primary"
                text-color="white"
                size="72px"
                :icon="module.icon"
              />
            </q-card-section>
            <q-separator vertical inset />
            <q-card-section class="full-width">
              <q-item dense>
                <q-item-section>
                  <q-item-label class="text-h6">{{
                    module.title
                  }}</q-item-label>
                </q-item-section>
                <q-item-section side>
                  <q-item-label caption>{{ module.id }}</q-item-label>
                </q-item-section>
              </q-item>
              <q-separator inset />
              <q-card-section class="text-caption text-grey-7">
                {{ module.description }}
              </q-card-section>
            </q-card-section>
          </q-card-section>
        </q-card>
      </div>

      <div
        v-for="view in filteredViews"
        :key="view.view_id"
        class="col-12 col-sm-6 col-md-4 col-lg-3"
      >
        <q-card
          flat
          bordered
          class="application-card cursor-pointer full-height"
          @click="openBusinessView(view.view_id)"
        >
          <q-card-section horizontal>
            <q-card-section class="row items-center">
              <q-avatar
                color="primary"
                text-color="white"
                size="72px"
                icon="view_list"
              />
            </q-card-section>
            <q-separator vertical inset />
            <q-card-section class="full-width">
              <q-item dense>
                <q-item-section>
                  <q-item-label class="text-h6">{{
                    view.title || view.table
                  }}</q-item-label>
                </q-item-section>
                <q-item-section side>
                  <q-item-label caption>{{ view.table }}</q-item-label>
                </q-item-section>
              </q-item>
              <q-separator inset />
              <q-card-section class="text-caption text-grey-7 ellipsis">
                {{ view.columns.length }} 个字段 · 契约驱动页面
              </q-card-section>
            </q-card-section>
          </q-card-section>
        </q-card>
      </div>
    </div>

    <q-inner-loading :showing="loading">
      <q-spinner color="primary" size="48px" />
    </q-inner-loading>
  </q-page>
</template>

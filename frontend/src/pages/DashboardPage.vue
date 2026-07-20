<script setup lang="ts">
import { computed, ref } from "vue";
import { storeToRefs } from "pinia";
import { useRouter } from "vue-router";
import { unassignedViews, visibleAccountSpaces } from "src/account-spaces";
import { useCatalogStore } from "stores/catalog";

const router = useRouter();
const store = useCatalogStore();
const { catalog, error, loading, selectedViewId, token } = storeToRefs(store);
const query = ref("");

const loggedIn = computed(() => Boolean(token.value.trim()));
const accountSpaces = computed(() => visibleAccountSpaces(catalog.value));
const businessViews = computed(() => unassignedViews(catalog.value));
const filteredSpaces = computed(() => {
  const keyword = query.value.trim().toLocaleLowerCase();
  if (!keyword) return accountSpaces.value;
  return accountSpaces.value.filter((space) =>
    [space.title, space.subtitle, space.description]
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

async function openAccountSpace(spaceId: string) {
  await router.push(`/space/${spaceId}`);
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
          @click="store.loadCatalog"
        />
      </template>
    </q-banner>

    <div
      v-if="accountSpaces.length + businessViews.length > 1"
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
        v-for="space in filteredSpaces"
        :key="space.id"
        class="col-12 col-sm-6 col-md-4 col-lg-3"
      >
        <q-card
          flat
          bordered
          class="application-card cursor-pointer full-height"
          :data-testid="`account-space-${space.id}`"
          @click="openAccountSpace(space.id)"
        >
          <q-card-section horizontal>
            <q-card-section class="row items-center">
              <q-avatar
                color="primary"
                text-color="white"
                size="72px"
                :icon="space.icon"
              />
            </q-card-section>
            <q-separator vertical inset />
            <q-card-section class="full-width">
              <q-item dense>
                <q-item-section>
                  <q-item-label class="text-h6">{{ space.title }}</q-item-label>
                </q-item-section>
                <q-item-section side>
                  <q-item-label caption>{{ space.subtitle }}</q-item-label>
                </q-item-section>
              </q-item>
              <q-separator inset />
              <q-card-section class="text-caption text-grey-7">
                {{
                  space.id === "user" && !loggedIn
                    ? "登录后进入个人账户"
                    : `${space.views.length} 个页面 · ${space.actions.length} 项操作`
                }}
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

<script setup lang="ts">
import { defineAsyncComponent, shallowRef, watch, type Component } from "vue";
import { storeToRefs } from "pinia";
import { useQuasar } from "quasar";
import type { ActionPresentationSchema } from "src/contracts/ui-catalog";
import { resolveCustomView } from "src/custom/registry";
import { useApplicationSession } from "src/composables/useApplicationSession";
import { useCatalogStore } from "stores/catalog";
import { useCatalogNavigationStore } from "stores/catalog-navigation";

const ActionDemo = defineAsyncComponent(
  () => import("components/action/ActionDemo.vue"),
);
const TableView = defineAsyncComponent(
  () => import("components/table/TableView.vue"),
);

const $q = useQuasar();
const catalogStore = useCatalogStore();
const navigationStore = useCatalogNavigationStore();
const { session } = useApplicationSession();
const { catalog, error, loading } = storeToRefs(catalogStore);
const { navigationMode, selectedAction, selectedView } =
  storeToRefs(navigationStore);
const customLoading = shallowRef(false);
const customComponent = shallowRef<Component>();
const customPresentation = shallowRef<ActionPresentationSchema>();

async function openCustomAction(presentation: ActionPresentationSchema) {
  const loader = resolveCustomView(presentation.view_id);
  if (!loader) {
    $q.notify({
      type: "warning",
      message: `自定义页面 ${presentation.view_id ?? "未声明"} 未注册，已保留通用 TableView`,
    });
    return;
  }
  customLoading.value = true;
  try {
    customComponent.value = (await loader()).default;
    customPresentation.value = presentation;
  } catch (cause) {
    customComponent.value = undefined;
    customPresentation.value = undefined;
    $q.notify({
      type: "negative",
      message: `自定义页面加载失败，已回退通用 TableView：${
        cause instanceof Error ? cause.message : String(cause)
      }`,
    });
  } finally {
    customLoading.value = false;
  }
}

watch(session, () => {
  customComponent.value = undefined;
  customPresentation.value = undefined;
});
</script>

<template>
  <q-page class="main-panel relative-position">
    <q-banner v-if="error" rounded class="bg-red-1 text-negative">
      <template #avatar><q-icon name="error" /></template>
      <strong>{{ error.message }}</strong>
      <div v-if="error.details?.length" class="q-mt-xs">
        {{ error.details.join("\n") }}
      </div>
    </q-banner>
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
      :view="selectedView"
      :actions="catalog.actions"
      :session="session"
      developer
      @custom-action="openCustomAction"
    />
    <ActionDemo
      v-else-if="navigationMode === 'actions' && selectedAction"
      :action="selectedAction"
      :session="session"
    />
    <div v-else-if="!loading" class="empty-state main-empty-state">
      <q-icon name="inbox" size="54px" />
      <span>后端目录中没有当前身份可访问的 Action</span>
    </div>
    <q-inner-loading :showing="loading || customLoading">
      <q-spinner-gears size="50px" color="primary" />
    </q-inner-loading>
  </q-page>
</template>

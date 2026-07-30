<script setup lang="ts">
import { shallowRef, watch, type Component } from "vue";
import { storeToRefs } from "pinia";
import { useQuasar } from "quasar";
import TableView from "components/table/TableView.vue";
import type { ActionPresentationSchema } from "src/contracts/ui-catalog";
import { resolveCustomView } from "src/custom/registry";
import { useApplicationSession } from "src/composables/useApplicationSession";
import { useApplicationLifecycleStore } from "stores/application-lifecycle";
import { useCatalogStore } from "stores/catalog";
import { useCatalogNavigationStore } from "stores/catalog-navigation";

const $q = useQuasar();
const catalogStore = useCatalogStore();
const navigationStore = useCatalogNavigationStore();
const lifecycleStore = useApplicationLifecycleStore();
const { session } = useApplicationSession();
const { catalog, error, loading } = storeToRefs(catalogStore);
const { selectedView } = storeToRefs(navigationStore);
const customLoading = shallowRef(false);
const customComponent = shallowRef<Component>();
const customPresentation = shallowRef<ActionPresentationSchema>();

async function openCustomAction(presentation: ActionPresentationSchema) {
  const loader = resolveCustomView(presentation.view_id);
  if (!loader) {
    $q.notify({
      type: "warning",
      message: `自定义页面 ${presentation.view_id ?? "未声明"} 未注册，已保留通用业务页`,
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
      message: `自定义页面加载失败，已回退通用业务页：${
        cause instanceof Error ? cause.message : String(cause)
      }`,
    });
  } finally {
    customLoading.value = false;
  }
}

watch([session, selectedView], () => {
  customComponent.value = undefined;
  customPresentation.value = undefined;
});
</script>

<template>
  <q-page padding class="business-page relative-position">
    <q-banner v-if="error" rounded class="bg-red-1 text-negative">
      <template #avatar><q-icon name="error" /></template>
      <strong>{{ error.message }}</strong>
      <template #action>
        <q-btn
          flat
          color="negative"
          label="重新加载"
          @click="lifecycleStore.reloadCatalog"
        />
      </template>
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
      v-else-if="selectedView && catalog"
      :view="selectedView"
      :actions="catalog.actions"
      :session="session"
      @custom-action="openCustomAction"
    />
    <div v-else-if="!loading" class="business-empty">
      <q-icon name="space_dashboard" size="52px" />
      <h2>暂无可访问的业务页面</h2>
      <p>请确认当前身份拥有页面权限，或稍后刷新后端目录。</p>
      <q-btn
        outline
        color="primary"
        label="刷新目录"
        @click="lifecycleStore.reloadCatalog"
      />
    </div>
    <q-inner-loading :showing="loading || customLoading">
      <q-spinner-gears size="50px" color="primary" />
    </q-inner-loading>
  </q-page>
</template>

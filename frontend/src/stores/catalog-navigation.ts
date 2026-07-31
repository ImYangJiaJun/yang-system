import { computed, ref } from "vue";
import { defineStore } from "pinia";
import type { UiCatalog } from "src/contracts/ui-catalog";
import { productLowerCase } from "src/product-locale";
import { useCatalogStore } from "./catalog";

export type NavigationMode = "views" | "actions";

export const navigationOptions: Array<{
  label: string;
  value: NavigationMode;
}> = [
  { label: "业务页面", value: "views" },
  { label: "接口演示", value: "actions" },
];

export const useCatalogNavigationStore = defineStore(
  "catalog-navigation",
  () => {
    const catalogStore = useCatalogStore();
    const query = ref("");
    const selectedOperationId = ref("");
    const selectedViewId = ref("");
    const navigationMode = ref<NavigationMode>("views");

    const actions = computed(() => {
      const keyword = productLowerCase(query.value.trim());
      if (!keyword) return catalogStore.catalog?.actions ?? [];
      return (catalogStore.catalog?.actions ?? []).filter((action) =>
        [action.operation_id, action.title, action.description, action.path]
          .map(productLowerCase)
          .join(" ")
          .includes(keyword),
      );
    });
    const views = computed(() => {
      const keyword = productLowerCase(query.value.trim());
      if (!keyword) return catalogStore.catalog?.table_views ?? [];
      return (catalogStore.catalog?.table_views ?? []).filter((view) =>
        [view.view_id, view.title, view.table, view.data_action]
          .map(productLowerCase)
          .join(" ")
          .includes(keyword),
      );
    });
    const selectedView = computed(() => {
      const all = catalogStore.catalog?.table_views ?? [];
      return (
        all.find((view) => view.view_id === selectedViewId.value) ?? all[0]
      );
    });
    const selectedAction = computed(() => {
      const all = catalogStore.catalog?.actions ?? [];
      return (
        all.find(
          (action) => action.operation_id === selectedOperationId.value,
        ) ?? all[0]
      );
    });

    function reconcile(catalog: UiCatalog) {
      if (
        !catalog.actions.some(
          (action) => action.operation_id === selectedOperationId.value,
        )
      ) {
        selectedOperationId.value = catalog.actions[0]?.operation_id ?? "";
      }
      if (
        !catalog.table_views.some(
          (view) => view.view_id === selectedViewId.value,
        )
      ) {
        selectedViewId.value = catalog.table_views[0]?.view_id ?? "";
      }
      if (!catalog.table_views.length) navigationMode.value = "actions";
    }

    function reset() {
      query.value = "";
      selectedOperationId.value = "";
      selectedViewId.value = "";
      navigationMode.value = "views";
    }

    return {
      query,
      selectedOperationId,
      selectedViewId,
      navigationMode,
      actions,
      views,
      selectedView,
      selectedAction,
      reconcile,
      reset,
    };
  },
);

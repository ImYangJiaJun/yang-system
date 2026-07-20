import { computed, ref, watch } from "vue";
import { defineStore } from "pinia";
import { CatalogCache } from "src/api/catalog-cache";
import { fetchUiCatalog, type SessionContext } from "src/api/client";
import {
  ContractError,
  type ActionDemoSchema,
  type UiCatalog,
} from "src/contracts/ui-catalog";

export type NavigationMode = "views" | "actions";

export const navigationOptions: Array<{
  label: string;
  value: NavigationMode;
}> = [
  { label: "业务页面", value: "views" },
  { label: "接口演示", value: "actions" },
];

const catalogCache = new CatalogCache();

function sessionValue(key: string): string {
  return typeof window === "undefined"
    ? ""
    : (sessionStorage.getItem(key) ?? "");
}

export const useCatalogStore = defineStore("catalog", () => {
  const token = ref(sessionValue("yang.token"));
  const tenantId = ref(sessionValue("yang.tenant-id"));
  const query = ref("");
  const catalog = ref<UiCatalog>();
  const selectedOperationId = ref("");
  const selectedViewId = ref("");
  const navigationMode = ref<NavigationMode>("views");
  const loading = ref(false);
  const error = ref<{ message: string; details?: string[] }>();
  let activeRequest: { id: number; controller: AbortController } | undefined;
  let nextRequestId = 0;
  let sessionReloadTimer: number | undefined;
  let started = false;

  const session = computed<SessionContext>(() => ({
    token: token.value || undefined,
    tenantId: tenantId.value || undefined,
  }));
  const actions = computed(() => {
    const keyword = query.value.trim().toLocaleLowerCase();
    if (!keyword) return catalog.value?.actions ?? [];
    return (catalog.value?.actions ?? []).filter((action) =>
      [action.operation_id, action.title, action.description, action.path]
        .join(" ")
        .toLocaleLowerCase()
        .includes(keyword),
    );
  });
  const views = computed(() => {
    const keyword = query.value.trim().toLocaleLowerCase();
    if (!keyword) return catalog.value?.table_views ?? [];
    return (catalog.value?.table_views ?? []).filter((view) =>
      [view.view_id, view.title, view.table, view.data_action]
        .join(" ")
        .toLocaleLowerCase()
        .includes(keyword),
    );
  });
  const selectedView = computed(() => {
    const all = catalog.value?.table_views ?? [];
    return all.find((view) => view.view_id === selectedViewId.value) ?? all[0];
  });
  const selectedAction = computed<ActionDemoSchema | undefined>(() => {
    const all = catalog.value?.actions ?? [];
    return (
      all.find((action) => action.operation_id === selectedOperationId.value) ??
      all[0]
    );
  });

  async function loadCatalog() {
    activeRequest?.controller.abort();
    const request = {
      id: ++nextRequestId,
      controller: new AbortController(),
    };
    activeRequest = request;
    const requestSession = { ...session.value };
    loading.value = true;
    error.value = undefined;
    try {
      const fetched = await fetchUiCatalog(
        requestSession,
        request.controller.signal,
      );
      if (activeRequest?.id !== request.id) return;
      catalog.value = catalogCache.accept(fetched);
      if (
        !catalog.value.actions.some(
          (action) => action.operation_id === selectedOperationId.value,
        )
      ) {
        selectedOperationId.value =
          catalog.value.actions[0]?.operation_id ?? "";
      }
      if (
        !catalog.value.table_views.some(
          (view) => view.view_id === selectedViewId.value,
        )
      ) {
        selectedViewId.value = catalog.value.table_views[0]?.view_id ?? "";
      }
      if (!catalog.value.table_views.length) navigationMode.value = "actions";
    } catch (cause) {
      if (
        activeRequest?.id !== request.id ||
        (cause instanceof Error && cause.name === "AbortError")
      )
        return;
      catalog.value = undefined;
      error.value =
        cause instanceof ContractError
          ? { message: cause.message, details: cause.details }
          : { message: cause instanceof Error ? cause.message : String(cause) };
    } finally {
      if (activeRequest?.id === request.id) {
        activeRequest = undefined;
        loading.value = false;
      }
    }
  }

  function setAccessToken(accessToken: string) {
    token.value = accessToken;
    sessionStorage.setItem("yang.token", accessToken);
  }

  function clearSession() {
    token.value = "";
    tenantId.value = "";
    catalog.value = undefined;
    sessionStorage.removeItem("yang.token");
    sessionStorage.removeItem("yang.tenant-id");
  }

  function start() {
    if (started) return;
    started = true;
    watch([token, tenantId], ([nextToken, nextTenant]) => {
      sessionStorage.setItem("yang.token", nextToken);
      sessionStorage.setItem("yang.tenant-id", nextTenant);
      if (sessionReloadTimer !== undefined)
        window.clearTimeout(sessionReloadTimer);
      sessionReloadTimer = window.setTimeout(() => void loadCatalog(), 400);
    });
    void loadCatalog();
  }

  function stopPendingRequests() {
    activeRequest?.controller.abort();
    activeRequest = undefined;
    loading.value = false;
    if (sessionReloadTimer !== undefined)
      window.clearTimeout(sessionReloadTimer);
  }

  return {
    token,
    tenantId,
    query,
    catalog,
    selectedOperationId,
    selectedViewId,
    navigationMode,
    loading,
    error,
    session,
    actions,
    views,
    selectedView,
    selectedAction,
    setAccessToken,
    clearSession,
    loadCatalog,
    start,
    stopPendingRequests,
  };
});

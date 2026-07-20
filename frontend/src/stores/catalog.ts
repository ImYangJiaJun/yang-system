import { computed, ref, watch } from "vue";
import { defineStore } from "pinia";
import { CatalogCache } from "src/api/catalog-cache";
import {
  fetchUiCatalog,
  invokeAction,
  type SessionContext,
} from "src/api/client";
import {
  parseOrganizationsPage,
  type OrganizationSummary,
} from "src/contracts/account-data";
import {
  ContractError,
  type ActionDemoSchema,
  type UiCatalog,
} from "src/contracts/ui-catalog";
import type { AccountIdentity } from "src/module-pages";

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

function sessionIdentity(): AccountIdentity {
  const identity = sessionValue("yang.account-identity");
  return identity === "admin" || identity === "org" ? identity : "user";
}

export const useCatalogStore = defineStore("catalog", () => {
  const token = ref(sessionValue("yang.token"));
  const tenantId = ref(sessionValue("yang.tenant-id"));
  const accountIdentity = ref<AccountIdentity>(sessionIdentity());
  const query = ref("");
  const catalog = ref<UiCatalog>();
  const selectedOperationId = ref("");
  const selectedViewId = ref("");
  const navigationMode = ref<NavigationMode>("views");
  const loading = ref(false);
  const error = ref<{ message: string; details?: string[] }>();
  const organizations = ref<OrganizationSummary[]>([]);
  const organizationsLoading = ref(false);
  const organizationsError = ref("");
  let activeRequest: { id: number; controller: AbortController } | undefined;
  let organizationsController: AbortController | undefined;
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
  const selectedOrganization = computed(() =>
    organizations.value.find(
      (organization) => String(organization.id) === tenantId.value,
    ),
  );

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
    selectAccountIdentity("user");
  }

  function selectAccountIdentity(identity: AccountIdentity) {
    accountIdentity.value = identity;
    sessionStorage.setItem("yang.account-identity", identity);
  }

  async function loadOrganizations() {
    const action = catalog.value?.actions.find(
      (candidate) => candidate.operation_id === "org.access.list",
    );
    if (!token.value || !action) {
      organizations.value = [];
      organizationsError.value = "";
      return;
    }
    organizationsController?.abort();
    const controller = new AbortController();
    organizationsController = controller;
    organizationsLoading.value = true;
    organizationsError.value = "";
    try {
      const result = await invokeAction(
        action,
        { page: 1, limit: 100 },
        { token: token.value },
        controller.signal,
      );
      if (result.kind !== "json") {
        throw new ContractError("我的企业 Action 必须返回 JSON");
      }
      organizations.value = parseOrganizationsPage(result.data);
    } catch (cause) {
      if (cause instanceof Error && cause.name === "AbortError") return;
      organizations.value = [];
      organizationsError.value =
        cause instanceof Error ? cause.message : String(cause);
    } finally {
      if (organizationsController === controller) {
        organizationsController = undefined;
        organizationsLoading.value = false;
      }
    }
  }

  function selectOrganization(organization?: OrganizationSummary) {
    tenantId.value = organization ? String(organization.id) : "";
  }

  function clearSession() {
    token.value = "";
    tenantId.value = "";
    accountIdentity.value = "user";
    catalog.value = undefined;
    organizations.value = [];
    organizationsError.value = "";
    sessionStorage.removeItem("yang.token");
    sessionStorage.removeItem("yang.tenant-id");
    sessionStorage.removeItem("yang.account-identity");
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

  return {
    token,
    tenantId,
    accountIdentity,
    query,
    catalog,
    selectedOperationId,
    selectedViewId,
    navigationMode,
    loading,
    error,
    organizations,
    organizationsLoading,
    organizationsError,
    session,
    actions,
    views,
    selectedView,
    selectedAction,
    selectedOrganization,
    setAccessToken,
    selectAccountIdentity,
    clearSession,
    loadOrganizations,
    selectOrganization,
    loadCatalog,
    start,
  };
});

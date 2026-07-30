import { computed, ref } from "vue";
import { defineStore } from "pinia";
import { invokeAction } from "src/api/client";
import {
  parseOrganizationsPage,
  type OrganizationSummary,
} from "src/contracts/account-data";
import { ContractError, type ActionDemoSchema } from "src/contracts/ui-catalog";

const TENANT_KEY = "yang.tenant-id";

function storedTenantId(): string {
  return typeof sessionStorage === "undefined"
    ? ""
    : (sessionStorage.getItem(TENANT_KEY) ?? "");
}

export const useTenantStore = defineStore("tenant", () => {
  const tenantId = ref(storedTenantId());
  const organizations = ref<OrganizationSummary[]>([]);
  const loading = ref(false);
  const error = ref("");
  let activeRequest: { id: number; controller: AbortController } | undefined;
  let nextRequestId = 0;

  const selectedOrganization = computed(() =>
    organizations.value.find(
      (organization) => String(organization.id) === tenantId.value,
    ),
  );

  function setTenantId(value: string) {
    tenantId.value = value;
    if (typeof sessionStorage === "undefined") return;
    if (value) sessionStorage.setItem(TENANT_KEY, value);
    else sessionStorage.removeItem(TENANT_KEY);
  }

  function selectOrganization(organization?: OrganizationSummary) {
    setTenantId(organization ? String(organization.id) : "");
  }

  async function loadOrganizations(actions: ActionDemoSchema[], token: string) {
    const action = actions.find(
      (candidate) => candidate.operation_id === "org.tenant.list",
    );
    if (!token || !action) {
      organizations.value = [];
      error.value = "";
      return;
    }
    activeRequest?.controller.abort();
    const request = {
      id: ++nextRequestId,
      controller: new AbortController(),
    };
    activeRequest = request;
    loading.value = true;
    error.value = "";
    try {
      const result = await invokeAction(
        action,
        { page: 1, limit: 100 },
        { token },
        request.controller.signal,
      );
      if (activeRequest?.id !== request.id) return;
      if (result.kind !== "json") {
        throw new ContractError("我的企业 Action 必须返回 JSON");
      }
      organizations.value = parseOrganizationsPage(result.data);
    } catch (cause) {
      if (
        activeRequest?.id !== request.id ||
        (cause instanceof Error && cause.name === "AbortError")
      ) {
        return;
      }
      organizations.value = [];
      error.value = cause instanceof Error ? cause.message : String(cause);
    } finally {
      if (activeRequest?.id === request.id) {
        activeRequest = undefined;
        loading.value = false;
      }
    }
  }

  function cancelRequests() {
    activeRequest?.controller.abort();
    activeRequest = undefined;
    loading.value = false;
  }

  function clear() {
    cancelRequests();
    tenantId.value = "";
    organizations.value = [];
    error.value = "";
    if (typeof sessionStorage !== "undefined") {
      sessionStorage.removeItem(TENANT_KEY);
    }
  }

  return {
    tenantId,
    organizations,
    loading,
    error,
    selectedOrganization,
    setTenantId,
    selectOrganization,
    loadOrganizations,
    cancelRequests,
    clear,
  };
});

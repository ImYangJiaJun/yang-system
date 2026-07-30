import { ref } from "vue";
import { defineStore } from "pinia";
import { CatalogCache } from "src/api/catalog-cache";
import { fetchUiCatalog, type SessionContext } from "src/api/client";
import { ContractError, type UiCatalog } from "src/contracts/ui-catalog";

const catalogCache = new CatalogCache();

export const useCatalogStore = defineStore("catalog", () => {
  const catalog = ref<UiCatalog>();
  const loading = ref(false);
  const error = ref<{ message: string; details?: string[] }>();
  let activeRequest: { id: number; controller: AbortController } | undefined;
  let nextRequestId = 0;

  async function loadCatalog(
    session: SessionContext,
  ): Promise<UiCatalog | undefined> {
    activeRequest?.controller.abort();
    const request = {
      id: ++nextRequestId,
      controller: new AbortController(),
    };
    activeRequest = request;
    loading.value = true;
    error.value = undefined;
    try {
      const fetched = await fetchUiCatalog(
        session,
        request.controller.signal,
        catalogCache.value,
      );
      if (activeRequest?.id !== request.id) return undefined;
      catalog.value = catalogCache.accept(fetched);
      return catalog.value;
    } catch (cause) {
      if (
        activeRequest?.id !== request.id ||
        (cause instanceof Error && cause.name === "AbortError")
      ) {
        return undefined;
      }
      catalog.value = undefined;
      error.value =
        cause instanceof ContractError
          ? { message: cause.message, details: cause.details }
          : { message: cause instanceof Error ? cause.message : String(cause) };
      return undefined;
    } finally {
      if (activeRequest?.id === request.id) {
        activeRequest = undefined;
        loading.value = false;
      }
    }
  }

  function reset() {
    activeRequest?.controller.abort();
    activeRequest = undefined;
    catalog.value = undefined;
    loading.value = false;
    error.value = undefined;
  }

  return {
    catalog,
    loading,
    error,
    loadCatalog,
    reset,
  };
});

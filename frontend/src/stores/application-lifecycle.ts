import { computed, watch } from "vue";
import { defineStore } from "pinia";
import { useCatalogStore } from "./catalog";
import { useCatalogNavigationStore } from "./catalog-navigation";
import { useSessionStore } from "./session";
import { useTenantStore } from "./tenant";

export const useApplicationLifecycleStore = defineStore(
  "application-lifecycle",
  () => {
    const sessionStore = useSessionStore();
    const tenantStore = useTenantStore();
    const catalogStore = useCatalogStore();
    const navigationStore = useCatalogNavigationStore();
    let stopContextWatcher: (() => void) | undefined;
    let reloadTimer: ReturnType<typeof setTimeout> | undefined;
    let started = false;

    const session = computed(() => ({
      token: sessionStore.token || undefined,
      tenantId: tenantStore.tenantId || undefined,
    }));

    async function reloadCatalog() {
      const loaded = await catalogStore.loadCatalog({ ...session.value });
      if (loaded) navigationStore.reconcile(loaded);
    }

    function scheduleReload() {
      if (reloadTimer !== undefined) clearTimeout(reloadTimer);
      reloadTimer = setTimeout(() => {
        reloadTimer = undefined;
        void reloadCatalog();
      }, 400);
    }

    function dispose() {
      stopContextWatcher?.();
      stopContextWatcher = undefined;
      if (reloadTimer !== undefined) clearTimeout(reloadTimer);
      reloadTimer = undefined;
      tenantStore.cancelRequests();
      catalogStore.reset();
      started = false;
    }

    function start() {
      if (started) return dispose;
      started = true;
      stopContextWatcher = watch(
        [() => sessionStore.token, () => tenantStore.tenantId],
        scheduleReload,
      );
      void reloadCatalog();
      return dispose;
    }

    return {
      session,
      reloadCatalog,
      start,
      dispose,
    };
  },
);

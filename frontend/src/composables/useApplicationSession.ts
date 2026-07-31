import type { LoginResult } from "src/api/auth";
import { logout } from "src/api/auth";
import { storeToRefs } from "pinia";
import { useCatalogStore } from "src/stores/catalog";
import { useCatalogNavigationStore } from "src/stores/catalog-navigation";
import { useIdentityStore } from "src/stores/identity";
import { useApplicationLifecycleStore } from "src/stores/application-lifecycle";
import { useSessionStore } from "src/stores/session";
import { useTenantStore } from "src/stores/tenant";
import { publishSessionEnd } from "src/api/session-coordination";

export function useApplicationSession() {
  const sessionStore = useSessionStore();
  const identityStore = useIdentityStore();
  const tenantStore = useTenantStore();
  const catalogStore = useCatalogStore();
  const navigationStore = useCatalogNavigationStore();
  const lifecycleStore = useApplicationLifecycleStore();
  const { session } = storeToRefs(lifecycleStore);

  function beginSession(tokens: LoginResult) {
    identityStore.clear();
    tenantStore.clear();
    catalogStore.reset();
    navigationStore.reset();
    sessionStore.setTokenPair(tokens);
  }

  function acceptRefreshedTokenPair(tokens: LoginResult) {
    sessionStore.setTokenPair(tokens);
  }

  function clearSession() {
    sessionStore.clear();
    identityStore.clear();
    tenantStore.clear();
    catalogStore.reset();
    navigationStore.reset();
  }

  async function endSession() {
    try {
      await logout(sessionStore.token || undefined);
    } finally {
      clearSession();
      publishSessionEnd("logout");
    }
  }

  return {
    session,
    beginSession,
    acceptRefreshedTokenPair,
    clearSession,
    endSession,
  };
}

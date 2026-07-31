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
import { StepUpRequiredError } from "src/api/client";
import { requestStepUpProof } from "src/components/step-up/requestStepUpProof";

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
    } catch (error: unknown) {
      if (!(error instanceof StepUpRequiredError)) throw error;
      const proof = await requestStepUpProof(error.challenge, {
        token: sessionStore.token || undefined,
        tenantId: tenantStore.tenantId || undefined,
      });
      if (!proof) return;
      await logout(sessionStore.token || undefined, undefined, proof);
    }
    clearSession();
    publishSessionEnd("logout");
  }

  return {
    session,
    beginSession,
    acceptRefreshedTokenPair,
    clearSession,
    endSession,
  };
}

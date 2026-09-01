import type { LoginResult } from "src/api/auth";
import { disableAccount as requestDisableAccount, logout } from "src/api/auth";
import { storeToRefs } from "pinia";
import { useCatalogStore } from "src/stores/catalog";
import { useCatalogNavigationStore } from "src/stores/catalog-navigation";
import { useIdentityStore } from "src/stores/identity";
import { useApplicationLifecycleStore } from "src/stores/application-lifecycle";
import { useSessionStore } from "src/stores/session";
import { publishSessionEnd } from "src/api/session-coordination";
import { StepUpRequiredError } from "src/api/client";
import { requestStepUpProof } from "src/components/step-up/requestStepUpProof";

export function useApplicationSession() {
  const sessionStore = useSessionStore();
  const identityStore = useIdentityStore();
  const catalogStore = useCatalogStore();
  const navigationStore = useCatalogNavigationStore();
  const lifecycleStore = useApplicationLifecycleStore();
  const { session } = storeToRefs(lifecycleStore);

  function beginSession(tokens: LoginResult) {
    identityStore.clear();
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
    catalogStore.reset();
    navigationStore.reset();
  }

  async function endSession() {
    const completed = await runStepUpProtectedSessionMutation(logout);
    if (!completed) return;
    clearSession();
    publishSessionEnd("logout");
  }

  async function disableAccount() {
    const completed = await runStepUpProtectedSessionMutation(
      requestDisableAccount,
    );
    if (!completed) return false;
    clearSession();
    publishSessionEnd("logout");
    return true;
  }

  async function runStepUpProtectedSessionMutation(
    request: (
      accessToken: string | undefined,
      signal?: AbortSignal,
      proof?: string,
    ) => Promise<unknown>,
  ) {
    try {
      await request(sessionStore.token || undefined);
    } catch (error: unknown) {
      if (!(error instanceof StepUpRequiredError)) throw error;
      const proof = await requestStepUpProof(error.challenge, {
        token: sessionStore.token || undefined,
      });
      if (!proof) return false;
      await request(sessionStore.token || undefined, undefined, proof);
    }
    return true;
  }

  return {
    session,
    beginSession,
    acceptRefreshedTokenPair,
    clearSession,
    disableAccount,
    endSession,
  };
}

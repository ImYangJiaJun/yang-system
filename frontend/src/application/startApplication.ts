import type { LoginResult } from "src/api/auth";
import {
  SESSION_EXPIRED_EVENT,
  SESSION_RELOGIN_REQUIRED_EVENT,
  SESSION_REFRESHED_EVENT,
} from "src/api/auth-session";
import {
  publishSessionEnd,
  type SessionEndReason,
  subscribeSessionEnd,
} from "src/api/session-coordination";
import { useApplicationSession } from "src/composables/useApplicationSession";
import { useApplicationLifecycleStore } from "src/stores/application-lifecycle";
import type { Router } from "vue-router";

export type ApplicationRouter = Pick<Router, "currentRoute" | "replace">;

export function startApplication(router: ApplicationRouter) {
  const disposeLifecycle = useApplicationLifecycleStore().start();
  const endSession = (reason: SessionEndReason, publish: boolean) => {
    useApplicationSession().clearSession();
    if (publish) publishSessionEnd(reason);
    if (router.currentRoute.value.name === "login") return;
    void router.replace({
      name: "login",
      query: {
        reason:
          reason === "credentials-changed"
            ? "credentials-changed"
            : "session-expired",
      },
    });
  };
  const refreshed = (event: Event) => {
    const tokens = (event as CustomEvent<LoginResult>).detail;
    if (tokens) {
      useApplicationSession().acceptRefreshedTokenPair(tokens);
    }
  };
  const expired = () => {
    endSession("expired", true);
  };
  const credentialsChanged = () => {
    endSession("credentials-changed", true);
  };
  const disposeCoordination = subscribeSessionEnd((reason) =>
    endSession(reason, false),
  );

  window.addEventListener(SESSION_REFRESHED_EVENT, refreshed);
  window.addEventListener(SESSION_EXPIRED_EVENT, expired);
  window.addEventListener(SESSION_RELOGIN_REQUIRED_EVENT, credentialsChanged);

  return () => {
    window.removeEventListener(SESSION_REFRESHED_EVENT, refreshed);
    window.removeEventListener(SESSION_EXPIRED_EVENT, expired);
    window.removeEventListener(
      SESSION_RELOGIN_REQUIRED_EVENT,
      credentialsChanged,
    );
    disposeCoordination();
    disposeLifecycle();
  };
}

import type { LoginResult } from "src/api/auth";
import {
  SESSION_EXPIRED_EVENT,
  SESSION_REFRESHED_EVENT,
} from "src/api/auth-session";
import { useApplicationSession } from "src/composables/useApplicationSession";
import { useApplicationLifecycleStore } from "src/stores/application-lifecycle";
import type { Router } from "vue-router";

export type ApplicationRouter = Pick<Router, "currentRoute" | "replace">;

export function startApplication(router: ApplicationRouter) {
  const disposeLifecycle = useApplicationLifecycleStore().start();
  const refreshed = (event: Event) => {
    const tokens = (event as CustomEvent<LoginResult>).detail;
    if (tokens) {
      useApplicationSession().acceptRefreshedTokenPair(tokens);
    }
  };
  const expired = () => {
    useApplicationSession().clearSession();
    if (router.currentRoute.value.name === "login") return;
    void router.replace({
      name: "login",
      query: { reason: "session-expired" },
    });
  };

  window.addEventListener(SESSION_REFRESHED_EVENT, refreshed);
  window.addEventListener(SESSION_EXPIRED_EVENT, expired);

  return () => {
    window.removeEventListener(SESSION_REFRESHED_EVENT, refreshed);
    window.removeEventListener(SESSION_EXPIRED_EVENT, expired);
    disposeLifecycle();
  };
}

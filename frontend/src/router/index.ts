import { defineRouter } from "#q-app/wrappers";
import {
  createMemoryHistory,
  createRouter,
  createWebHistory,
} from "vue-router";
import { identityForModuleId } from "src/module-pages";
import type { LoginResult } from "src/api/auth";
import {
  SESSION_EXPIRED_EVENT,
  SESSION_REFRESHED_EVENT,
} from "src/api/auth-session";
import { useCatalogStore } from "src/stores/catalog";
import {
  readAccessState,
  resolveAccessRedirect,
  type AccessTarget,
} from "./access-policy";
import routes from "./routes";

export default defineRouter(() => {
  const router = createRouter({
    history: process.env.SERVER
      ? createMemoryHistory()
      : createWebHistory(process.env.VUE_ROUTER_BASE),
    routes,
    scrollBehavior: () => ({ left: 0, top: 0 }),
  });

  router.beforeEach((to) => {
    if (
      to.name !== "login" &&
      to.name !== "role-selection" &&
      !to.meta.requiresRole
    ) {
      return undefined;
    }
    const target: AccessTarget =
      to.name === "login"
        ? "login"
        : to.name === "role-selection"
          ? "role-selection"
          : to.meta.requiresRole
            ? "protected"
            : "role-selection";
    const targetIdentity =
      to.name === "module-page"
        ? identityForModuleId(String(to.params.moduleId ?? ""))
        : undefined;
    return resolveAccessRedirect(target, readAccessState(), targetIdentity);
  });

  if (typeof window !== "undefined") {
    window.addEventListener(SESSION_REFRESHED_EVENT, (event) => {
      const tokens = (event as CustomEvent<LoginResult>).detail;
      if (tokens) useCatalogStore().acceptRefreshedTokenPair(tokens);
    });
    window.addEventListener(SESSION_EXPIRED_EVENT, () => {
      useCatalogStore().clearSession();
      if (router.currentRoute.value.name === "login") return;
      void router.replace({
        name: "login",
        query: { reason: "session-expired" },
      });
    });
  }

  return router;
});

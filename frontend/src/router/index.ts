import { defineRouter } from "#q-app/wrappers";
import {
  createMemoryHistory,
  createRouter,
  createWebHistory,
} from "vue-router";
import { resolveAccessRedirect, type AccessTarget } from "./access-policy";
import routes from "./routes";
import { useIdentityStore } from "src/stores/identity";
import { useSessionStore } from "src/stores/session";

export default defineRouter(({ store }) => {
  const sessionStore = useSessionStore(store);
  const identityStore = useIdentityStore(store);
  const router = createRouter({
    history: process.env.SERVER
      ? createMemoryHistory()
      : createWebHistory(process.env.VUE_ROUTER_BASE),
    routes,
    scrollBehavior: () => ({ left: 0, top: 0 }),
  });

  router.beforeEach(async (to) => {
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
    await sessionStore.restoreFromCookie();
    return resolveAccessRedirect(target, {
      authenticated: sessionStore.loggedIn,
      accountIdentity: identityStore.accountIdentity,
    });
  });

  return router;
});

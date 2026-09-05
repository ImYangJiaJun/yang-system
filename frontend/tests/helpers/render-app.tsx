import { type ReactNode } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render } from "@testing-library/react";
import { createMemoryRouter, RouterProvider } from "react-router";

import { createSessionController } from "@/engine/session/session-controller";
import { SessionControllerContext } from "@/engine/session/use-session";
import { createIdentityStore, storeIdentity } from "@/features/auth/identity";
import { IdentityStoreContext } from "@/features/auth/use-identity";
import { appRoutes } from "@/shell/routes";

/**
 * 测试渲染 helper：与 App.tsx 相同的 provider 组合
 * （SessionController + IdentityStore + QueryClient + memory router）。
 * react-refresh 豁免见 eslint.config.js 的 tests/helpers 覆盖。
 */

function TestProviders({
  controller,
  identityStore,
  queryClient,
  router,
  children,
}: {
  controller: ReturnType<typeof createSessionController>;
  identityStore: ReturnType<typeof createIdentityStore>;
  queryClient: QueryClient;
  router: ReturnType<typeof createMemoryRouter>;
  children?: ReactNode;
}) {
  return (
    <SessionControllerContext.Provider value={controller}>
      <IdentityStoreContext.Provider value={identityStore}>
        <QueryClientProvider client={queryClient}>
          {children ?? <RouterProvider router={router} />}
        </QueryClientProvider>
      </IdentityStoreContext.Provider>
    </SessionControllerContext.Provider>
  );
}

export function renderTestApp(options: {
  path?: string;
  authenticated?: boolean;
  identity?: string;
  controller?: ReturnType<typeof createSessionController>;
}) {
  const controller = options.controller ?? createSessionController();
  if (options.authenticated ?? true) {
    controller.beginSession({ accessToken: "test-access" });
  }
  if (options.identity) storeIdentity(options.identity);
  const identityStore = createIdentityStore();
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const router = createMemoryRouter(appRoutes, {
    initialEntries: [options.path ?? "/"],
  });
  render(
    <TestProviders
      controller={controller}
      identityStore={identityStore}
      queryClient={queryClient}
      router={router}
    />,
  );
  return { controller, identityStore, router, queryClient };
}

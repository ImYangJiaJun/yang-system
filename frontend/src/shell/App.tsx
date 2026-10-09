import { useEffect, useMemo, useRef, useState } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createBrowserRouter, RouterProvider } from "react-router";

import { createSessionController } from "@/engine/session/session-controller";
import { SessionControllerContext } from "@/engine/session/use-session";
import { applyDensity, loadDensity } from "@/shell/density";
import { createSessionResetHandler } from "@/shell/session-reset";
import { createIdentityStore } from "@/features/auth/identity";
import { IdentityStoreContext } from "@/features/auth/use-identity";
import { ToastProvider } from "@/shared/providers/toast-provider";
import { Toaster } from "@/shared/ui/toaster";
import { appRoutes } from "./routes";

export default function App() {
  // 身份选择与 SessionController 同为外置 store；会话建立/清空时级联清空身份。
  const [identityStore] = useState(() => createIdentityStore());
  const identityResetRef = useRef<() => void>(() => undefined);
  const [controller] = useState(() =>
    createSessionController({
      onSessionReset: () => identityResetRef.current(),
    }),
  );
  const [queryClient] = useState(
    () => new QueryClient({ defaultOptions: { queries: { retry: 1 } } }),
  );
  const router = useMemo(() => createBrowserRouter(appRoutes), []);

  useEffect(() => {
    // 会话边界（beginSession / clearSession）时级联清空查询缓存与身份 store；
    // 轮换（acceptRefreshedTokenPair）不触发，详见 shell/session-reset.ts。
    identityResetRef.current = createSessionResetHandler({
      clearIdentity: () => identityStore.clear(),
      queryClient,
    });
  }, [identityStore, queryClient]);
  useEffect(() => {
    applyDensity(loadDensity());
  }, []);

  return (
    <SessionControllerContext.Provider value={controller}>
      <IdentityStoreContext.Provider value={identityStore}>
        <QueryClientProvider client={queryClient}>
          <ToastProvider>
            <RouterProvider router={router} />
            <Toaster />
          </ToastProvider>
        </QueryClientProvider>
      </IdentityStoreContext.Provider>
    </SessionControllerContext.Provider>
  );
}

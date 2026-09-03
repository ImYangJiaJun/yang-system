import { useMemo, useState } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createBrowserRouter, RouterProvider } from "react-router";

import { createSessionController } from "@/api/session-controller";
import { SessionControllerContext } from "@/api/use-session";
import { appRoutes } from "./app/routes";

export default function App() {
  const [controller] = useState(() => createSessionController());
  const [queryClient] = useState(
    () => new QueryClient({ defaultOptions: { queries: { retry: 1 } } }),
  );
  const router = useMemo(() => createBrowserRouter(appRoutes), []);

  return (
    <SessionControllerContext.Provider value={controller}>
      <QueryClientProvider client={queryClient}>
        <RouterProvider router={router} />
      </QueryClientProvider>
    </SessionControllerContext.Provider>
  );
}

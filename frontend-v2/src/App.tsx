import { useMemo, useRef, useState } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createBrowserRouter, RouterProvider } from "react-router";

import { createSessionController } from "@/api/session-controller";
import { SessionControllerContext } from "@/api/use-session";
import {
  StepUpDialogHost,
  type StepUpProofHandler,
} from "@/components/step-up/step-up-host";
import { appRoutes } from "./app/routes";

export default function App() {
  // SessionController 在 React 树外创建；Step-up UI 通过 delegate ref 晚绑定，
  // 宿主未挂载时 fail-loud 而不是静默吞掉 428。
  const stepUpDelegate = useRef<StepUpProofHandler | undefined>(undefined);
  const [controller] = useState(() =>
    createSessionController({
      requestStepUpProof: (challenge, session) => {
        const handler = stepUpDelegate.current;
        if (!handler) {
          return Promise.reject(new Error("Step-up 交互组件未就绪"));
        }
        return handler(challenge, session);
      },
    }),
  );
  const [queryClient] = useState(
    () => new QueryClient({ defaultOptions: { queries: { retry: 1 } } }),
  );
  const router = useMemo(() => createBrowserRouter(appRoutes), []);

  return (
    <SessionControllerContext.Provider value={controller}>
      <QueryClientProvider client={queryClient}>
        <RouterProvider router={router} />
        <StepUpDialogHost
          onReady={(handler) => {
            stepUpDelegate.current = handler;
          }}
        />
      </QueryClientProvider>
    </SessionControllerContext.Provider>
  );
}

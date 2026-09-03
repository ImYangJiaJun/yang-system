import AppLayout from "@/layout/AppLayout";
import ModulePage from "@/pages/ModulePage";
import LoginPage from "@/pages/LoginPage";

import { RedirectIfAuthed, RequireAuth } from "./auth-gate";
import HomeRedirect from "./home-redirect";
import SessionBridge from "./session-bridge";

export const appRoutes = [
  {
    // 会话失效事件桥接挂在路由树根部，登录页与受保护区共享。
    element: <SessionBridge />,
    children: [
      {
        path: "/login",
        element: (
          <RedirectIfAuthed>
            <LoginPage />
          </RedirectIfAuthed>
        ),
      },
      {
        path: "/",
        element: (
          <RequireAuth>
            <AppLayout />
          </RequireAuth>
        ),
        children: [
          { index: true, element: <HomeRedirect /> },
          { path: "m/:moduleId", element: <ModulePage /> },
          { path: "m/:moduleId/v/:viewId", element: <ModulePage /> },
        ],
      },
    ],
  },
];

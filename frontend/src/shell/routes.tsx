import AppLayout from "@/shell/AppLayout";
import BusinessPage from "@/shell/pages/BusinessPage";
import DashboardPage from "@/shell/pages/DashboardPage";
import LoginPage from "@/features/auth/pages/LoginPage";
import ModulePage from "@/shell/pages/ModulePage";
import RegisterPage from "@/features/auth/pages/RegisterPage";
import ResetPasswordPage from "@/features/auth/pages/ResetPasswordPage";
import SelectIdentityPage from "@/features/auth/pages/SelectIdentityPage";

import { RedirectIfAuthed, RequireAuth } from "./auth-gate";
import SessionBridge from "./session-bridge";

// 开发工作台仅开发构建可见：生产构建不含该路由（ADR-5 能力 14 安全姿态）。
const devOnlyRoutes = import.meta.env.DEV
  ? [
      {
        path: "workbench",
        lazy: async () => ({
          Component: (await import("@/shell/pages/WorkbenchPage")).default,
        }),
      },
    ]
  : [];

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
        path: "/register",
        element: (
          <RedirectIfAuthed>
            <RegisterPage />
          </RedirectIfAuthed>
        ),
      },
      {
        path: "/reset-password",
        element: (
          <RedirectIfAuthed>
            <ResetPasswordPage />
          </RedirectIfAuthed>
        ),
      },
      {
        path: "/select-identity",
        element: (
          <RequireAuth>
            <SelectIdentityPage />
          </RequireAuth>
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
          { index: true, element: <DashboardPage /> },
          { path: "m/:moduleId", element: <ModulePage /> },
          { path: "m/:moduleId/v/:viewId", element: <ModulePage /> },
          { path: "business", element: <BusinessPage /> },
          ...devOnlyRoutes,
        ],
      },
    ],
  },
];

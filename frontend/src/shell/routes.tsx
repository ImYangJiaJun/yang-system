import AppLayout from "@/shell/AppLayout";
import AccountSettingsPage from "@/features/account/AccountSettingsPage";
import BusinessPage from "@/shell/pages/BusinessPage";
import DashboardPage from "@/shell/pages/DashboardPage";
import LoginPage from "@/features/auth/pages/LoginPage";
import ModulePage from "@/shell/pages/ModulePage";
import RegisterPage from "@/features/auth/pages/RegisterPage";
import ResetPasswordPage from "@/features/auth/pages/ResetPasswordPage";
import SelectIdentityPage from "@/features/auth/pages/SelectIdentityPage";

import { RedirectIfAuthed, RequireAuth } from "./auth-gate";
import { RouteFallback } from "./RouteFallback";
import SessionBridge from "./session-bridge";

// 开发工作台仅开发构建可见：生产构建不含该路由（ADR-5 能力 14 安全姿态）。
const devOnlyRoutes = import.meta.env.DEV
  ? [
      {
        path: "workbench",
        lazy: async () => ({
          Component: (await import("@/shell/pages/WorkbenchPage")).default,
        }),
        hydrateFallbackElement: <RouteFallback />,
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
          { path: "account", element: <AccountSettingsPage /> },
          { path: "m/:moduleId", element: <ModulePage /> },
          { path: "m/:moduleId/v/:viewId", element: <ModulePage /> },
          { path: "business", element: <BusinessPage /> },
          {
            // 飞书数据源控制台：路由级 lazy，页面文件在 features/feishu/views/。
            // 列表页与详情页分属两个 chunk，详情页只在进入时加载。
            path: "feishu/datasources",
            lazy: async () => ({
              Component: (
                await import("@/features/feishu/views/DatasourceListPage")
              ).default,
            }),
            // 路由级 lazy 的页面在硬导航时必须给兜底：否则首次导航完成前是空白，
            // 而且 react-router 会打一条 HydrateFallback 警告（见 RouteFallback.tsx）。
            hydrateFallbackElement: <RouteFallback />,
          },
          {
            // 详情页按**表级主键**路由。它曾经按 `:sourceKey`（一条字段绑定的标识）
            // 路由，那是字段级时代的身份：一条表级行有 N 个 source_key，页面因此
            // 天生只能看一个字段，且查询端还拿它去 order_by/搜一张没有这一列的表。
            // 「一条数据源」的身份是 `id`；`source_key` 只在选项与凭据两处局部使用。
            path: "feishu/datasources/:id",
            lazy: async () => ({
              Component: (
                await import("@/features/feishu/views/DatasourceDetailPage")
              ).default,
            }),
            hydrateFallbackElement: <RouteFallback />,
          },
          {
            // 权限组管理面：同样路由级 lazy，页面文件在 features/access/views/。
            path: "access/groups",
            lazy: async () => ({
              Component: (
                await import("@/features/access/views/PermissionGroupsPage")
              ).default,
            }),
          },
          ...devOnlyRoutes,
        ],
      },
    ],
  },
];

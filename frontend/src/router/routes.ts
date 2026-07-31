import type { RouteRecordRaw } from "vue-router";

const workbenchRoutes: RouteRecordRaw[] = import.meta.env.DEV
  ? [
      {
        path: "/workbench",
        component: () => import("layouts/WorkbenchLayout.vue"),
        children: [
          {
            path: "",
            name: "workbench",
            component: () => import("pages/WorkbenchPage.vue"),
          },
        ],
      },
    ]
  : [];

const routes: RouteRecordRaw[] = [
  {
    path: "/login",
    name: "login",
    component: () => import("pages/LoginPage.vue"),
  },
  {
    path: "/register",
    name: "register",
    component: () => import("pages/RegisterPage.vue"),
  },
  {
    path: "/reset-password",
    name: "reset-password",
    component: () => import("pages/ResetPasswordPage.vue"),
  },
  {
    path: "/roles",
    name: "role-selection",
    component: () => import("pages/RoleSelectionPage.vue"),
  },
  {
    path: "/",
    component: () => import("layouts/MainLayout.vue"),
    meta: { requiresRole: true },
    children: [
      {
        path: "",
        redirect: "/roles",
      },
      {
        path: "business",
        name: "business",
        component: () => import("pages/BusinessPage.vue"),
      },
      {
        path: "module/:moduleId",
        name: "module-page",
        component: () => import("pages/ModulePage.vue"),
      },
    ],
  },
  ...workbenchRoutes,
  {
    path: "/:catchAll(.*)*",
    redirect: "/",
  },
];

export default routes;

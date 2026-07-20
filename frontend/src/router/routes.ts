import type { RouteRecordRaw } from "vue-router";

const routes: RouteRecordRaw[] = [
  {
    path: "/login",
    name: "login",
    component: () => import("pages/LoginPage.vue"),
  },
  {
    path: "/",
    component: () => import("layouts/MainLayout.vue"),
    children: [
      {
        path: "",
        name: "dashboard",
        component: () => import("pages/DashboardPage.vue"),
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
      {
        path: "space/:space",
        redirect: (route) => {
          const modules: Record<string, string> = {
            user: "account.user",
            admin: "admin.user",
            org: "org.tenant",
          };
          return `/module/${modules[String(route.params.space)] ?? "account.user"}`;
        },
      },
    ],
  },
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
  {
    path: "/:catchAll(.*)*",
    redirect: "/",
  },
];

export default routes;

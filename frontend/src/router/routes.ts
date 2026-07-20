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

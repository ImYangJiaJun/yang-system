import type { RouteRecordRaw } from "vue-router";

const routes: RouteRecordRaw[] = [
  {
    path: "/",
    component: () => import("layouts/MainLayout.vue"),
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

import type { Component } from "vue";

export type CustomViewModule = { default: Component };
export type CustomViewLoader = () => Promise<CustomViewModule>;

// 唯一允许的自定义页面解析入口。键和值都由前端构建产物静态确定，禁止根据后端
// 字符串拼接 import 路径。
const registry: Readonly<Record<string, CustomViewLoader>> = Object.freeze({
  "demo.items.insight": () => import("./DemoItemInsight.vue"),
});

export function resolveCustomView(
  viewId: string | null | undefined,
): CustomViewLoader | undefined {
  if (!viewId || !Object.prototype.hasOwnProperty.call(registry, viewId))
    return undefined;
  return registry[viewId];
}

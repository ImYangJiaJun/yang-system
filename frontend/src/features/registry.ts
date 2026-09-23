import { lazy, type LazyExoticComponent, type ComponentType } from "react";

import type {
  ActionDemoSchema,
  ActionPresentationSchema,
} from "@/engine/contracts/ui-catalog";

/// 自定义 View 的 props 契约（对齐旧 DemoItemInsight.vue 的 props/emit）。
export interface CustomViewProps {
  presentation: ActionPresentationSchema;
  actions: ActionDemoSchema[];
  onClose: () => void;
}

export type CustomViewComponent = LazyExoticComponent<
  ComponentType<CustomViewProps>
>;

// 唯一允许的自定义页面解析入口。键和值都由前端构建产物静态确定，禁止根据后端
// 字符串拼接 import 路径（React.lazy 的参数必须是静态字面量）。
const registry: Readonly<Record<string, CustomViewComponent>> = Object.freeze({
  "demo.items.insight": lazy(() => import("./demo/views/DemoItemInsight")),
  // 权限组管理面：通用 TableView 表达不了「组 + 权限条目 + 成员」这三块结构，
  // 所以按设计 §11.1 登记成自定义视图。键取后端 Action 名（`access.groups.list_groups`），
  // 与 `features/access/api.ts` 的 `GROUP_OPERATION_IDS.list` 同源。
  "access.groups.list_groups": lazy(
    () => import("./access/views/PermissionGroupsPage"),
  ),
});

export function resolveCustomView(
  viewId: string | null | undefined,
): CustomViewComponent | undefined {
  if (!viewId || !Object.prototype.hasOwnProperty.call(registry, viewId)) {
    return undefined;
  }
  return registry[viewId];
}

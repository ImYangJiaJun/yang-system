import type { TableViewSchema, UiCatalog } from "@/engine/contracts/ui-catalog";
import {
  buildAccountModulePages,
  unassignedViews,
  type ModulePageDefinition,
} from "@/engine/catalog/module-pages";

/**
 * 导航投影：后端 Catalog → 侧边栏条目。
 * 业务模块由 buildAccountModulePages 投影；未分配给任何 Module 的 TableView
 * （如无 Module 的演示后端）按旧 Workbench 语义合成单视图模块页兜底。
 */

export const WORKSPACE_IDENTITY = "workspace";

function syntheticPageForView(
  view: TableViewSchema,
  catalog: UiCatalog,
): ModulePageDefinition {
  const actionById = new Map(
    catalog.actions.map((action) => [action.operation_id, action]),
  );
  return {
    id: view.view_id,
    identity: WORKSPACE_IDENTITY,
    title: view.title || view.table,
    description: "",
    icon: "table",
    order: Number.MAX_SAFE_INTEGER,
    primaryAction: undefined,
    actions: view.actions.flatMap((operationId) => {
      const action = actionById.get(operationId);
      return action ? [action] : [];
    }),
    actionPresentations: [],
    views: [view],
  };
}

export function buildNavigationPages(
  catalog: UiCatalog | undefined,
): ModulePageDefinition[] {
  if (!catalog) return [];
  const modulePages = buildAccountModulePages(catalog);
  const synthetic = unassignedViews(catalog).map((view) =>
    syntheticPageForView(view, catalog),
  );
  return [...modulePages, ...synthetic];
}

export type NavigationGroup = {
  identity: string;
  title: string;
  pages: ModulePageDefinition[];
};

export function groupNavigationPages(
  pages: ModulePageDefinition[],
  catalog: UiCatalog | undefined,
): NavigationGroup[] {
  const identityTitles = new Map<string, string>();
  for (const module of catalog?.modules ?? []) {
    identityTitles.set(module.identity.id, module.identity.title);
  }
  identityTitles.set(WORKSPACE_IDENTITY, "工作台");
  const groups = new Map<string, NavigationGroup>();
  for (const page of pages) {
    const group = groups.get(page.identity) ?? {
      identity: page.identity,
      title: identityTitles.get(page.identity) ?? page.identity,
      pages: [],
    };
    group.pages.push(page);
    groups.set(page.identity, group);
  }
  return [...groups.values()];
}

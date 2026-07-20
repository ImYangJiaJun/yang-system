import type {
  ActionDemoSchema,
  TableViewSchema,
  UiCatalog,
} from "src/contracts/ui-catalog";

export type AccountSpaceId = "user" | "admin" | "org";

export interface AccountSpaceDefinition {
  id: AccountSpaceId;
  namespace: string;
  title: string;
  subtitle: string;
  description: string;
  icon: string;
}

export interface AccountSpaceSummary extends AccountSpaceDefinition {
  actions: ActionDemoSchema[];
  views: TableViewSchema[];
  available: boolean;
}

export const accountSpaceDefinitions: readonly AccountSpaceDefinition[] = [
  {
    id: "user",
    namespace: "account.user",
    title: "个人账户",
    subtitle: "User",
    description: "管理当前用户的身份、会话与个人安全设置。",
    icon: "account_circle",
  },
  {
    id: "admin",
    namespace: "admin",
    title: "管理平台",
    subtitle: "Admin",
    description: "维护平台账号、管理员授权与账号启停状态。",
    icon: "admin_panel_settings",
  },
  {
    id: "org",
    namespace: "org",
    title: "企业账户",
    subtitle: "Organization",
    description: "管理企业资料、成员关系与企业管理员。",
    icon: "domain",
  },
] as const;

function belongsTo(namespace: string, value: string): boolean {
  return value === namespace || value.startsWith(`${namespace}.`);
}

function viewBelongsTo(
  definition: AccountSpaceDefinition,
  view: TableViewSchema,
): boolean {
  return [view.view_id, view.data_action, view.table, ...view.actions].some(
    (value) => belongsTo(definition.namespace, value),
  );
}

export function summarizeAccountSpaces(
  catalog: UiCatalog | undefined,
): AccountSpaceSummary[] {
  const actions = catalog?.actions ?? [];
  const views = catalog?.table_views ?? [];
  return accountSpaceDefinitions.map((definition) => {
    const spaceActions = actions.filter((action) =>
      belongsTo(definition.namespace, action.operation_id),
    );
    const spaceViews = views.filter((view) => viewBelongsTo(definition, view));
    return {
      ...definition,
      actions: spaceActions,
      views: spaceViews,
      available:
        definition.id === "user" ||
        spaceActions.length > 0 ||
        spaceViews.length > 0,
    };
  });
}

export function visibleAccountSpaces(
  catalog: UiCatalog | undefined,
): AccountSpaceSummary[] {
  return summarizeAccountSpaces(catalog).filter((space) => space.available);
}

export function unassignedViews(
  catalog: UiCatalog | undefined,
): TableViewSchema[] {
  return (catalog?.table_views ?? []).filter(
    (view) =>
      !accountSpaceDefinitions.some((definition) =>
        viewBelongsTo(definition, view),
      ),
  );
}

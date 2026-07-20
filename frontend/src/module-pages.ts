import type {
  ActionDemoSchema,
  TableViewSchema,
  UiCatalog,
} from "src/contracts/ui-catalog";

export type AccountIdentity = "user" | "admin" | "org";

export interface AccountIdentityDefinition {
  id: AccountIdentity;
  title: string;
  icon: string;
}

export interface ModulePageDefinition {
  id: string;
  identity: AccountIdentity;
  title: string;
  description: string;
  icon: string;
  actions: ActionDemoSchema[];
  views: TableViewSchema[];
}

type KnownModule = Omit<ModulePageDefinition, "actions" | "views">;

export const accountIdentityDefinitions: readonly AccountIdentityDefinition[] =
  [
    { id: "user", title: "个人账户", icon: "account_circle" },
    { id: "admin", title: "管理平台", icon: "admin_panel_settings" },
    { id: "org", title: "企业账户", icon: "domain" },
  ] as const;

const knownModules: readonly KnownModule[] = [
  {
    id: "account.user",
    identity: "user",
    title: "用户中心",
    description: "查看当前用户身份并管理登录会话。",
    icon: "manage_accounts",
  },
  {
    id: "admin.user",
    identity: "admin",
    title: "平台账号",
    description: "查询、添加和维护平台管理员账号。",
    icon: "admin_panel_settings",
  },
  {
    id: "org.access",
    identity: "org",
    title: "我的企业",
    description: "选择已有企业或创建新的企业账户。",
    icon: "domain",
  },
  {
    id: "org.org",
    identity: "org",
    title: "企业资料",
    description: "查看当前企业的基础资料与状态。",
    icon: "apartment",
  },
  {
    id: "org.user",
    identity: "org",
    title: "企业成员",
    description: "维护企业成员、职务和企业管理员身份。",
    icon: "groups",
  },
] as const;

function moduleIdFromOperation(operationId: string): string {
  return operationId.split(".").slice(0, -1).join(".");
}

function viewBelongsTo(moduleId: string, view: TableViewSchema): boolean {
  return [view.view_id, view.data_action, ...view.actions].some(
    (value) =>
      value === moduleId ||
      value.startsWith(`${moduleId}.`) ||
      moduleIdFromOperation(value) === moduleId,
  );
}

export function buildAccountModulePages(
  catalog: UiCatalog | undefined,
): ModulePageDefinition[] {
  const actions = catalog?.actions ?? [];
  const views = catalog?.table_views ?? [];
  return knownModules.flatMap((definition) => {
    const moduleActions = actions.filter(
      (action) => moduleIdFromOperation(action.operation_id) === definition.id,
    );
    const moduleViews = views.filter((view) =>
      viewBelongsTo(definition.id, view),
    );
    if (!moduleActions.length && !moduleViews.length) return [];
    return [{ ...definition, actions: moduleActions, views: moduleViews }];
  });
}

export function modulesForIdentity(
  pages: ModulePageDefinition[],
  identity: AccountIdentity,
): ModulePageDefinition[] {
  return pages.filter((page) => page.identity === identity);
}

export function visibleAccountIdentities(
  pages: ModulePageDefinition[],
): AccountIdentityDefinition[] {
  return accountIdentityDefinitions.filter((identity) =>
    pages.some((page) => page.identity === identity.id),
  );
}

export function unassignedViews(
  catalog: UiCatalog | undefined,
): TableViewSchema[] {
  return (catalog?.table_views ?? []).filter(
    (view) =>
      !knownModules.some((definition) => viewBelongsTo(definition.id, view)),
  );
}

import type {
  ActionDemoSchema,
  ActionPresentationSchema,
  TableViewSchema,
  UiCatalog,
} from "@/contracts/ui-catalog";
import { compareProductText } from "@/lib/product-locale";

export type AccountIdentity = string;

export interface AccountIdentityDefinition {
  id: AccountIdentity;
  title: string;
  icon: string;
  order: number;
}

export interface ModulePageDefinition {
  id: string;
  identity: AccountIdentity;
  title: string;
  description: string;
  icon: string;
  order: number;
  primaryAction?: ActionDemoSchema;
  actions: ActionDemoSchema[];
  actionPresentations: ActionPresentationSchema[];
  views: TableViewSchema[];
}

const iconTokens: Readonly<Record<string, string>> = {
  account: "manage_accounts",
  admin_users: "admin_panel_settings",
  administrator: "admin_panel_settings",
  organization: "domain",
  organization_members: "groups",
  organization_profile: "apartment",
  organizations: "domain",
  person: "account_circle",
};

function iconFor(token: string): string {
  return iconTokens[token] ?? "extension";
}

export function buildAccountModulePages(
  catalog: UiCatalog | undefined,
): ModulePageDefinition[] {
  if (!catalog) return [];
  const actions = new Map(
    catalog.actions.map((action) => [action.operation_id, action]),
  );
  const views = new Map(
    catalog.table_views.map((view) => [view.view_id, view]),
  );
  return catalog.modules
    .map((module): ModulePageDefinition => {
      const primaryAction = module.primary_action
        ? actions.get(module.primary_action)
        : undefined;
      const moduleActions = module.actions.flatMap((operationId) => {
        const action = actions.get(operationId);
        return action ? [action] : [];
      });
      const moduleActionIds = new Set(
        moduleActions.map((action) => action.operation_id),
      );
      return {
        id: module.module_id,
        identity: module.identity.id,
        title: module.title,
        description: module.description,
        icon: iconFor(module.icon),
        order: module.order,
        primaryAction,
        actions: moduleActions,
        actionPresentations: module.action_presentations.filter(
          (presentation) => moduleActionIds.has(presentation.operation_id),
        ),
        views: module.views.flatMap((viewId) => {
          const view = views.get(viewId);
          return view ? [view] : [];
        }),
      };
    })
    .sort(
      (left, right) =>
        left.order - right.order || compareProductText(left.id, right.id),
    );
}

export function moduleView(
  page: ModulePageDefinition,
  viewId: string | undefined,
): TableViewSchema | undefined {
  const view =
    page.views.find((candidate) => candidate.view_id === viewId) ??
    page.views[0];
  if (!view) return undefined;
  const existingPresentations = new Set(
    view.action_presentations.map(
      (presentation) =>
        `${presentation.operation_id}\u0000${presentation.placement}`,
    ),
  );
  const modulePresentations = page.actionPresentations.filter(
    (presentation) =>
      !existingPresentations.has(
        `${presentation.operation_id}\u0000${presentation.placement}`,
      ),
  );
  return {
    ...view,
    actions: [
      ...new Set([
        ...view.actions,
        ...modulePresentations.map((presentation) => presentation.operation_id),
      ]),
    ],
    action_presentations: [
      ...view.action_presentations,
      ...modulePresentations,
    ],
  };
}

export function modulesForIdentity(
  pages: ModulePageDefinition[],
  identity: AccountIdentity | undefined,
): ModulePageDefinition[] {
  if (!identity) return [];
  return pages.filter((page) => page.identity === identity);
}

export function identityForModuleId(
  pages: ModulePageDefinition[],
  moduleId: string,
): AccountIdentity | undefined {
  return pages.find((module) => module.id === moduleId)?.identity;
}

export function visibleAccountIdentities(
  pages: ModulePageDefinition[],
  catalog?: UiCatalog,
): AccountIdentityDefinition[] {
  const visible = new Set(pages.map((page) => page.identity));
  const identities = new Map<string, AccountIdentityDefinition>();
  for (const module of catalog?.modules ?? []) {
    if (!visible.has(module.identity.id)) continue;
    identities.set(module.identity.id, {
      id: module.identity.id,
      title: module.identity.title,
      icon: iconFor(module.identity.icon),
      order: module.identity.order,
    });
  }
  return [...identities.values()].sort(
    (left, right) =>
      left.order - right.order || compareProductText(left.id, right.id),
  );
}

export function unassignedViews(
  catalog: UiCatalog | undefined,
): TableViewSchema[] {
  if (!catalog) return [];
  const assigned = new Set(catalog.modules.flatMap((module) => module.views));
  return catalog.table_views.filter((view) => !assigned.has(view.view_id));
}

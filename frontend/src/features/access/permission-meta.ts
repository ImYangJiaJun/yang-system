/**
 * 权限展示元数据：把权限目录条目 + UI 目录投影成「界面能直接画的」形状。
 *
 * 本文件只放**纯函数**（无 hook、无副作用），单测直接喂目录构造件即可。
 * hook 壳（`usePermissionMeta`）在 `workspace-api.ts`，这里不碰 React。
 *
 * 两条映射规则（对着 UI 目录的形状定，不要凭直觉改）：
 *
 * 1. **权限名取声明它的 Action**：`declared_by` 里形如 `module.operation` 的 id
 *    若在 `catalog.actions` 存在，用该 Action 的 `title`/`description`；
 * 2. **模块名按权限前缀找**：权限的模块前缀（如 `access.grants`、`account.users`）
 *    在 `catalog.modules` 按 `module_id` 找 `module.title`；命不中时按
 *    `identity.id` 找 `identity.title` 兜底；都命不中返回 `undefined`，
 *    由界面回退显示权限字符串本身。
 */

import type { UiCatalog } from "@/engine";
import type { PermissionCatalogEntry } from "./api";

/// 一条权限的展示元数据（权限工作台的行模型）。
export type PermissionMeta = {
  permission: string;
  /// 展示名：优先取声明它的 Action 的 title，回退权限字符串本身。
  title: string;
  /// 模块前缀（权限字符串去掉最后一段），分组与「模块内定位」都靠它。
  modulePrefix: string;
  /// 模块展示名；命不中时是 `undefined`（分组回退到 `modulePrefix`）。
  moduleTitle: string | undefined;
  description: string | undefined;
  declaredBy: string[];
  adminEquivalent: boolean;
  /// 管理员等价权限的警示文案（目录里没有则为 null）。
  reason: string | null;
};

/// 折叠面板的一组（按模块前缀分）。
export type PermissionMetaGroup = {
  title: string;
  permissions: PermissionMeta[];
};

function moduleTitleOf(
  modulePrefix: string,
  catalog: UiCatalog | undefined,
): string | undefined {
  const module = catalog?.modules.find(
    (candidate) => candidate.module_id === modulePrefix,
  );
  if (module) return module.title;
  // identity 兜底：权限前缀与身份 ID 相同时用身份标题（目录里模块_id 与权限前缀
  // 不同源，如 `account.user` 模块对应 `account.users.*` 权限时 exact 命中必然落空）。
  const byIdentity = catalog?.modules.find(
    (candidate) => candidate.identity.id === modulePrefix,
  );
  return byIdentity?.identity.title;
}

/// 第一条能对上的声明 Action：`declared_by` 里存在即命中（操作 id 本身必然是
/// `module.operation` 形状，存在性检查已经隐含了形状检查）。
function firstDeclaringAction(
  entry: PermissionCatalogEntry,
  catalog: UiCatalog | undefined,
) {
  for (const operationId of entry.declaredBy) {
    const action = catalog?.actions.find(
      (candidate) => candidate.operation_id === operationId,
    );
    if (action) return action;
  }
  return undefined;
}

/// 目录条目 → 展示元数据（按权限字符串为键，供 id → 元数据直查）。
export function buildPermissionMeta(
  entries: PermissionCatalogEntry[],
  catalog: UiCatalog | undefined,
): Map<string, PermissionMeta> {
  const meta = new Map<string, PermissionMeta>();
  for (const entry of entries) {
    const action = firstDeclaringAction(entry, catalog);
    const dot = entry.permission.lastIndexOf(".");
    const modulePrefix =
      dot > 0 ? entry.permission.slice(0, dot) : entry.permission;
    const title =
      action && action.title !== "" ? action.title : entry.permission;
    const description =
      action && action.description !== "" ? action.description : undefined;
    meta.set(entry.permission, {
      permission: entry.permission,
      title,
      modulePrefix,
      moduleTitle: moduleTitleOf(modulePrefix, catalog),
      description,
      declaredBy: entry.declaredBy,
      adminEquivalent: entry.adminEquivalent,
      reason: entry.reason,
    });
  }
  return meta;
}

/// 按模块前缀分组（折叠面板的组模型），组标题 = `moduleTitle ?? modulePrefix`，
/// 保持目录给出的顺序（第一次出现的顺序即组序）。
export function groupPermissionMeta(
  meta: PermissionMeta[],
): PermissionMetaGroup[] {
  const groups: PermissionMetaGroup[] = [];
  const byTitle = new Map<string, PermissionMetaGroup>();
  for (const item of meta) {
    const title = item.moduleTitle ?? item.modulePrefix;
    let group = byTitle.get(title);
    if (!group) {
      group = { title, permissions: [] };
      byTitle.set(title, group);
      groups.push(group);
    }
    group.permissions.push(item);
  }
  return groups;
}

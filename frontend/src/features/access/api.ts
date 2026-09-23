/**
 * 权限组管理面的数据访问层：`@/engine` 的 Action 调用**薄封装** + query key 工厂。
 *
 * 本文件不写 `fetch`、不新建第二个 HTTP 客户端：`invokeAction` 已经从 Action schema
 * 解析 method + path，并负责 token 刷新、信封解析与错误映射。与飞书控制台同一套形状
 * （那边是 `features/feishu/api.ts`），两个域之间不互相 import——公用的那一粒判据
 * 已下沉到 `engine/catalog/has-operation.ts`。
 *
 * 三个契约事实（都对着 `src/addon/access/groups/actions/*.rs` 核实过，不要凭直觉改）：
 *
 * 1. 列表与详情是 `GET`，详情的 `group_id` 在**路径**里（`/groups/{group_id}`）；
 *    其余七个写接口都是 `POST`，`group_id` 走**请求体**——路由模板里没有路径段；
 * 2. 加权限经服务端 `ensure_declared` fail-closed（目录里没有的权限会被 400 拒掉），
 *    移除则**不做**目录校验——所以「已不在目录里的孤儿条目」只能靠移除清理，
 *    界面上必须给它们留一条可点的退路；
 * 3. 内置全权组（`group_key = system_admin`）的权限由权限目录实时计算，
 *    条目表里没有可增删的授权事实：接口以 `effective_all: true` 显式表达。
 */

import { useQuery, type UseQueryResult } from "@tanstack/react-query";
import { useMemo } from "react";

import {
  hasOperation,
  invokeAction,
  useSessionCredentials,
  useUiCatalog,
} from "@/engine";
import type {
  ActionDemoSchema,
  InvocationResult,
  SessionContext,
  UiCatalog,
} from "@/engine";

/// 权限组管理面要用的九个 Action。
///
/// **每一粒都必须是服务端真实注册过的 `operation_id`**（逐个对着
/// `src/addon/access/groups/actions/*.rs` 的 `action_name!(...)` 核过，模块名是
/// `access.groups`）。写一个不存在的 id 的后果不是报错而是**静默**：`hasOperation`
/// 恒为 false，于是整个写侧永远不渲染——界面上看不出任何异常。
export const GROUP_OPERATION_IDS = {
  list: "access.groups.list_groups",
  detail: "access.groups.get_group",
  create: "access.groups.create_group",
  update: "access.groups.update_group",
  remove: "access.groups.delete_group",
  addItem: "access.groups.add_group_item",
  removeItem: "access.groups.remove_group_item",
  addMember: "access.groups.add_group_member",
  removeMember: "access.groups.remove_group_member",
} as const;

/* ------------------------------- 权限门控 -------------------------------- */

/// 能否看列表与详情（侧边栏入口与页面正文的渲染条件）。
export function canReadGroups(catalog: UiCatalog | undefined): boolean {
  return hasOperation(catalog, GROUP_OPERATION_IDS.list);
}

/// 能否改（新建/改名/删除/加删条目/加删成员是否**渲染**，不是禁用）。
///
/// 判据取 `create_group` 这一粒：九个接口的写侧都声明 `access.groups.write`，
/// 目录按身份投影之后，有它就意味着这一整套写接口都在。
export function canManageGroups(catalog: UiCatalog | undefined): boolean {
  return hasOperation(catalog, GROUP_OPERATION_IDS.create);
}

/* --------------------------------- 形状 ---------------------------------- */

/// 列表里的一个组（对齐 `list_groups.rs` 的 `GroupSummaryView`）。
///
/// `orphanItemCount` 只报告不清理：它数的是「组里有、但当前权限目录里已不存在」
/// 的条目（设计 §8.4）。
export type GroupSummary = {
  id: number;
  groupKey: string;
  title: string;
  description: string | null;
  memberCount: number;
  itemCount: number;
  isBuiltin: boolean;
  orphanItemCount: number;
};

/// 一条组条目。`isOrphan` 为真是「这条权限已不在权限目录里」——**只标记不清理**，
/// 界面上要把它和正常条目分开画，并给出移除的出路。
export type GroupItemEntry = {
  permission: string;
  isOrphan: boolean;
};

/// 组详情（对齐 `get_group.rs` 的 `GetGroupResult`）。
///
/// `effectiveAll` 为真表示内置全权组：它的权限由权限目录实时计算，
/// `items` 里没有可展示的授权事实（设计 §15 第 3 条），页面必须特判。
export type GroupDetail = {
  id: number;
  groupKey: string;
  title: string;
  description: string | null;
  effectiveAll: boolean;
  items: GroupItemEntry[];
  members: number[];
};

/* ------------------------------- 响应解析 -------------------------------- */

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

function asNumber(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function asString(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

/// 可空字符串：空串与缺失一律折成 `null`，避免界面出现「有值但不可见」的空白格。
function asNullableString(value: unknown): string | null {
  return typeof value === "string" && value !== "" ? value : null;
}

/// 一条列表行。**没有 `id` 的行无法定位**（选中、详情、写接口都用它），
/// 收下它只会得到一个点不动的选项，所以在这一层丢掉。
function parseGroupSummary(raw: Record<string, unknown>): GroupSummary | null {
  const id = typeof raw.id === "number" ? raw.id : null;
  if (id === null) return null;
  return {
    id,
    groupKey: asString(raw.group_key),
    title: asString(raw.title),
    description: asNullableString(raw.description),
    memberCount: asNumber(raw.member_count, 0),
    itemCount: asNumber(raw.item_count, 0),
    isBuiltin: raw.is_builtin === true,
    orphanItemCount: asNumber(raw.orphan_item_count, 0),
  };
}

function parseGroupItem(raw: Record<string, unknown>): GroupItemEntry | null {
  const permission = asString(raw.permission);
  // 没有权限字符串的条目定位不了，也就画不出「移除哪一条」
  return permission === ""
    ? null
    : { permission, isOrphan: raw.is_orphan === true };
}

function parseGroupDetail(data: unknown, groupId: number): GroupDetail {
  const record = asRecord(data);
  const rawItems = record?.items;
  const rawMembers = record?.members;
  return {
    // `id` 缺了就按请求时的 `group_id` 回填：它是这次响应唯一可能的身份
    id: asNumber(record?.id, groupId),
    groupKey: asString(record?.group_key),
    title: asString(record?.title),
    description: asNullableString(record?.description),
    // 缺 `effective_all` 时按**不是**内置组处理：这个布尔决定要不要渲染矩阵，
    // 猜成 true 会让一个普通组凭空失去全部条目
    effectiveAll: record?.effective_all === true,
    items: Array.isArray(rawItems)
      ? rawItems
          .map((raw) => asRecord(raw))
          .filter((raw): raw is Record<string, unknown> => raw !== undefined)
          .map(parseGroupItem)
          .filter((item): item is GroupItemEntry => item !== null)
      : [],
    members: Array.isArray(rawMembers)
      ? rawMembers.filter(
          (member): member is number =>
            typeof member === "number" && Number.isFinite(member),
        )
      : [],
  };
}

/* ------------------------------- Action 调用 ------------------------------ */

/// 调用 Action 需要的两样东西：目录（含 method/path）与会话凭据。
export type AccessInvokeDeps = {
  catalog: UiCatalog | undefined;
  session: SessionContext;
};

/// 目录里找不到 operation_id 时抛明确错误，而不是静默发一个空请求。
export function requireGroupAction(
  catalog: UiCatalog | undefined,
  operationId: string,
): ActionDemoSchema {
  const action = catalog?.actions.find(
    (candidate) => candidate.operation_id === operationId,
  );
  if (!action) {
    throw new Error(
      `UI 目录里找不到 Action「${operationId}」：当前身份可能没有该权限，或目录尚未加载完成`,
    );
  }
  return action;
}

async function invokeGroupAction(
  deps: AccessInvokeDeps,
  operationId: string,
  values: Record<string, unknown>,
  signal?: AbortSignal,
): Promise<InvocationResult> {
  return invokeAction(
    requireGroupAction(deps.catalog, operationId),
    values,
    deps.session,
    signal,
  );
}

/* ------------------------------- 读 ------------------------------- */

export async function listGroups(
  deps: AccessInvokeDeps,
  signal?: AbortSignal,
): Promise<GroupSummary[]> {
  const result = await invokeGroupAction(
    deps,
    GROUP_OPERATION_IDS.list,
    {},
    signal,
  );
  const raw = asRecord(result.data)?.groups;
  if (!Array.isArray(raw)) return [];
  return raw
    .map((item) => asRecord(item))
    .filter((item): item is Record<string, unknown> => item !== undefined)
    .map(parseGroupSummary)
    .filter((item): item is GroupSummary => item !== null);
}

/// 读取一个组的详情。`group_id` 走**路径**（服务端把它声明成 path 参数）。
export async function getGroup(
  groupId: number,
  deps: AccessInvokeDeps,
  signal?: AbortSignal,
): Promise<GroupDetail> {
  const result = await invokeGroupAction(
    deps,
    GROUP_OPERATION_IDS.detail,
    { group_id: groupId },
    signal,
  );
  return parseGroupDetail(result.data, groupId);
}

/* ------------------------------- 写 ------------------------------- */

export type CreateGroupInput = {
  groupKey: string;
  title: string;
  /// 省略即「没有描述」：`deny_unknown_fields` 的请求体里**不许出现多余的键**，
  /// 这里用 `undefined` 表达省略（引擎不会把它写进 body）。
  description?: string;
};

/// 建组。回执里带新组的主键，调用方据此把选中态挪过去。
export async function createGroup(
  input: CreateGroupInput,
  deps: AccessInvokeDeps,
  signal?: AbortSignal,
): Promise<number> {
  const result = await invokeGroupAction(
    deps,
    GROUP_OPERATION_IDS.create,
    {
      group_key: input.groupKey,
      title: input.title,
      description: input.description,
    },
    signal,
  );
  return asNumber(asRecord(result.data)?.id, 0);
}

export type UpdateGroupInput = {
  groupId: number;
  title: string;
  description?: string;
};

/// 改展示名与描述。`group_key` 不可改（服务端不接受这个键，内置组还会直接拒）。
export async function updateGroup(
  input: UpdateGroupInput,
  deps: AccessInvokeDeps,
  signal?: AbortSignal,
): Promise<void> {
  await invokeGroupAction(
    deps,
    GROUP_OPERATION_IDS.update,
    {
      group_id: input.groupId,
      title: input.title,
      description: input.description,
    },
    signal,
  );
}

/// 删组。组内仍有成员时服务端回 409（应用层前置检查 + 外键 RESTRICT 兜底）。
export async function deleteGroup(
  groupId: number,
  deps: AccessInvokeDeps,
  signal?: AbortSignal,
): Promise<void> {
  await invokeGroupAction(
    deps,
    GROUP_OPERATION_IDS.remove,
    { group_id: groupId },
    signal,
  );
}

/// 加一条权限（幂等）。目录里没有的权限会被服务端 400 拒掉，错误原文直接上抛。
export async function addGroupItem(
  groupId: number,
  permission: string,
  deps: AccessInvokeDeps,
  signal?: AbortSignal,
): Promise<void> {
  await invokeGroupAction(
    deps,
    GROUP_OPERATION_IDS.addItem,
    { group_id: groupId, permission },
    signal,
  );
}

/// 移除一条权限（幂等）。**不做目录校验**：孤儿条目走的正是这条路。
export async function removeGroupItem(
  groupId: number,
  permission: string,
  deps: AccessInvokeDeps,
  signal?: AbortSignal,
): Promise<void> {
  await invokeGroupAction(
    deps,
    GROUP_OPERATION_IDS.removeItem,
    { group_id: groupId, permission },
    signal,
  );
}

export async function addGroupMember(
  groupId: number,
  userId: number,
  deps: AccessInvokeDeps,
  signal?: AbortSignal,
): Promise<void> {
  await invokeGroupAction(
    deps,
    GROUP_OPERATION_IDS.addMember,
    { group_id: groupId, user_id: userId },
    signal,
  );
}

export async function removeGroupMember(
  groupId: number,
  userId: number,
  deps: AccessInvokeDeps,
  signal?: AbortSignal,
): Promise<void> {
  await invokeGroupAction(
    deps,
    GROUP_OPERATION_IDS.removeMember,
    { group_id: groupId, user_id: userId },
    signal,
  );
}

/* -------------------------------- query key ------------------------------- */

/// 顶层键：会话边界（`session-reset` 的 `queryClient.clear()`）按前缀清空查询缓存，
/// 这里以 `"access"` 作为本域的唯一顶层段。
export const ACCESS_QUERY_ROOT = "access";

/// query key 工厂。
///
/// 详情键是列表键的**子键**（`["access","groups",id]` 以 `["access","groups"]`
/// 为前缀），所以一次前缀失效就能同时作废列表与详情——写接口只回计数与幂等标记，
/// 最新事实只能靠回读，两处少失效一处就会留下对不上的屏。
export const accessGroupQueryKeys = {
  root: () => [ACCESS_QUERY_ROOT] as const,
  groups: () => [ACCESS_QUERY_ROOT, "groups"] as const,
  group: (groupId: number) => [ACCESS_QUERY_ROOT, "groups", groupId] as const,
};

/* --------------------------------- hooks --------------------------------- */

/// 目录里没有读权限时不发请求（省一次注定 403 的往返）。
export function useGroupList(): UseQueryResult<GroupSummary[]> {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;
  return useQuery({
    enabled: canReadGroups(catalogData),
    queryKey: accessGroupQueryKeys.groups(),
    queryFn: ({ signal }) =>
      listGroups({ catalog: catalogData, session }, signal),
    staleTime: 10_000,
  });
}

/// 选中组的详情。
///
/// **刻意不用 `keepPreviousData`**：切换组时保留上一组的数据，会让 B 组的名字下面
/// 先亮着 A 组的条目与成员——那正是「按主键定位」要消灭的错配（同一粒权限被勾在
/// 错误的组上，点一下就写错事实）。宁可闪一格骨架。
export function useGroupDetail(
  groupId: number | null,
): UseQueryResult<GroupDetail> {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;
  return useQuery({
    enabled: canReadGroups(catalogData) && groupId !== null,
    queryKey: accessGroupQueryKeys.group(groupId ?? 0),
    queryFn: ({ signal }) => {
      // `enabled` 已经挡住 null；这里再兜一次是为了让 `groupId` 收窄成 number
      if (groupId === null) {
        throw new Error("未选中权限组");
      }
      return getGroup(groupId, { catalog: catalogData, session }, signal);
    },
    staleTime: 5_000,
  });
}

/// 页面用的一组「已绑定目录与会话」的写操作入口。
///
/// 变更函数本身**不缓存失效**——写接口只回计数或幂等标记，页面提交后必须回读，
/// 所以失效由调用方（页面）统一挂在 `accessGroupQueryKeys.root()` 上。
export type GroupActions = {
  canRead: boolean;
  canManage: boolean;
  createGroup: (input: CreateGroupInput) => Promise<number>;
  updateGroup: (input: UpdateGroupInput) => Promise<void>;
  deleteGroup: (groupId: number) => Promise<void>;
  addItem: (groupId: number, permission: string) => Promise<void>;
  removeItem: (groupId: number, permission: string) => Promise<void>;
  addMember: (groupId: number, userId: number) => Promise<void>;
  removeMember: (groupId: number, userId: number) => Promise<void>;
};

export function useGroupActions(): GroupActions {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;

  const deps = useMemo<AccessInvokeDeps>(
    () => ({ catalog: catalogData, session }),
    [catalogData, session],
  );

  return useMemo(
    () => ({
      canRead: canReadGroups(catalogData),
      canManage: canManageGroups(catalogData),
      createGroup: (input: CreateGroupInput) => createGroup(input, deps),
      updateGroup: (input: UpdateGroupInput) => updateGroup(input, deps),
      deleteGroup: (groupId: number) => deleteGroup(groupId, deps),
      addItem: (groupId: number, permission: string) =>
        addGroupItem(groupId, permission, deps),
      removeItem: (groupId: number, permission: string) =>
        removeGroupItem(groupId, permission, deps),
      addMember: (groupId: number, userId: number) =>
        addGroupMember(groupId, userId, deps),
      removeMember: (groupId: number, userId: number) =>
        removeGroupMember(groupId, userId, deps),
    }),
    [catalogData, deps],
  );
}

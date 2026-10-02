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
  useSessionController,
  useUiCatalog,
} from "@/engine";
import { StepUpRequiredError } from "@/engine/http/errors";
import type {
  ActionDemoSchema,
  InvocationResult,
  SessionContext,
  UiCatalog,
} from "@/engine";

/// 权限组管理面要用的十个 operation：九个组 Action + 一个权限目录读接口。
///
/// **每一粒都必须是服务端真实注册过的 `operation_id`**（逐个对着
/// `src/addon/access/groups/actions/*.rs` 的 `action_name!(...)` 核过，模块名是
/// `access.groups`；`listPermissions` 除外——它注册在 `src/addon/access/grants/actions/
/// list_permissions.rs`，模块名是 `access.grants`，权限位 `access.grants.read`）。
/// 写一个不存在的 id 的后果不是报错而是**静默**：`hasOperation`
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
  /// 权限目录（组条目候选的来源）：只读，不带 Step-up 语义。
  listPermissions: "access.grants.list_permissions",
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

/// 权限目录里的一条权限（对齐 `grants/actions/list_permissions.rs` 的 `PermissionEntry`）。
///
/// `adminEquivalent` 为真表示「管理员等价权限」（G2）：授予它等于交出一部分系统
/// 管理能力，界面上必须把危害面标出来（设计 `sensitive_permissions.rs`）。
export type PermissionCatalogEntry = {
  permission: string;
  declaredBy: string[];
  adminEquivalent: boolean;
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

/// 一条目录条目。没有权限字符串的条目没有候选意义（选了也发不出去），直接丢掉。
function parsePermissionEntry(
  raw: Record<string, unknown>,
): PermissionCatalogEntry | null {
  const permission = asString(raw.permission);
  if (permission === "") return null;
  return {
    permission,
    declaredBy: Array.isArray(raw.declared_by)
      ? raw.declared_by.filter(
          (item): item is string => typeof item === "string",
        )
      : [],
    adminEquivalent: raw.admin_equivalent === true,
  };
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
  stepUpProof?: string,
): Promise<InvocationResult> {
  return invokeAction(
    requireGroupAction(deps.catalog, operationId),
    values,
    deps.session,
    signal,
    { stepUpProof },
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

/// 读权限目录：组条目候选的来源。**只读**，不需要 Step-up。
export async function listPermissions(
  deps: AccessInvokeDeps,
  signal?: AbortSignal,
): Promise<PermissionCatalogEntry[]> {
  const result = await invokeGroupAction(
    deps,
    GROUP_OPERATION_IDS.listPermissions,
    {},
    signal,
  );
  const raw = asRecord(result.data)?.permissions;
  if (!Array.isArray(raw)) return [];
  return raw
    .map((item) => asRecord(item))
    .filter((item): item is Record<string, unknown> => item !== undefined)
    .map(parsePermissionEntry)
    .filter((item): item is PermissionCatalogEntry => item !== null);
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
  stepUpProof?: string,
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
    stepUpProof,
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
  stepUpProof?: string,
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
    stepUpProof,
  );
}

/// 删组。组内仍有成员时服务端回 409（应用层前置检查 + 外键 RESTRICT 兜底）。
export async function deleteGroup(
  groupId: number,
  deps: AccessInvokeDeps,
  signal?: AbortSignal,
  stepUpProof?: string,
): Promise<void> {
  await invokeGroupAction(
    deps,
    GROUP_OPERATION_IDS.remove,
    { group_id: groupId },
    signal,
    stepUpProof,
  );
}

/// 加一条权限（幂等）。目录里没有的权限会被服务端 400 拒掉，错误原文直接上抛。
export async function addGroupItem(
  groupId: number,
  permission: string,
  deps: AccessInvokeDeps,
  signal?: AbortSignal,
  stepUpProof?: string,
): Promise<void> {
  await invokeGroupAction(
    deps,
    GROUP_OPERATION_IDS.addItem,
    { group_id: groupId, permission },
    signal,
    stepUpProof,
  );
}

/// 移除一条权限（幂等）。**不做目录校验**：孤儿条目走的正是这条路。
export async function removeGroupItem(
  groupId: number,
  permission: string,
  deps: AccessInvokeDeps,
  signal?: AbortSignal,
  stepUpProof?: string,
): Promise<void> {
  await invokeGroupAction(
    deps,
    GROUP_OPERATION_IDS.removeItem,
    { group_id: groupId, permission },
    signal,
    stepUpProof,
  );
}

export async function addGroupMember(
  groupId: number,
  userId: number,
  deps: AccessInvokeDeps,
  signal?: AbortSignal,
  stepUpProof?: string,
): Promise<void> {
  await invokeGroupAction(
    deps,
    GROUP_OPERATION_IDS.addMember,
    { group_id: groupId, user_id: userId },
    signal,
    stepUpProof,
  );
}

export async function removeGroupMember(
  groupId: number,
  userId: number,
  deps: AccessInvokeDeps,
  signal?: AbortSignal,
  stepUpProof?: string,
): Promise<void> {
  await invokeGroupAction(
    deps,
    GROUP_OPERATION_IDS.removeMember,
    { group_id: groupId, user_id: userId },
    signal,
    stepUpProof,
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
  /// 权限目录挂在同一前缀下：页面「写完全部回读」与 `session-refreshed`
  /// 事件做一次前缀失效时，它跟着一起作废重拉。
  permissions: () => [ACCESS_QUERY_ROOT, "permissions"] as const,
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

/// 权限目录（组条目候选的来源）。
///
/// 只为「加权限」表单服务，所以 `enabled` 同时看两粒独立的权限位：能管理组
/// （`create_group` 在目录里）才需要候选；能看到目录（`list_permissions` 在目录里）
/// 才拉得到候选。缺后者时**不发注定 403 的往返**，页面改为说明为什么加不了。
export function usePermissionCatalog(): UseQueryResult<
  PermissionCatalogEntry[]
> {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;
  return useQuery({
    enabled:
      canManageGroups(catalogData) &&
      hasOperation(catalogData, GROUP_OPERATION_IDS.listPermissions),
    queryKey: accessGroupQueryKeys.permissions(),
    queryFn: ({ signal }) =>
      listPermissions({ catalog: catalogData, session }, signal),
    staleTime: 60_000,
  });
}

/// 页面用的一组「已绑定目录与会话」的写操作入口。
///
/// 变更函数本身**不缓存失效**——写接口只回计数或幂等标记，页面提交后必须回读，
/// 所以失效由调用方（页面）统一挂在 `accessGroupQueryKeys.root()` 上。
///
/// 每个写操作内置 Step-up 重试：收到 428 challenge 时自动弹重认证对话框并重放请求；
/// 用户取消重认证时函数静默返回（`createGroup` 返回 0，其余无操作）。
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
  const controller = useSessionController();

  const deps = useMemo<AccessInvokeDeps>(
    () => ({ catalog: catalogData, session }),
    [catalogData, session],
  );

  /// Step-up 透明重试：首次请求遇 428 时弹重认证对话框换 proof 后重放。
  const request = useMemo(
    () =>
      <T>(
        fn: (proof: string | undefined) => Promise<T>,
      ): Promise<T | undefined> =>
        runProtected(fn, controller),
    [controller],
  );

  return useMemo(
    () => ({
      canRead: canReadGroups(catalogData),
      canManage: canManageGroups(catalogData),
      createGroup: async (input: CreateGroupInput) => {
        const result = await request((proof) =>
          createGroup(input, deps, undefined, proof),
        );
        return result ?? 0;
      },
      updateGroup: async (input: UpdateGroupInput) => {
        await request((proof) => updateGroup(input, deps, undefined, proof));
      },
      deleteGroup: async (groupId: number) => {
        await request((proof) => deleteGroup(groupId, deps, undefined, proof));
      },
      addItem: async (groupId: number, permission: string) => {
        await request((proof) =>
          addGroupItem(groupId, permission, deps, undefined, proof),
        );
      },
      removeItem: async (groupId: number, permission: string) => {
        await request((proof) =>
          removeGroupItem(groupId, permission, deps, undefined, proof),
        );
      },
      addMember: async (groupId: number, userId: number) => {
        await request((proof) =>
          addGroupMember(groupId, userId, deps, undefined, proof),
        );
      },
      removeMember: async (groupId: number, userId: number) => {
        await request((proof) =>
          removeGroupMember(groupId, userId, deps, undefined, proof),
        );
      },
    }),
    [catalogData, deps, request],
  );
}

/// Step-up 透明重试：首次请求遇 428 时通过 controller 弹对话框换 proof 后重放。
async function runProtected<T>(
  request: (proof: string | undefined) => Promise<T>,
  controller: ReturnType<typeof useSessionController>,
): Promise<T | undefined> {
  try {
    return await request(undefined);
  } catch (cause) {
    if (!(cause instanceof StepUpRequiredError)) throw cause;
    const proof = await controller.requestStepUpProof(cause.challenge);
    if (!proof) return undefined; // 用户取消
    return await request(proof);
  }
}

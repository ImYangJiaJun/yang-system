/**
 * 权限工作台的共享数据层：用户目录/用户查找、用户直授列表、权限持有者。
 *
 * 与 `api.ts` 同一套纪律：不写 `fetch`、不新建 HTTP 客户端，Action 一律经
 * `invokeAction` 走 UI 目录（method/path/参数落位都由目录里的 Action 声明决定）；
 * 响应形状按钉住的契约解析，值缺失时**丢行不猜值**（fail-closed）。
 *
 * 三个契约事实（`lookup` 与 `list_holders` 按钉住的契约实现，后端并行开发中；
 * `list_user_grants` 对着 `src/addon/access/grants/actions/list_user_grants.rs`
 * 核实过，`user_id` 在**路径**里）：
 *
 * 1. `account.users.lookup`：GET，`q`/`page`/`page_size` 都是 query 参数
 *    （目录里声明了什么来源，引擎就放去哪里），响应 `{users:[{id,username,email,status}]}`；
 * 2. `access.grants.list_user_grants`：GET，`user_id` 走路径，响应
 *    `{user_id, grants:[{id,permission,granted_by,occurred_at,expires_at,expired}]}`，
 *    含过期行（审计视图），`expires_at` 为 null 表示永久；
 * 3. `access.grants.list_holders`：GET，响应
 *    `{direct:[{user_id,granted_by,occurred_at,expires_at,expired}], groups:[{id,group_key,title,member_count}]}`。
 */

import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";

import {
  hasOperation,
  invokeAction,
  useSessionCredentials,
  useSessionController,
  useUiCatalog,
} from "@/engine";
import type { InvocationResult } from "@/engine";

import {
  ACCESS_QUERY_ROOT,
  asNullableString,
  asNumber,
  asRecord,
  asString,
  requireGroupAction,
  runProtected,
  usePermissionCatalog,
  type AccessInvokeDeps,
} from "./api";
import { buildPermissionMeta, type PermissionMeta } from "./permission-meta";

/// 工作台要用的五个 operation（`access.grants.list_user_grants` 已核实存在；
/// `lookup` 与 `list_holders` 是钉住的契约，后端并行开发中——写错 id 的后果是
/// 静默：`hasOperation` 恒为 false，界面整块不渲染；grant/revoke 与
/// `grants/actions/grant_permission.rs` / `revoke_permission.rs` 的注册核对过）。
export const WORKSPACE_OPERATION_IDS = {
  lookup: "account.users.lookup",
  listUserGrants: "access.grants.list_user_grants",
  listHolders: "access.grants.list_holders",
  grantPermission: "access.grants.grant_permission",
  revokePermission: "access.grants.revoke_permission",
} as const;

/// 全量拉取的每页大小：用户量小，50 一页最多两三页；后端 page_size 上限 100。
const LOOKUP_PAGE_SIZE = 50;
/// 翻页硬上限：后端若忽略分页会恒返回同一页，封顶防止死循环（5000 用户才触顶）。
const MAX_USER_PAGES = 100;

/* --------------------------------- 形状 ---------------------------------- */

/// 用户查找结果里的一行（对齐钉住的 `account.users.lookup` 契约）。
export type UserLookupEntry = {
  id: number;
  username: string;
  email: string | null;
  /// `active` / `disabled` / `deleted`（停用与已删除要置灰并标注，不可作为授予对象）。
  status: string;
};

/// 一条直授权限（审计视图，含过期行）。
export type UserGrant = {
  id: number;
  permission: string;
  grantedBy: number;
  occurredAt: number;
  /// Unix 秒；null = 永久有效。
  expiresAt: number | null;
  /// 派生标记：是否已过期（过期后权限在解析侧失效，行保留做审计）。
  expired: boolean;
};

export type UserGrantsResult = {
  userId: number;
  grants: UserGrant[];
};

/// 直授持有者（`list_holders` 的 direct 数组元素，没有 id 字段，user_id 是身份）。
export type HolderEntry = {
  user_id: number;
  granted_by: number;
  occurred_at: number;
  expires_at: number | null;
  expired: boolean;
};

/// 通过权限组间接持有的来源组。
export type GroupHolder = {
  id: number;
  group_key: string;
  title: string;
  member_count: number;
};

export type HoldersResult = {
  direct: HolderEntry[];
  groups: GroupHolder[];
};

/* ------------------------------- 响应解析 -------------------------------- */

function asNullableNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/// 没有 id 的行无法定位（选中、展示、撤销都靠它），直接丢掉。
function parseUserLookupEntry(
  raw: Record<string, unknown>,
): UserLookupEntry | null {
  const id = asNumber(raw.id, 0);
  if (id <= 0) return null;
  return {
    id,
    username: asString(raw.username),
    email: asNullableString(raw.email),
    // 缺 status 时按 active 展示：只有非 active 才需要标注，猜成停用会误伤可用行
    status: asString(raw.status, "active"),
  };
}

/// 一条直授。没有 id 或权限字符串的行没有展示意义，丢掉。
function parseUserGrant(raw: Record<string, unknown>): UserGrant | null {
  const id = asNumber(raw.id, 0);
  const permission = asString(raw.permission);
  if (id <= 0 || permission === "") return null;
  return {
    id,
    permission,
    grantedBy: asNumber(raw.granted_by, 0),
    occurredAt: asNumber(raw.occurred_at, 0),
    expiresAt: asNullableNumber(raw.expires_at),
    expired: raw.expired === true,
  };
}

function parseUserGrantsResult(
  data: unknown,
  userId: number,
): UserGrantsResult {
  const record = asRecord(data);
  const rawGrants = record?.grants;
  return {
    // `user_id` 缺了就按请求时的目标用户回填：它是这次响应唯一可能的身份
    userId: asNumber(record?.user_id, userId),
    grants: Array.isArray(rawGrants)
      ? rawGrants
          .map((raw) => asRecord(raw))
          .filter((raw): raw is Record<string, unknown> => raw !== undefined)
          .map(parseUserGrant)
          .filter((grant): grant is UserGrant => grant !== null)
      : [],
  };
}

/// 直授持有者行：`user_id` 是唯一身份，缺失即丢。
function parseHolderEntry(raw: Record<string, unknown>): HolderEntry | null {
  const user_id = asNumber(raw.user_id, 0);
  if (user_id <= 0) return null;
  return {
    user_id,
    granted_by: asNumber(raw.granted_by, 0),
    occurred_at: asNumber(raw.occurred_at, 0),
    expires_at: asNullableNumber(raw.expires_at),
    expired: raw.expired === true,
  };
}

function parseGroupHolder(raw: Record<string, unknown>): GroupHolder | null {
  const id = asNumber(raw.id, 0);
  const group_key = asString(raw.group_key);
  if (id <= 0 || group_key === "") return null;
  return {
    id,
    group_key,
    title: asString(raw.title),
    member_count: asNumber(raw.member_count, 0),
  };
}

function parseHoldersResult(data: unknown): HoldersResult {
  const record = asRecord(data);
  const rawDirect = record?.direct;
  const rawGroups = record?.groups;
  return {
    direct: Array.isArray(rawDirect)
      ? rawDirect
          .map((raw) => asRecord(raw))
          .filter((raw): raw is Record<string, unknown> => raw !== undefined)
          .map(parseHolderEntry)
          .filter((entry): entry is HolderEntry => entry !== null)
      : [],
    groups: Array.isArray(rawGroups)
      ? rawGroups
          .map((raw) => asRecord(raw))
          .filter((raw): raw is Record<string, unknown> => raw !== undefined)
          .map(parseGroupHolder)
          .filter((group): group is GroupHolder => group !== null)
      : [],
  };
}

/* ------------------------------- Action 调用 ------------------------------ */

async function invokeWorkspaceAction(
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

/// 用户查找（搜索下拉的数据源）。`q`/`page`/`page_size` 的落位由目录里的
/// Action 声明决定，引擎按契约放好（query/body），这里只传值。
async function listUserLookupPage(
  deps: AccessInvokeDeps,
  q: string,
  page: number,
  signal?: AbortSignal,
): Promise<UserLookupEntry[]> {
  const result = await invokeWorkspaceAction(
    deps,
    WORKSPACE_OPERATION_IDS.lookup,
    { q, page, page_size: LOOKUP_PAGE_SIZE },
    signal,
  );
  const raw = asRecord(result.data)?.users;
  if (!Array.isArray(raw)) return [];
  return raw
    .map((item) => asRecord(item))
    .filter((item): item is Record<string, unknown> => item !== undefined)
    .map(parseUserLookupEntry)
    .filter((entry): entry is UserLookupEntry => entry !== null);
}

/// 用户查找第一页（搜索下拉用）。
export async function listUserLookup(
  deps: AccessInvokeDeps,
  q: string,
  signal?: AbortSignal,
): Promise<UserLookupEntry[]> {
  return listUserLookupPage(deps, q, 1, signal);
}

/// 循环翻页拉全量用户（q 为空，page_size=50，用户量小）。
export async function fetchAllUserPages(
  deps: AccessInvokeDeps,
  signal?: AbortSignal,
): Promise<UserLookupEntry[]> {
  const users: UserLookupEntry[] = [];
  for (let page = 1; page <= MAX_USER_PAGES; page += 1) {
    const batch = await listUserLookupPage(deps, "", page, signal);
    users.push(...batch);
    // 不足一页 = 拉完了；正好满页时多打一页空页来收尾（比猜总数可靠）
    if (batch.length < LOOKUP_PAGE_SIZE) break;
  }
  return users;
}

/// 目标用户的全部直授权限（审计视图，含过期行）。
export async function listUserGrants(
  userId: number,
  deps: AccessInvokeDeps,
  signal?: AbortSignal,
): Promise<UserGrantsResult> {
  const result = await invokeWorkspaceAction(
    deps,
    WORKSPACE_OPERATION_IDS.listUserGrants,
    // `user_id` 走路径：路由模板里有路径段，引擎按目录声明替换
    { user_id: userId },
    signal,
  );
  return parseUserGrantsResult(result.data, userId);
}

/// 一条权限的全部持有者：直授用户 + 间接持有的权限组。
export async function listHolders(
  permission: string,
  deps: AccessInvokeDeps,
  signal?: AbortSignal,
): Promise<HoldersResult> {
  const result = await invokeWorkspaceAction(
    deps,
    WORKSPACE_OPERATION_IDS.listHolders,
    { permission },
    signal,
  );
  return parseHoldersResult(result.data);
}

/* ------------------------------- 写 ------------------------------- */

export type GrantPermissionInput = {
  userId: number;
  permission: string;
  /// Unix 秒；null = 永久有效（服务端 `expires_at: Option<i64>`）。
  expiresAt: number | null;
};

/// 授予（或原地续期）一条直授权限。幂等：同一对 (user_id, permission) 已存在时
/// 服务端在原地更新过期时间（`grant_permission.rs` 的 upsert 语义）。
export async function grantPermission(
  input: GrantPermissionInput,
  deps: AccessInvokeDeps,
  signal?: AbortSignal,
  stepUpProof?: string,
): Promise<void> {
  await invokeWorkspaceAction(
    deps,
    WORKSPACE_OPERATION_IDS.grantPermission,
    {
      user_id: input.userId,
      permission: input.permission,
      // 永久 = 省略该键：请求体带显式 null 会被服务端当成「值非法」拒掉，
      // 与 `api.ts` 的 `description: undefined` 同一纪律（引擎不写 undefined 键）。
      expires_at: input.expiresAt ?? undefined,
    },
    signal,
    stepUpProof,
  );
}

/// 撤销一条直授权限（幂等）。过期行也能撤销——审计行一并清掉。
export async function revokePermission(
  userId: number,
  permission: string,
  deps: AccessInvokeDeps,
  signal?: AbortSignal,
  stepUpProof?: string,
): Promise<void> {
  await invokeWorkspaceAction(
    deps,
    WORKSPACE_OPERATION_IDS.revokePermission,
    { user_id: userId, permission },
    signal,
    stepUpProof,
  );
}

/* -------------------------------- query key ------------------------------- */

/// 工作台查询键：挂在 `access` 前缀下，权限组页的 `accessGroupQueryKeys.root()`
/// 一次前缀失效就把工作台查询一起作废（「写完全部回读」同一纪律）。
export const accessWorkspaceQueryKeys = {
  /// 全量用户目录：id → 用户名映射的唯一来源，staleTime 长（10 分钟）。
  userDirectory: () => [ACCESS_QUERY_ROOT, "user-directory"] as const,
  /// 搜索下拉：键带查询词，不同词各自缓存。
  userLookup: (q: string) => [ACCESS_QUERY_ROOT, "user-lookup", q] as const,
  userGrants: (userId: number) =>
    [ACCESS_QUERY_ROOT, "user-grants", userId] as const,
  holders: (permission: string) =>
    [ACCESS_QUERY_ROOT, "holders", permission] as const,
};

/* --------------------------------- hooks --------------------------------- */

/// 全量用户目录：循环翻页拉全量用户，构建 `byId` 供「id → 名字」映射。
/// 目录里没有 lookup 操作时不发请求（省一次注定 403 的往返）。
export function useUserDirectory(): {
  users: UserLookupEntry[];
  byId: Map<number, { username: string; email: string | null; status: string }>;
  isPending: boolean;
  isError: boolean;
  error: unknown;
} {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;
  const query = useQuery({
    enabled: hasOperation(catalogData, WORKSPACE_OPERATION_IDS.lookup),
    queryKey: accessWorkspaceQueryKeys.userDirectory(),
    queryFn: ({ signal }) =>
      fetchAllUserPages({ catalog: catalogData, session }, signal),
    // 用户目录变化慢，10 分钟一拉足够；写侧提交后随 access 前缀一起作废
    staleTime: 600_000,
  });
  const byId = useMemo(() => {
    const map = new Map<
      number,
      { username: string; email: string | null; status: string }
    >();
    for (const user of query.data ?? []) {
      map.set(user.id, {
        username: user.username,
        email: user.email,
        status: user.status,
      });
    }
    return map;
  }, [query.data]);
  return {
    users: query.data ?? [],
    byId,
    isPending: query.isPending,
    isError: query.isError,
    error: query.error,
  };
}

/// 权限展示元数据：目录条目 + UI 目录 → `Map<permission, PermissionMeta>`。
///
/// 复用 `usePermissionCatalog`（与权限组页同一粒查询、同一 query key，缓存共享）：
/// 传 `true` 使 enabled 退化为「目录里有 `list_permissions` 就读」——工作台只要
/// 能看到目录就拉，不依赖「当前组可管理」那粒判据。
export function usePermissionMeta(): {
  meta: Map<string, PermissionMeta>;
  isPending: boolean;
  isError: boolean;
  error: unknown;
  refetch: () => void;
} {
  const catalog = useUiCatalog();
  const catalogData = catalog.data;
  const permissionQuery = usePermissionCatalog(true);
  const meta = useMemo(
    () => buildPermissionMeta(permissionQuery.data ?? [], catalogData),
    [permissionQuery.data, catalogData],
  );
  return {
    meta,
    isPending: permissionQuery.isPending,
    isError: permissionQuery.isError,
    error: permissionQuery.error,
    refetch: () => void permissionQuery.refetch(),
  };
}

/// 工作台写侧入口：授予/撤销（与 `useGroupActions` 同一套 Step-up 外壳——
/// `access.grants.write` 的写 Action 同样挂在 Step-up 中间件下，见
/// `grants/mod.rs` 的 `step_up_targets`：428 时弹重认证并重放）。
///
/// 变更函数不缓存失效：写接口只回幂等标记，页面提交后回读，失效由调用方统一挂在
/// `accessGroupQueryKeys.root()` 上。
export type GrantActions = {
  grantPermission: (input: GrantPermissionInput) => Promise<void>;
  revokePermission: (userId: number, permission: string) => Promise<void>;
};

export function useGrantActions(): GrantActions {
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;
  const controller = useSessionController();

  const deps = useMemo<AccessInvokeDeps>(
    () => ({ catalog: catalogData, session }),
    [catalogData, session],
  );

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
      grantPermission: async (input: GrantPermissionInput) => {
        await request((proof) =>
          grantPermission(input, deps, undefined, proof),
        );
      },
      revokePermission: async (userId: number, permission: string) => {
        await request((proof) =>
          revokePermission(userId, permission, deps, undefined, proof),
        );
      },
    }),
    [deps, request],
  );
}

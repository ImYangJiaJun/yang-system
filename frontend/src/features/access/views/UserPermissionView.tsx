/**
 * 权限工作台·按用户视图：直授权限、所属权限组、有效权限并集三块总览。
 *
 * 三条刻意写死的不变量：
 *
 * 1. **过期行只留审计，不进并集**。`list_user_grants` 返回的审计视图含过期行
 *    （解析侧已失效），直授权限块照常展示它们（带「已过期」徽标），但有效权限
 *    并集把它们过滤掉——并集回答的是「现在能做什么」。
 * 2. **孤儿条目不进并集**。组条目里不在权限目录中的权限（目录收缩后残留）不算
 *    有效权限，与权限组页的孤儿语义同一判据（设计 §8.4）。
 * 3. **写入口按目录权限位渲染，不渲染即禁用**（与组页不变量 3 同一纪律）：
 *    授予/撤销需要 `access.grants.write` 系的两粒操作都在目录里才渲染按钮，
 *    缺一粒整块隐藏，绝不发注定 403 的请求。
 */

import { useMemo, useState, type ReactNode } from "react";
import { useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { Plus, RefreshCw } from "lucide-react";

import { hasOperation, useSessionCredentials, useUiCatalog } from "@/engine";
import { Badge } from "@/shared/ui/badge";
import { Button } from "@/shared/ui/button";
import { Skeleton } from "@/shared/ui/skeleton";
import { useToast } from "@/shared/lib/toast";

import {
  accessGroupQueryKeys,
  canReadGroups,
  getGroup,
  useGroupList,
  type AccessInvokeDeps,
  type GroupDetail,
  type GroupSummary,
} from "../api";
import { PermissionBadge } from "../components/PermissionBadge";
import { PermissionPickerDialog } from "../components/PermissionPickerDialog";
import { RevokeConfirmDialog } from "../components/RevokeConfirmDialog";
import { UserPicker } from "../components/UserPicker";
import { groupPermissionMeta, type PermissionMeta } from "../permission-meta";
import {
  accessWorkspaceQueryKeys,
  listUserGrants,
  useGrantActions,
  usePermissionMeta,
  useUserDirectory,
  WORKSPACE_OPERATION_IDS,
  type UserGrant,
} from "../workspace-api";

function messageOf(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

/// 目录里没有这条权限时（如目录刚收缩），用权限字符串本身顶一个展示形状。
function fallbackMeta(permission: string): PermissionMeta {
  const dot = permission.lastIndexOf(".");
  return {
    permission,
    title: permission,
    modulePrefix: dot > 0 ? permission.slice(0, dot) : permission,
    moduleTitle: undefined,
    description: undefined,
    declaredBy: [],
    adminEquivalent: false,
    reason: null,
  };
}

function formatDate(unixSeconds: number): string {
  return new Date(unixSeconds * 1000).toLocaleDateString();
}

/// 直授行的过期徽标：已过期 / 永久 / 临期「N 天后过期」（N 按自然日向上取整）。
function expiryBadges(grant: UserGrant): ReactNode {
  if (grant.expired) return <PermissionBadge kind="expired" />;
  if (grant.expiresAt === null) {
    return <Badge variant="outline">永久</Badge>;
  }
  const days = Math.ceil((grant.expiresAt - Date.now() / 1000) / 86400);
  return <Badge variant="secondary">{Math.max(days, 1)} 天后过期</Badge>;
}

/// 并集里的一条：直授或来自某组的来源们。
type UnionSource = { kind: "direct" } | { kind: "group"; title: string };

type UnionEntry = {
  permission: string;
  meta: PermissionMeta | undefined;
  sources: UnionSource[];
};

export function UserPermissionView() {
  const queryClient = useQueryClient();
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;
  const deps = useMemo<AccessInvokeDeps>(
    () => ({ catalog: catalogData, session }),
    [catalogData, session],
  );
  const grants = useGrantActions();
  const toast = useToast();
  const directory = useUserDirectory();
  const { meta } = usePermissionMeta();

  const [targetUserId, setTargetUserId] = useState<number | null>(null);
  const [grantDialogOpen, setGrantDialogOpen] = useState(false);
  /// 重新授予的目标权限：非空即弹窗预选该权限（直授模式，服务端原地续期过期行）。
  const [regrantPermission, setRegrantPermission] = useState<string | null>(
    null,
  );
  const [revokeTarget, setRevokeTarget] = useState<{
    userId: number;
    permission: string;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);

  const canRead =
    hasOperation(catalogData, WORKSPACE_OPERATION_IDS.lookup) &&
    hasOperation(catalogData, WORKSPACE_OPERATION_IDS.listUserGrants);
  const canWrite =
    hasOperation(catalogData, WORKSPACE_OPERATION_IDS.grantPermission) &&
    hasOperation(catalogData, WORKSPACE_OPERATION_IDS.revokePermission);
  const canReadGroupsOf = canReadGroups(catalogData);

  const grantsQuery = useQuery({
    enabled: canRead && targetUserId !== null,
    queryKey: accessWorkspaceQueryKeys.userGrants(targetUserId ?? 0),
    queryFn: ({ signal }) => listUserGrants(targetUserId!, deps, signal),
    staleTime: 5_000,
  });
  // `?? []` 每次渲染都是新数组：单独 memo，否则下游 useMemo 每次白算
  const grantsData = useMemo(
    () => grantsQuery.data?.grants ?? [],
    [grantsQuery.data],
  );

  const groupsQuery = useGroupList();
  const groups = useMemo(() => groupsQuery.data ?? [], [groupsQuery.data]);

  /// 组数量小：每个组的详情并行拉（成员判定与有效并集都要它）。
  /// ponytail: 逐组查询，组多后应由后端补「按用户聚合」的接口再换。
  const groupDetailQueries = useQueries({
    queries: groups.map((group) => ({
      queryKey: accessGroupQueryKeys.group(group.id),
      queryFn: ({ signal }) => getGroup(group.id, deps, signal),
      enabled: canRead && targetUserId !== null,
      staleTime: 5_000,
    })),
  });
  const groupDetails = useMemo(
    () =>
      new Map(
        groupDetailQueries.map((query, index) => [
          groups[index]?.id ?? 0,
          query.data,
        ]),
      ),
    [groupDetailQueries, groups],
  );

  /// 直授权限按模块分组（行模型与权限目录同一套分组，中文名/危险徽标同源）。
  const grantGroups = useMemo(() => {
    const rows = grantsData.map(
      (grant) => meta.get(grant.permission) ?? fallbackMeta(grant.permission),
    );
    return groupPermissionMeta(rows);
  }, [grantsData, meta]);
  const grantsByPermission = useMemo(
    () => new Map(grantsData.map((grant) => [grant.permission, grant])),
    [grantsData],
  );

  /// 有效权限并集：直授（未过期）∪ 各组成员条目（孤儿不算）。
  const union = useMemo<UnionEntry[]>(() => {
    const map = new Map<string, UnionEntry>();
    for (const grant of grantsData) {
      if (grant.expired) continue;
      const entry = map.get(grant.permission) ?? {
        permission: grant.permission,
        meta: meta.get(grant.permission),
        sources: [],
      };
      entry.sources.push({ kind: "direct" });
      map.set(grant.permission, entry);
    }
    for (const group of groups) {
      const detail = groupDetails.get(group.id);
      if (!detail) continue;
      for (const item of detail.items) {
        // 孤儿条目：不在权限目录里就不算有效权限（与组页孤儿语义同一判据）
        const itemMeta = meta.get(item.permission);
        if (itemMeta === undefined) continue;
        const entry = map.get(item.permission) ?? {
          permission: item.permission,
          meta: itemMeta,
          sources: [],
        };
        entry.sources.push({ kind: "group", title: group.title });
        map.set(item.permission, entry);
      }
    }
    return [...map.values()];
  }, [grantsData, groups, groupDetails, meta]);

  /// 写完全部回读：access 前缀一起作废（与组页同一纪律）。
  function refresh() {
    return queryClient.invalidateQueries({
      queryKey: accessGroupQueryKeys.root(),
    });
  }

  function submit(action: () => Promise<void>, successMessage?: string) {
    setError(null);
    setPending(true);
    void (async () => {
      try {
        await action();
        await refresh();
        if (successMessage !== undefined) {
          toast.success(successMessage);
        }
      } catch (cause) {
        setError(messageOf(cause));
      } finally {
        setPending(false);
      }
    })();
  }

  if (!canRead) {
    return (
      <p
        aria-live="polite"
        className="rounded-md border border-border bg-muted/50 px-3 py-2 text-sm"
      >
        {catalog.isPending ? "正在加载权限目录…" : "查看权限工作台需要先登录。"}
      </p>
    );
  }

  const revokeTargetName =
    revokeTarget === null
      ? undefined
      : `${revokeTarget.permission}（${
          directory.byId.get(revokeTarget.userId)?.username ??
          `#${revokeTarget.userId}`
        }）`;

  return (
    <div className="space-y-6">
      {error ? (
        <p
          role="alert"
          className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
        >
          {error}
        </p>
      ) : null}

      <section
        aria-labelledby="workspace-user-target-heading"
        className="space-y-2 rounded-xl border border-border bg-card p-4"
      >
        <h2
          id="workspace-user-target-heading"
          className="text-base font-medium"
        >
          目标用户
        </h2>
        <UserPicker
          multiple={false}
          value={targetUserId}
          onChange={setTargetUserId}
        />
      </section>

      {targetUserId === null ? (
        <p className="rounded-xl border border-border bg-card p-5 text-sm text-muted-foreground">
          选一个用户，这里会展示它的直授权限、所属权限组与有效权限并集。
        </p>
      ) : grantsQuery.isPending ? (
        <Skeleton className="h-64 w-full" />
      ) : grantsQuery.isError ? (
        <div
          role="alert"
          className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
        >
          <span>{messageOf(grantsQuery.error)}</span>
          <Button
            variant="outline"
            size="sm"
            onClick={() => void grantsQuery.refetch()}
          >
            <RefreshCw aria-hidden="true" />
            重试
          </Button>
        </div>
      ) : (
        <>
          <DirectGrantsCard
            groups={grantGroups}
            grantsByPermission={grantsByPermission}
            byId={directory.byId}
            targetUserId={targetUserId}
            canWrite={canWrite}
            busy={pending}
            onGrantNew={() => setGrantDialogOpen(true)}
            onRegrant={(permission) => setRegrantPermission(permission)}
            onRevoke={(userId, permission) =>
              setRevokeTarget({ userId, permission })
            }
          />
          <GroupMembershipCard
            groups={groups}
            groupDetails={groupDetails}
            targetUserId={targetUserId}
            canReadGroupsOf={canReadGroupsOf}
          />
          <EffectiveUnionCard union={union} />
        </>
      )}

      <PermissionPickerDialog
        open={grantDialogOpen || regrantPermission !== null}
        onOpenChange={(next) => {
          if (!next) {
            setGrantDialogOpen(false);
            setRegrantPermission(null);
          }
        }}
        mode="direct"
        busy={pending}
        presetPermission={regrantPermission}
        onSubmit={(selection) => {
          if (targetUserId === null) return;
          const permission = selection.permissions[0];
          if (!permission) return;
          setGrantDialogOpen(false);
          setRegrantPermission(null);
          submit(async () => {
            await grants.grantPermission({
              userId: targetUserId,
              permission,
              expiresAt: selection.expiresAt,
            });
            // 授权事实写的是目标用户的授权版本：对方要刷新会话才生效，
            // 提示里说清楚，别让人以为立刻就能用。
          }, "已授予，对方刷新会话后生效");
        }}
      />

      <RevokeConfirmDialog
        open={revokeTarget !== null}
        target={revokeTargetName}
        pending={pending}
        onConfirm={() => {
          const target = revokeTarget;
          if (target === null) return;
          setRevokeTarget(null);
          submit(async () => {
            await grants.revokePermission(target.userId, target.permission);
          }, `已撤销「${target.permission}」`);
        }}
        onCancel={() => setRevokeTarget(null)}
      />
    </div>
  );
}

/* ------------------------------ 直授权限块 ------------------------------- */

function DirectGrantsCard({
  groups,
  grantsByPermission,
  byId,
  targetUserId,
  canWrite,
  busy,
  onGrantNew,
  onRegrant,
  onRevoke,
}: {
  groups: ReturnType<typeof groupPermissionMeta>;
  grantsByPermission: Map<string, UserGrant>;
  byId: Map<number, { username: string; email: string | null; status: string }>;
  /// 撤销的 user_id 是选中的目标用户（直授行本身没有 userId 字段）。
  targetUserId: number;
  canWrite: boolean;
  busy: boolean;
  onGrantNew: () => void;
  onRegrant: (permission: string) => void;
  onRevoke: (userId: number, permission: string) => void;
}) {
  return (
    <section
      aria-labelledby="workspace-direct-grants-heading"
      className="space-y-2 rounded-xl border border-border bg-card p-4"
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2
          id="workspace-direct-grants-heading"
          className="text-base font-medium"
        >
          直授权限
        </h2>
        {canWrite ? (
          <Button size="sm" disabled={busy} onClick={onGrantNew}>
            <Plus aria-hidden="true" />
            授予新权限
          </Button>
        ) : null}
      </div>

      {groups.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          该用户没有任何直授权限。
        </p>
      ) : (
        <div className="space-y-2">
          {groups.map((group) => (
            <details
              key={group.title}
              open
              className="rounded-lg border border-border bg-muted/30"
            >
              <summary className="cursor-pointer px-3 py-2 text-sm font-medium select-none">
                {group.title}
                <span className="ml-2 text-xs font-normal text-muted-foreground">
                  共 {group.permissions.length} 项
                </span>
              </summary>
              <ul className="space-y-1 px-3 pb-3">
                {group.permissions.map((item) => {
                  const grant = grantsByPermission.get(item.permission);
                  if (grant === undefined) return null;
                  return (
                    <li
                      key={item.permission}
                      className="flex flex-wrap items-center justify-between gap-2 rounded-md border border-border px-3 py-2"
                    >
                      <div className="min-w-0">
                        <div className="flex flex-wrap items-center gap-2">
                          {item.title === item.permission ? (
                            <span className="font-mono text-sm">
                              {item.permission}
                            </span>
                          ) : (
                            <span className="text-sm font-medium">
                              {item.title}
                            </span>
                          )}
                          {item.adminEquivalent ? (
                            <PermissionBadge
                              kind="danger"
                              reason={item.reason}
                            />
                          ) : null}
                          {expiryBadges(grant)}
                        </div>
                        {item.title !== item.permission ? (
                          <span className="block truncate font-mono text-xs text-muted-foreground">
                            {item.permission}
                          </span>
                        ) : null}
                        <span className="block text-xs text-muted-foreground">
                          授予人{" "}
                          {byId.get(grant.grantedBy)?.username ??
                            `#${grant.grantedBy}`}{" "}
                          · {formatDate(grant.occurredAt)}
                        </span>
                      </div>
                      {canWrite ? (
                        <div className="flex flex-wrap gap-2">
                          <Button
                            variant="outline"
                            size="sm"
                            aria-label={`重新授予 ${item.permission}`}
                            disabled={busy}
                            onClick={() => onRegrant(item.permission)}
                          >
                            重新授予
                          </Button>
                          <Button
                            variant="outline"
                            size="sm"
                            aria-label={`撤销 ${item.permission}`}
                            disabled={busy}
                            onClick={() =>
                              onRevoke(targetUserId, item.permission)
                            }
                          >
                            撤销
                          </Button>
                        </div>
                      ) : null}
                    </li>
                  );
                })}
              </ul>
            </details>
          ))}
        </div>
      )}
    </section>
  );
}

/* ------------------------------ 所属权限组块 ------------------------------ */

function GroupMembershipCard({
  groups,
  groupDetails,
  targetUserId,
  canReadGroupsOf,
}: {
  groups: GroupSummary[];
  groupDetails: Map<number, GroupDetail | undefined>;
  targetUserId: number;
  canReadGroupsOf: boolean;
}) {
  const memberships = groups.filter((group) =>
    groupDetails.get(group.id)?.members.includes(targetUserId),
  );

  return (
    <section
      aria-labelledby="workspace-group-membership-heading"
      className="space-y-2 rounded-xl border border-border bg-card p-4"
    >
      <h2
        id="workspace-group-membership-heading"
        className="text-base font-medium"
      >
        所属权限组
      </h2>
      {!canReadGroupsOf ? (
        <p className="text-sm text-muted-foreground">
          当前身份看不到权限组，无法计算组成员身份。
        </p>
      ) : memberships.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          该用户不属于任何权限组。
        </p>
      ) : (
        <ul aria-label="所属权限组" className="space-y-1">
          {memberships.map((group) => (
            <li
              key={group.id}
              className="flex flex-wrap items-center justify-between gap-2 rounded-md border border-border px-3 py-2"
            >
              <div className="min-w-0">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="text-sm font-medium">{group.title}</span>
                  <span className="font-mono text-xs text-muted-foreground">
                    {group.groupKey}
                  </span>
                  {group.isBuiltin ? <PermissionBadge kind="builtin" /> : null}
                </div>
                <span className="block text-xs text-muted-foreground">
                  成员 {group.memberCount} · 权限 {group.itemCount}
                </span>
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/* ------------------------------ 有效权限并集 ----------------------------- */

function EffectiveUnionCard({ union }: { union: UnionEntry[] }) {
  return (
    <section
      aria-labelledby="workspace-effective-union-heading"
      className="space-y-2 rounded-xl border border-border bg-card p-4"
    >
      <h2
        id="workspace-effective-union-heading"
        className="text-base font-medium"
      >
        有效权限并集
      </h2>
      <p className="text-xs text-muted-foreground">
        直授（未过期）与所在权限组条目的并集；不在权限目录里的孤儿条目不计。
      </p>
      {union.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          该用户当前没有任何有效权限。
        </p>
      ) : (
        <ul aria-label="有效权限并集" className="space-y-1">
          {union.map((entry) => (
            <li
              key={entry.permission}
              className="flex flex-wrap items-center justify-between gap-2 rounded-md border border-border px-3 py-2"
            >
              <div className="min-w-0">
                {entry.meta !== undefined &&
                entry.meta.title !== entry.permission ? (
                  <>
                    <span className="block text-sm font-medium">
                      {entry.meta.title}
                    </span>
                    <span className="block truncate font-mono text-xs text-muted-foreground">
                      {entry.permission}
                    </span>
                  </>
                ) : (
                  <span className="block font-mono text-sm">
                    {entry.permission}
                  </span>
                )}
              </div>
              <span className="flex flex-wrap items-center gap-1">
                {entry.sources.map((source) =>
                  source.kind === "direct" ? (
                    <Badge key="direct" variant="outline">
                      直授
                    </Badge>
                  ) : (
                    <PermissionBadge
                      key={source.title}
                      kind="source"
                      sourceTitle={source.title}
                    />
                  ),
                )}
              </span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

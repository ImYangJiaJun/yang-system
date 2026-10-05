/**
 * 权限工作台·按功能视图：目录浏览 + 权限下钻（持有者两张表 + 授权入口）。
 *
 * 三条刻意写死的不变量：
 *
 * 1. **下钻接口按目录 fail-closed**：`list_holders` 对不在目录里的权限会直接 400
 *    （`ensure_declared`），所以右侧详情只认目录里选中的权限——左侧浏览列表
 *    就是唯一入口，不存在「手输一条权限再下钻」的路径。
 * 2. **持有者两张表口径不同**：直授表是审计视图（含过期行，带 expired 标记），
 *    组表每行是一个权限组（成员数另计）——两张表合起来才是「谁有这个权限」。
 * 3. **写入口按目录权限位渲染**：给用户走 `access.grants.grant_permission`，
 *    给组走 `access.groups.add_group_item`，两粒缺一粒该入口就不渲染
 *    （与组页不变量 3 同一纪律：不渲染即禁用，绝不发注定 403 的请求）。
 */

import { useEffect, useMemo, useState } from "react";
import {
  useQuery,
  useQueryClient,
  type UseQueryResult,
} from "@tanstack/react-query";
import { RefreshCw, UserPlus } from "lucide-react";

import { hasOperation, useSessionCredentials, useUiCatalog } from "@/engine";
import { Badge } from "@/shared/ui/badge";
import { Button } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import { Input } from "@/shared/ui/input";
import { Label } from "@/shared/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/shared/ui/select";
import { Skeleton } from "@/shared/ui/skeleton";
import { cn } from "@/shared/lib/utils";
import { useToast } from "@/shared/lib/toast";

import {
  accessGroupQueryKeys,
  GROUP_OPERATION_IDS,
  useGroupActions,
  useGroupList,
  type AccessInvokeDeps,
  type GroupSummary,
} from "../api";
import { PermissionBadge } from "../components/PermissionBadge";
import { UserPicker } from "../components/UserPicker";
import { groupPermissionMeta } from "../permission-meta";
import {
  accessWorkspaceQueryKeys,
  listHolders,
  useGrantActions,
  usePermissionMeta,
  useUserDirectory,
  WORKSPACE_OPERATION_IDS,
  type HolderEntry,
  type HoldersResult,
} from "../workspace-api";

function messageOf(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

function formatDate(unixSeconds: number): string {
  return new Date(unixSeconds * 1000).toLocaleDateString();
}

/// 持有者行的过期徽标（与按用户视图的直授徽标同一口径：已过期 / 永久 / 临期）。
function holderExpiryBadges(holder: HolderEntry) {
  if (holder.expired) return <PermissionBadge kind="expired" />;
  if (holder.expires_at === null) {
    return <Badge variant="outline">永久</Badge>;
  }
  const days = Math.ceil((holder.expires_at - Date.now() / 1000) / 86400);
  return <Badge variant="secondary">{Math.max(days, 1)} 天后过期</Badge>;
}

export function PermissionBrowseView() {
  const queryClient = useQueryClient();
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;
  const deps = useMemo<AccessInvokeDeps>(
    () => ({ catalog: catalogData, session }),
    [catalogData, session],
  );
  const grants = useGrantActions();
  const groupActions = useGroupActions();
  const toast = useToast();
  const directory = useUserDirectory();
  const { meta, isPending, isError, refetch } = usePermissionMeta();
  const groupsQuery = useGroupList();
  // `?? []` 每次渲染都是新数组：单独 memo，避免 visibleMeta 白算
  const groups = useMemo(() => groupsQuery.data ?? [], [groupsQuery.data]);

  const [selectedPermission, setSelectedPermission] = useState<string | null>(
    null,
  );
  const [search, setSearch] = useState("");
  const [grantDialogOpen, setGrantDialogOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);

  const canRead =
    hasOperation(catalogData, WORKSPACE_OPERATION_IDS.listHolders) &&
    hasOperation(catalogData, GROUP_OPERATION_IDS.listPermissions);
  const canWrite =
    hasOperation(catalogData, WORKSPACE_OPERATION_IDS.grantPermission) &&
    hasOperation(catalogData, GROUP_OPERATION_IDS.addItem);

  const holdersQuery = useQuery({
    enabled: canRead && selectedPermission !== null,
    queryKey: accessWorkspaceQueryKeys.holders(selectedPermission ?? ""),
    queryFn: ({ signal }) => listHolders(selectedPermission!, deps, signal),
    staleTime: 5_000,
  });

  const selected =
    selectedPermission === null ? undefined : meta.get(selectedPermission);

  /// 搜索：权限字符串 / 中文名 / 说明，三路都匹配（与授予弹窗同一过滤逻辑）。
  const needle = search.trim().toLowerCase();
  const visibleMeta = useMemo(() => {
    const all = [...meta.values()];
    if (needle === "") return all;
    return all.filter(
      (item) =>
        item.permission.toLowerCase().includes(needle) ||
        item.title.toLowerCase().includes(needle) ||
        (item.description ?? "").toLowerCase().includes(needle),
    );
  }, [meta, needle]);
  const catalogGroups = useMemo(
    () => groupPermissionMeta(visibleMeta),
    [visibleMeta],
  );

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

  return (
    <div className="grid gap-6 lg:grid-cols-[22rem_minmax(0,1fr)]">
      <CatalogPanel
        groups={catalogGroups}
        selectedPermission={selectedPermission}
        search={search}
        onSearchChange={setSearch}
        pending={isPending}
        isError={isError}
        onRetry={() => void refetch()}
        onSelect={setSelectedPermission}
      />

      <section
        aria-labelledby="workspace-permission-detail-heading"
        className="space-y-4"
      >
        <h2
          id="workspace-permission-detail-heading"
          className="sr-only text-base font-medium"
        >
          权限详情
        </h2>

        {error ? (
          <p
            role="alert"
            className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
          >
            {error}
          </p>
        ) : null}

        {selectedPermission === null ? (
          <p className="rounded-xl border border-border bg-card p-5 text-sm text-muted-foreground">
            左边选一条权限，这里展示它的基本信息、持有者与授权入口。
          </p>
        ) : selected === undefined ? (
          <p className="rounded-xl border border-border bg-card p-5 text-sm text-muted-foreground">
            这条权限已不在权限目录中，只能移除相关授权，不能重新下钻或授予。
          </p>
        ) : (
          <>
            <div className="space-y-2 rounded-xl border border-border bg-card p-4">
              <div className="flex flex-wrap items-center gap-2">
                <h3 className="text-base font-medium">{selected.title}</h3>
                {selected.adminEquivalent ? (
                  <PermissionBadge kind="danger" reason={selected.reason} />
                ) : null}
              </div>
              <span className="block truncate font-mono text-xs text-muted-foreground">
                {selected.permission}
              </span>
              {selected.declaredBy.length > 0 ? (
                <span className="block text-xs text-muted-foreground">
                  声明于 Action：{selected.declaredBy.join("、")}
                </span>
              ) : null}
              {selected.adminEquivalent && selected.reason !== null ? (
                <div
                  role="alert"
                  className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
                >
                  {selected.reason}
                </div>
              ) : null}
            </div>

            <HoldersPanel
              query={holdersQuery}
              byId={directory.byId}
              onRetry={() => void holdersQuery.refetch()}
            />

            {canWrite ? (
              <Button
                size="sm"
                disabled={pending}
                onClick={() => setGrantDialogOpen(true)}
              >
                <UserPlus aria-hidden="true" />
                给用户 / 组授权
              </Button>
            ) : null}
          </>
        )}
      </section>

      <GrantTargetDialog
        open={grantDialogOpen}
        onOpenChange={setGrantDialogOpen}
        permission={selectedPermission ?? ""}
        groups={groups}
        busy={pending}
        onSubmit={(target) => {
          const permission = selectedPermission;
          if (permission === null) return;
          setGrantDialogOpen(false);
          if (target.mode === "user") {
            submit(async () => {
              await grants.grantPermission({
                userId: target.userId,
                permission,
                expiresAt: null,
              });
              // 授权事实写的是目标用户的授权版本：对方要刷新会话才生效
            }, "已授予，对方刷新会话后生效");
          } else {
            submit(async () => {
              await groupActions.addItem(target.groupId, permission);
            }, `已把「${permission}」加入权限组`);
          }
        }}
      />
    </div>
  );
}

/* ------------------------------ 左：目录浏览 ------------------------------ */

function CatalogPanel({
  groups,
  selectedPermission,
  search,
  onSearchChange,
  pending,
  isError,
  onRetry,
  onSelect,
}: {
  groups: ReturnType<typeof groupPermissionMeta>;
  selectedPermission: string | null;
  search: string;
  onSearchChange: (value: string) => void;
  pending: boolean;
  isError: boolean;
  onRetry: () => void;
  onSelect: (permission: string) => void;
}) {
  return (
    <section
      aria-labelledby="workspace-catalog-heading"
      className="space-y-2 self-start rounded-xl border border-border bg-card p-4"
    >
      <div className="flex items-baseline justify-between gap-2">
        <h3 id="workspace-catalog-heading" className="text-base font-medium">
          权限目录
        </h3>
        <span className="text-xs text-muted-foreground">
          共 {groups.reduce((sum, group) => sum + group.permissions.length, 0)}{" "}
          项
        </span>
      </div>

      <Input
        value={search}
        onChange={(event) => onSearchChange(event.target.value)}
        placeholder="搜索权限…"
        aria-label="搜索权限"
      />

      {pending ? (
        <Skeleton className="h-40 w-full" />
      ) : isError ? (
        <div
          role="alert"
          className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
        >
          <span>权限目录加载失败，请重试。</span>
          <Button variant="outline" size="sm" onClick={onRetry}>
            <RefreshCw aria-hidden="true" />
            重试
          </Button>
        </div>
      ) : groups.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          {search.trim() === "" ? "权限目录是空的。" : "没有匹配的权限。"}
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
                {group.permissions.map((item) => (
                  <li key={item.permission}>
                    <button
                      type="button"
                      aria-pressed={item.permission === selectedPermission}
                      onClick={() => onSelect(item.permission)}
                      className={cn(
                        "flex w-full items-center justify-between gap-2 rounded-md border px-3 py-2 text-left transition-colors",
                        item.permission === selectedPermission
                          ? "border-primary bg-accent"
                          : "border-transparent hover:bg-accent",
                      )}
                    >
                      <span className="min-w-0">
                        <span className="block text-sm font-medium">
                          {item.title}
                        </span>
                        <span className="block truncate font-mono text-xs text-muted-foreground">
                          {item.permission}
                        </span>
                      </span>
                      {item.adminEquivalent ? (
                        <PermissionBadge kind="danger" reason={item.reason} />
                      ) : null}
                    </button>
                  </li>
                ))}
              </ul>
            </details>
          ))}
        </div>
      )}
    </section>
  );
}

/* ------------------------------ 右：持有者 ------------------------------ */

function HoldersPanel({
  query,
  byId,
  onRetry,
}: {
  query: UseQueryResult<HoldersResult>;
  byId: Map<number, { username: string; email: string | null; status: string }>;
  onRetry: () => void;
}) {
  const holders = query.data;

  if (query.isPending) {
    return <Skeleton className="h-48 w-full" />;
  }

  if (query.isError) {
    return (
      <div
        role="alert"
        className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
      >
        <span>{messageOf(query.error)}</span>
        <Button variant="outline" size="sm" onClick={onRetry}>
          <RefreshCw aria-hidden="true" />
          重试
        </Button>
      </div>
    );
  }

  const direct = holders?.direct ?? [];
  const groups = holders?.groups ?? [];

  return (
    <div className="space-y-4">
      <section className="space-y-2 rounded-xl border border-border bg-card p-4">
        <h4 className="text-sm font-medium">直授持有者</h4>
        {direct.length === 0 ? (
          <p className="text-sm text-muted-foreground">没有直授持有者。</p>
        ) : (
          <ul aria-label="直授持有者" className="space-y-1">
            {direct.map((holder) => (
              <li
                key={holder.user_id}
                className="flex flex-wrap items-center justify-between gap-2 rounded-md border border-border px-3 py-2"
              >
                <div className="min-w-0">
                  <span className="block text-sm font-medium">
                    {byId.get(holder.user_id)?.username ??
                      `用户 #${holder.user_id}`}
                  </span>
                  <span className="block text-xs text-muted-foreground">
                    授予人{" "}
                    {byId.get(holder.granted_by)?.username ??
                      `#${holder.granted_by}`}{" "}
                    · {formatDate(holder.occurred_at)}
                  </span>
                </div>
                {holderExpiryBadges(holder)}
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="space-y-2 rounded-xl border border-border bg-card p-4">
        <h4 className="text-sm font-medium">组持有者</h4>
        {groups.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            没有包含该权限条目的权限组。
          </p>
        ) : (
          <ul aria-label="组持有者" className="space-y-1">
            {groups.map((group) => (
              <li
                key={group.id}
                className="flex flex-wrap items-center justify-between gap-2 rounded-md border border-border px-3 py-2"
              >
                <div className="min-w-0">
                  <span className="block text-sm font-medium">
                    {group.title}
                  </span>
                  <span className="block font-mono text-xs text-muted-foreground">
                    {group.group_key}
                  </span>
                </div>
                <span className="text-xs text-muted-foreground">
                  成员 {group.member_count}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}

/* ------------------------------ 授权目标对话框 ---------------------------- */

function GrantTargetDialog({
  open,
  onOpenChange,
  permission,
  groups,
  busy,
  onSubmit,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  permission: string;
  groups: GroupSummary[];
  busy: boolean;
  onSubmit: (
    target:
      { mode: "user"; userId: number } | { mode: "group"; groupId: number },
  ) => void;
}) {
  const [mode, setMode] = useState<"user" | "group">("user");
  const [userId, setUserId] = useState<number | null>(null);
  const [groupId, setGroupId] = useState("");

  /// 关闭即清场：下次打开不带着上一次的目标。
  useEffect(() => {
    if (!open) {
      setMode("user");
      setUserId(null);
      setGroupId("");
    }
  }, [open]);

  const canSubmit =
    !busy && (mode === "user" ? userId !== null : groupId !== "");

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !busy) onOpenChange(false);
      }}
    >
      <DialogContent showCloseButton={!busy}>
        <DialogHeader>
          <DialogTitle>给用户 / 组授权</DialogTitle>
          <DialogDescription>
            把「{permission}」授予一个用户（直授）或加入一个权限组的条目。
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-3">
          <div
            role="tablist"
            aria-label="授权目标类型"
            className="flex w-fit gap-1 rounded-lg border border-border bg-muted/40 p-1"
          >
            <button
              type="button"
              role="tab"
              aria-selected={mode === "user"}
              onClick={() => setMode("user")}
              className={cn(
                "rounded-md px-3 py-1.5 text-sm transition-colors",
                mode === "user"
                  ? "bg-accent font-medium text-accent-foreground"
                  : "hover:bg-accent/60",
              )}
            >
              用户
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={mode === "group"}
              onClick={() => setMode("group")}
              className={cn(
                "rounded-md px-3 py-1.5 text-sm transition-colors",
                mode === "group"
                  ? "bg-accent font-medium text-accent-foreground"
                  : "hover:bg-accent/60",
              )}
            >
              权限组
            </button>
          </div>

          {mode === "user" ? (
            <UserPicker multiple={false} value={userId} onChange={setUserId} />
          ) : (
            <div className="space-y-1">
              <Label htmlFor="grant-target-group">目标权限组</Label>
              <Select value={groupId} onValueChange={setGroupId}>
                <SelectTrigger
                  id="grant-target-group"
                  className="w-full"
                  disabled={busy}
                >
                  <SelectValue placeholder="从权限组中选择…" />
                </SelectTrigger>
                <SelectContent>
                  {groups.map((group) => (
                    <SelectItem key={group.id} value={String(group.id)}>
                      {group.title}（{group.groupKey}）
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {groups.length === 0 ? (
                <p className="text-xs text-muted-foreground">
                  当前身份看不到权限组，无法选择组目标。
                </p>
              ) : null}
            </div>
          )}
        </div>

        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={busy}
          >
            取消
          </Button>
          <Button
            disabled={!canSubmit}
            onClick={() => {
              if (mode === "user" && userId !== null) {
                onSubmit({ mode: "user", userId });
              } else if (mode === "group" && groupId !== "") {
                onSubmit({ mode: "group", groupId: Number(groupId) });
              }
            }}
          >
            {mode === "user" ? "确认授予" : "确认加入组"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

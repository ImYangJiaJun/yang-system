/**
 * 权限组管理页（路由 `/access/groups`，路由级 lazy → 必须 default 导出）。
 *
 * 三块内容：组列表（含内置/孤儿徽标）、选中组的权限条目、选中组的成员。
 * 四个被刻意写死在页面上的不变量：
 *
 * 1. **内置全权组没有条目矩阵**。`system_admin` 的权限由权限目录实时计算，
 *    条目表里没有可增删的授权事实（设计 §15 第 3 条）。接口用 `effective_all`
 *    把这件事显式表达出来，页面据此换一整块渲染——不是把矩阵画成空的。
 * 2. **孤儿条目单独画**。目录收缩后组里会留下目录中已不存在的权限（设计 §8.4），
 *    服务端只标记不清理。它必须和正常条目长得不一样，并给出一条能点的退路：
 *    移除不做目录校验，所以孤儿只能靠移除来清。
 * 3. **权限门控是「不渲染」而不是「禁用」**：管理入口按**每个组**的 `canManage`
 *    显隐（后端按「组所有者或全局写权限」算好，authenticated-only 后登录不再等于
 *    可管理），复选框、加入表单与改名/删除整块消失（禁用表示「此刻不可用」，
 *    这里表示「这个入口不属于你」）。只有新建组按钮按登录态渲染（登录即可建组）。
 * 4. **写完全部回读**。九个写接口没有一个会回最新条目或成员，页面每次写完都把
 *    `access` 这个前缀下的查询一起作废重拉，不做乐观更新。
 * 5. **加权限走候选目录，不手输**：候选来自权限目录读接口
 *    （`access.grants.list_permissions`），与「能管理组」是两粒独立权限位——
 *    缺目录那粒时加入表单换成一句说明（同样是不渲染，不是禁用）。
 *    管理员等价权限在候选中带危害面徽标。
 *
 * 「共 N 项」里的 N 取的是**当前身份在界面目录里能看到的 Action 数**：
 * UI 目录不投影权限清单，服务端的完整口径只在权限目录里，页面拿不到。
 * 所以这句话旁边必须写明它的口径，别让一个偏小的数被读成「这个组只有这么点权限」。
 *
 * 组管理面主体（`GroupManagementContent`）同时被权限工作台「按组」tab 复用：
 * 会话刷新监听、写外壳、主从布局都在主体里，外层只差页面标题与 tab 外壳。
 * 本文件 default 导出仍是独立页（注册表自定义视图入口），两者共用同一块内容。
 */

import { useEffect, useMemo, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Plus, RefreshCw } from "lucide-react";

import { hasOperation, useUiCatalog } from "@/engine";
import { SESSION_REFRESHED_EVENT } from "@/engine/session/auth-session";
import { Badge } from "@/shared/ui/badge";
import { Button } from "@/shared/ui/button";
import { Checkbox } from "@/shared/ui/checkbox";
import { Input } from "@/shared/ui/input";
import { Label } from "@/shared/ui/label";
import { Skeleton } from "@/shared/ui/skeleton";
import { cn } from "@/shared/lib/utils";
import { useToast } from "@/shared/lib/toast";

import {
  accessGroupQueryKeys,
  GROUP_OPERATION_IDS,
  useGroupActions,
  useGroupDetail,
  useGroupList,
} from "../api";
import type { GroupDetail, GroupSummary } from "../api";
import { PermissionBadge } from "../components/PermissionBadge";
import { PermissionPickerDialog } from "../components/PermissionPickerDialog";
import { UserPicker } from "../components/UserPicker";
import { usePermissionMeta, useUserDirectory } from "../workspace-api";

function messageOf(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

export function GroupManagementContent() {
  const queryClient = useQueryClient();
  const catalog = useUiCatalog();
  const actions = useGroupActions();
  const listQuery = useGroupList();
  const toast = useToast();

  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);

  const groups = listQuery.data ?? [];

  // 主从布局里空着右半边只会让人以为「这个页面只有列表」：默认选中第一个。
  // 同时兜住「选中的组被删掉了」——**派生**而不是把纠正写回 state：
  // 状态里留一个已经不在列表里的 id 是无害的，而写回会在渲染与提交之间多开一扇窗。
  const selectedGroupId =
    selectedId !== null && groups.some((group) => group.id === selectedId)
      ? selectedId
      : (groups[0]?.id ?? null);

  const detailQuery = useGroupDetail(selectedGroupId);
  const detail = detailQuery.data ?? null;

  /// 写完之后把本域所有查询一起作废（详情键是列表键的子键，一次前缀失效全覆盖）。
  async function refresh() {
    await queryClient.invalidateQueries({
      queryKey: accessGroupQueryKeys.root(),
    });
  }

  /// 会话刷新（`yang:session-refreshed`）后把 access 前缀查询一起作废重拉：
  /// 别处对当前账号授权事实的改动（如权限授予）要目标用户刷新会话才生效，
  /// 事件到达这里就是「现在生效了」——列表、详情、候选目录整块回读一次。
  /// 与 `refresh()` 同一粒失效调用；`queryClient` 稳定，订阅只挂一次。
  useEffect(() => {
    const onSessionRefreshed = () => {
      void queryClient.invalidateQueries({
        queryKey: accessGroupQueryKeys.root(),
      });
    };
    window.addEventListener(SESSION_REFRESHED_EVENT, onSessionRefreshed);
    return () => {
      window.removeEventListener(SESSION_REFRESHED_EVENT, onSessionRefreshed);
    };
  }, [queryClient]);

  /// 所有写操作的共同外壳：清提示 → 执行 → 回读 → 落提示；失败就把服务端原文亮出来。
  /// 428（Step-up）已由 `useGroupActions` 内置的 `request`/`runProtected` 透明处理，
  /// 这里不需要再感知 `StepUpRequiredError`——只需执行、回读、落提示。
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

  /// 左栏危险徽标判据：组详情 items 里含管理员等价权限（G2）时给那一行
  /// 亮「管理员等价」——详情只有当前选中组在内存里，所以徽标只出现在
  /// 选中的那一行（列表其余组的详情没有拉，无从判断）。
  const { meta } = usePermissionMeta();
  const dangerSelected = useMemo(
    () =>
      detail !== null &&
      !detail.effectiveAll &&
      detail.items.some(
        (item) => meta.get(item.permission)?.adminEquivalent === true,
      ),
    [detail, meta],
  );

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

      {!actions.canRead ? (
        <p
          aria-live="polite"
          className="rounded-md border border-border bg-muted/50 px-3 py-2 text-sm"
        >
          {catalog.isPending ? "正在加载权限目录…" : "查看权限组需要先登录。"}
        </p>
      ) : listQuery.isError ? (
        <div
          role="alert"
          className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
        >
          <span>{messageOf(listQuery.error)}</span>
          <Button
            variant="outline"
            size="sm"
            onClick={() => void listQuery.refetch()}
          >
            <RefreshCw aria-hidden="true" />
            重试
          </Button>
        </div>
      ) : (
        <div className="grid gap-6 lg:grid-cols-[22rem_minmax(0,1fr)]">
          <GroupListPanel
            groups={groups}
            selectedId={selectedGroupId}
            dangerSelected={dangerSelected}
            pending={listQuery.isPending}
            canManage={actions.canManage}
            busy={pending}
            onSelect={setSelectedId}
            onCreate={(input) =>
              submit(async () => {
                await actions.createGroup(input);
              }, `已创建权限组「${input.title}」`)
            }
          />

          <GroupDetailPanel
            detail={detail}
            pending={detailQuery.isPending}
            isError={detailQuery.isError}
            error={detailQuery.error}
            onRetry={() => void detailQuery.refetch()}
            actions={actions}
            submit={submit}
            busy={pending}
            /// 目录计数只用于内置组那句话说清楚「算出来多少项」。
            visibleCatalogActionCount={catalog.data?.actions.length ?? 0}
          />
        </div>
      )}
    </div>
  );
}

/// 独立页壳（注册表自定义视图与旧入口仍在用）：标题 + 主体。
export default function PermissionGroupsPage() {
  return (
    <main className="mx-auto w-full max-w-6xl space-y-6 p-6">
      <div className="space-y-1">
        <h1 className="text-xl font-semibold">权限组</h1>
        <p className="text-sm text-muted-foreground">
          组是「权限集合 + 成员」的粘合：把权限加进组，再把账号加进组，
          账号就获得组里的全部权限。
        </p>
      </div>
      <GroupManagementContent />
    </main>
  );
}

/* ------------------------------- 左：组列表 ------------------------------- */

function GroupListPanel({
  groups,
  selectedId,
  dangerSelected,
  pending,
  canManage,
  busy,
  onSelect,
  onCreate,
}: {
  groups: GroupSummary[];
  selectedId: number | null;
  /// 当前选中组的详情里是否含管理员等价条目（只有选中行能判断，见调用方）。
  dangerSelected: boolean;
  pending: boolean;
  canManage: boolean;
  busy: boolean;
  onSelect: (groupId: number) => void;
  onCreate: (input: {
    groupKey: string;
    title: string;
    description?: string;
  }) => void;
}) {
  return (
    <section
      aria-labelledby="permission-group-list-heading"
      className="space-y-3 self-start rounded-xl border border-border bg-card p-4"
    >
      <div className="flex items-baseline justify-between gap-2">
        <h2
          id="permission-group-list-heading"
          className="text-base font-medium"
        >
          权限组
        </h2>
        <span className="text-xs text-muted-foreground">
          共 {groups.length} 个
        </span>
      </div>

      {pending ? (
        <Skeleton className="h-24 w-full" />
      ) : groups.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          还没有权限组。
          {canManage ? "用下面的表单建第一个。" : "登录后即可创建权限组。"}
        </p>
      ) : (
        <ul aria-label="权限组列表" className="space-y-1">
          {groups.map((group) => (
            <li key={group.id}>
              <button
                type="button"
                aria-pressed={group.id === selectedId}
                onClick={() => onSelect(group.id)}
                className={cn(
                  "w-full rounded-lg border px-3 py-2 text-left transition-colors",
                  group.id === selectedId
                    ? "border-primary bg-accent"
                    : "border-transparent hover:bg-accent",
                )}
              >
                <span className="flex flex-wrap items-center gap-2">
                  <span className="text-sm font-medium">{group.title}</span>
                  {group.isBuiltin ? (
                    <Badge variant="secondary">内置</Badge>
                  ) : null}
                  {group.orphanItemCount > 0 ? (
                    <span className="inline-flex items-center rounded-md border border-destructive/40 bg-destructive/10 px-2 py-0.5 text-xs font-medium text-destructive">
                      孤儿 {group.orphanItemCount}
                    </span>
                  ) : null}
                  {group.id === selectedId && dangerSelected ? (
                    <PermissionBadge kind="danger" />
                  ) : null}
                </span>
                <span className="mt-0.5 block text-xs text-muted-foreground">
                  {group.groupKey} · 成员 {group.memberCount} · 权限{" "}
                  {group.itemCount}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}

      {canManage ? <CreateGroupForm busy={busy} onCreate={onCreate} /> : null}
    </section>
  );
}

/// 建组：`group_key` 是不再可改的身份，所以表单只在这里收一次。
function CreateGroupForm({
  busy,
  onCreate,
}: {
  busy: boolean;
  onCreate: (input: {
    groupKey: string;
    title: string;
    description?: string;
  }) => void;
}) {
  const [groupKey, setGroupKey] = useState("");
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");

  return (
    <form
      className="space-y-2 border-t border-border pt-3"
      onSubmit={(event) => {
        event.preventDefault();
        if (groupKey.trim() === "" || title.trim() === "") return;
        onCreate({
          groupKey: groupKey.trim(),
          title: title.trim(),
          // 空描述 = 没有描述：发空串会被当成「有一个空描述」写进库
          description:
            description.trim() === "" ? undefined : description.trim(),
        });
        setGroupKey("");
        setTitle("");
        setDescription("");
      }}
    >
      <h3 className="text-sm font-medium">新建权限组</h3>
      <div className="space-y-1">
        <Label htmlFor="permission-group-key">组标识</Label>
        <Input
          id="permission-group-key"
          value={groupKey}
          onChange={(event) => setGroupKey(event.target.value)}
          placeholder="ops"
        />
      </div>
      <div className="space-y-1">
        <Label htmlFor="permission-group-title">展示名</Label>
        <Input
          id="permission-group-title"
          value={title}
          onChange={(event) => setTitle(event.target.value)}
          placeholder="运维"
        />
      </div>
      <div className="space-y-1">
        <Label htmlFor="permission-group-description">描述（可选）</Label>
        <Input
          id="permission-group-description"
          value={description}
          onChange={(event) => setDescription(event.target.value)}
        />
      </div>
      <Button type="submit" size="sm" disabled={busy}>
        <Plus aria-hidden="true" />
        新建权限组
      </Button>
    </form>
  );
}

/* ------------------------------- 右：组详情 ------------------------------- */

function GroupDetailPanel({
  detail,
  pending,
  isError,
  error,
  onRetry,
  actions,
  submit,
  busy,
  visibleCatalogActionCount,
}: {
  detail: GroupDetail | null;
  pending: boolean;
  isError: boolean;
  error: unknown;
  onRetry: () => void;
  actions: ReturnType<typeof useGroupActions>;
  submit: (action: () => Promise<void>, successMessage?: string) => void;
  busy: boolean;
  visibleCatalogActionCount: number;
}) {
  if (pending || (detail === null && !isError)) {
    return <Skeleton className="h-64 w-full" />;
  }

  if (isError) {
    return (
      <div
        role="alert"
        className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
      >
        <span>{messageOf(error)}</span>
        <Button variant="outline" size="sm" onClick={onRetry}>
          <RefreshCw aria-hidden="true" />
          重试
        </Button>
      </div>
    );
  }

  if (detail === null) {
    return (
      <p className="rounded-xl border border-border bg-card p-5 text-sm text-muted-foreground">
        左边还没有可选的组。
      </p>
    );
  }

  return (
    <div className="space-y-4">
      <header className="space-y-2 rounded-xl border border-border bg-card p-4">
        <div className="flex flex-wrap items-center gap-2">
          <h2
            id="permission-group-detail-heading"
            className="text-base font-medium"
          >
            {detail.title}
          </h2>
          <span className="font-mono text-xs text-muted-foreground">
            {detail.groupKey}
          </span>
          {detail.effectiveAll ? <PermissionBadge kind="builtin" /> : null}
        </div>
        {detail.description !== null ? (
          <p className="text-sm text-muted-foreground">{detail.description}</p>
        ) : null}

        {detail.canManage && !detail.effectiveAll ? (
          <>
            <RenameGroupForm
              detail={detail}
              busy={busy}
              onRename={(title, description) =>
                submit(async () => {
                  await actions.updateGroup({
                    groupId: detail.id,
                    title,
                    description,
                  });
                }, "已更新该组的展示信息")
              }
            />
            <Button
              variant="outline"
              size="sm"
              disabled={busy}
              onClick={() =>
                submit(async () => {
                  await actions.deleteGroup(detail.id);
                }, `已删除权限组「${detail.title}」`)
              }
            >
              删除该组
            </Button>
          </>
        ) : null}
      </header>

      <ItemPanel
        detail={detail}
        actions={actions}
        submit={submit}
        busy={busy}
        visibleCatalogActionCount={visibleCatalogActionCount}
      />

      <MemberPanel
        detail={detail}
        actions={actions}
        submit={submit}
        busy={busy}
      />
    </div>
  );
}

/// 改名/改描述。`group_key` 不可改，所以这里只出现两个可编辑字段。
function RenameGroupForm({
  detail,
  busy,
  onRename,
}: {
  detail: GroupDetail;
  busy: boolean;
  onRename: (title: string, description?: string) => void;
}) {
  const [title, setTitle] = useState(detail.title);
  const [description, setDescription] = useState(detail.description ?? "");

  // 切换组之后表单要跟着换成新组的现值，否则会把 A 组的名字存回 B 组。
  useEffect(() => {
    setTitle(detail.title);
    setDescription(detail.description ?? "");
  }, [detail.id, detail.title, detail.description]);

  return (
    <form
      className="flex flex-wrap items-end gap-2"
      onSubmit={(event) => {
        event.preventDefault();
        if (title.trim() === "") return;
        onRename(
          title.trim(),
          description.trim() === "" ? undefined : description.trim(),
        );
      }}
    >
      <div className="space-y-1">
        <Label htmlFor="permission-group-rename-title">展示名</Label>
        <Input
          id="permission-group-rename-title"
          value={title}
          onChange={(event) => setTitle(event.target.value)}
        />
      </div>
      <div className="space-y-1">
        <Label htmlFor="permission-group-rename-description">
          描述（可选）
        </Label>
        <Input
          id="permission-group-rename-description"
          value={description}
          onChange={(event) => setDescription(event.target.value)}
        />
      </div>
      <Button type="submit" variant="outline" size="sm" disabled={busy}>
        保存
      </Button>
    </form>
  );
}

/* ------------------------------ 权限条目块 ------------------------------- */

/// 条目文字：中文名与权限字符串不同才分两行；相同只留一行
/// （目录里没有这条权限时 title 回退成字符串本身，两行一样没意义）。
function PermissionItemText({
  title,
  permission,
  orphan,
}: {
  title: string;
  permission: string;
  orphan: boolean;
}) {
  if (title === permission) {
    return (
      <span
        className={cn(
          "min-w-0 font-mono text-sm",
          orphan && "text-destructive",
        )}
      >
        {permission}
      </span>
    );
  }
  return (
    <span className="min-w-0">
      <span className="block text-sm font-medium">{title}</span>
      <span className="block truncate font-mono text-xs text-muted-foreground">
        {permission}
      </span>
    </span>
  );
}

function ItemPanel({
  detail,
  actions,
  submit,
  busy,
  visibleCatalogActionCount,
}: {
  detail: GroupDetail;
  actions: ReturnType<typeof useGroupActions>;
  submit: (action: () => Promise<void>, successMessage?: string) => void;
  busy: boolean;
  visibleCatalogActionCount: number;
}) {
  const uiCatalog = useUiCatalog();
  // 展示元数据（中文名/管理员等价/孤儿判据）与「添加条目」候选共用同一粒查询：
  // usePermissionMeta 内部即 usePermissionCatalog(true)，同一 query key、缓存共享。
  const { meta, isError, refetch } = usePermissionMeta();
  const [pickerOpen, setPickerOpen] = useState(false);

  /// 加权限的候选来源是权限目录（`access.grants.list_permissions`），它与「能管理
  /// 这个组」是两粒独立的权限位：缺目录那粒时入口不渲染（见文件头不变量 3），
  /// 移除不受影响。
  const canSeeCatalog =
    detail.canManage &&
    hasOperation(uiCatalog.data, GROUP_OPERATION_IDS.listPermissions);

  /// 已在该组的条目：弹窗候选中挪出去（服务端幂等，但选出来发请求没有新效果）。
  const existingPermissions = detail.items.map((item) => item.permission);

  return (
    <section
      aria-labelledby="permission-group-items-heading"
      className="space-y-2 rounded-xl border border-border bg-card p-4"
    >
      <h3 id="permission-group-items-heading" className="text-sm font-medium">
        权限条目
      </h3>

      {detail.effectiveAll ? (
        // 内置全权组：权限由目录算出来，条目表里没有可增删的事实。
        // 不渲染矩阵，也不渲染空矩阵——那句话本身就是全部内容。
        <>
          <p className="text-sm text-muted-foreground">
            该组的权限由权限目录实时计算，共 {visibleCatalogActionCount}{" "}
            项，不在此处逐条列出。
          </p>
          <p className="text-xs text-muted-foreground">
            这里的项数按当前身份在界面目录里能看到的 Action
            计；服务端的完整口径以权限目录为准。
          </p>
        </>
      ) : (
        <>
          {detail.items.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              该组还没有任何权限条目，成员加进来也不会得到权限。
            </p>
          ) : (
            <ul aria-label="组权限条目" className="space-y-1">
              {detail.items.map((item) => {
                const itemMeta = meta.get(item.permission);
                return (
                  <li
                    key={item.permission}
                    data-slot="group-item"
                    data-orphan={item.isOrphan ? "true" : "false"}
                    className={cn(
                      "flex flex-wrap items-center justify-between gap-2 rounded-md border px-3 py-2",
                      // 孤儿条目：目录里已经没有这条权限了（设计 §8.4）。
                      // 警示样式是为了让「这条其实不生效」在列表里一眼可辨。
                      item.isOrphan
                        ? "border-destructive/40 bg-destructive/10"
                        : "border-border",
                    )}
                  >
                    {detail.canManage ? (
                      // 勾选态恒为「在组里」：这个列表画的就是组现有的条目，
                      // 取消勾选即移除。不能管理这个组时连复选框都不渲染（见文件头不变量 3）。
                      <label className="flex min-w-0 items-center gap-2">
                        <Checkbox
                          aria-label={`${itemMeta?.title ?? item.permission} 权限`}
                          checked
                          disabled={busy}
                          onCheckedChange={() =>
                            submit(async () => {
                              await actions.removeItem(
                                detail.id,
                                item.permission,
                              );
                            }, `已移除「${item.permission}」`)
                          }
                        />
                        <PermissionItemText
                          title={itemMeta?.title ?? item.permission}
                          permission={item.permission}
                          orphan={item.isOrphan}
                        />
                      </label>
                    ) : (
                      <PermissionItemText
                        title={itemMeta?.title ?? item.permission}
                        permission={item.permission}
                        orphan={item.isOrphan}
                      />
                    )}
                    <span className="flex flex-wrap items-center gap-1">
                      {itemMeta?.adminEquivalent ? (
                        <PermissionBadge
                          kind="danger"
                          reason={itemMeta.reason}
                        />
                      ) : null}
                      {item.isOrphan ? <PermissionBadge kind="orphan" /> : null}
                    </span>
                    {item.isOrphan ? (
                      <span className="text-xs text-destructive">
                        该权限已不在权限目录中，可安全移除
                      </span>
                    ) : null}
                  </li>
                );
              })}
            </ul>
          )}

          {detail.canManage ? (
            isError ? (
              // 目录查询失败：候选里「空」和「加载中」之外不该有第三种面孔，
              // 失败态必须单独画出来，否则和「加载出了空目录」无法区分。
              <div
                role="alert"
                className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
              >
                <span>权限目录加载失败，请重试；移除不受影响。</span>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => void refetch()}
                >
                  <RefreshCw aria-hidden="true" />
                  重试
                </Button>
              </div>
            ) : canSeeCatalog ? (
              <div className="space-y-2 border-t border-border pt-3">
                <Button
                  type="button"
                  size="sm"
                  disabled={busy}
                  onClick={() => setPickerOpen(true)}
                >
                  <Plus aria-hidden="true" />
                  添加条目
                </Button>
                <p className="text-xs text-muted-foreground">
                  从权限目录勾选要加入的权限（可多选）；已在该组的权限不在候选中。
                  标「管理员等价」的权限会显著扩大危害面，加入前请确认。
                </p>
              </div>
            ) : (
              <p className="text-xs text-muted-foreground">
                当前身份看不到权限目录（需要 access.grants.read），
                不能在此添加权限；移除不受影响。
              </p>
            )
          ) : null}
        </>
      )}

      <PermissionPickerDialog
        open={pickerOpen}
        onOpenChange={setPickerOpen}
        mode="group"
        busy={busy}
        exclude={existingPermissions}
        onSubmit={(selection) => {
          if (selection.permissions.length === 0) return;
          setPickerOpen(false);
          submit(async () => {
            // 逐条调用（幂等）：组条目没有批量写接口，多选结果依次写入
            for (const permission of selection.permissions) {
              await actions.addItem(detail.id, permission);
            }
          }, `已加入 ${selection.permissions.length} 条权限到该组`);
        }}
      />
    </section>
  );
}

/* ------------------------------- 成员块 -------------------------------- */

function MemberPanel({
  detail,
  actions,
  submit,
  busy,
}: {
  detail: GroupDetail;
  actions: ReturnType<typeof useGroupActions>;
  submit: (action: () => Promise<void>, successMessage?: string) => void;
  busy: boolean;
}) {
  const directory = useUserDirectory();
  // 多选后一次「加入所选成员」提交：逐条写（没有批量接口），比点候选即写安全
  // （误点一个候选不该立刻改授权事实）。
  const [selected, setSelected] = useState<number[]>([]);
  /// 内置全权组的成员操作一并锁定：权限由目录实时计算，成员是唯二可动的部分，
  /// 但全权组是整个系统的最顶层授权面，加删成员必须走专门流程（只读）。
  const locked = detail.effectiveAll;

  return (
    <section
      aria-labelledby="permission-group-members-heading"
      className="space-y-2 rounded-xl border border-border bg-card p-4"
    >
      <h3 id="permission-group-members-heading" className="text-sm font-medium">
        成员
      </h3>

      {detail.members.length === 0 ? (
        <p className="text-sm text-muted-foreground">该组还没有成员。</p>
      ) : (
        <ul aria-label="组成员" className="space-y-1">
          {detail.members.map((memberId) => (
            <li
              key={memberId}
              className="flex flex-wrap items-center justify-between gap-2 rounded-md border border-border px-3 py-2"
            >
              <span className="text-sm">
                {directory.byId.get(memberId)?.username ?? `用户 #${memberId}`}
              </span>
              {detail.canManage && !locked ? (
                <Button
                  variant="outline"
                  size="sm"
                  aria-label={`移出成员 #${memberId}`}
                  disabled={busy}
                  onClick={() =>
                    submit(async () => {
                      await actions.removeMember(detail.id, memberId);
                    }, `已移出成员 #${memberId}`)
                  }
                >
                  移出
                </Button>
              ) : null}
            </li>
          ))}
        </ul>
      )}

      {detail.canManage && !locked ? (
        <div className="space-y-2 border-t border-border pt-3">
          <UserPicker multiple value={selected} onChange={setSelected} />
          <Button
            type="button"
            size="sm"
            disabled={busy || selected.length === 0}
            onClick={() => {
              const userIds = [...selected];
              setSelected([]);
              submit(async () => {
                for (const userId of userIds) {
                  await actions.addMember(detail.id, userId);
                }
              }, `已把 ${userIds.length} 位用户加入该组`);
            }}
          >
            <Plus aria-hidden="true" />
            加入所选成员
          </Button>
        </div>
      ) : null}
    </section>
  );
}

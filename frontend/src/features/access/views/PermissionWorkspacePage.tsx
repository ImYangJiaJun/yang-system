/**
 * 权限工作台（路由 `/access/workspace`；旧 URL `/access/groups` 也指向本页，
 * 无 `?tab=` 参数时按路径默认「按组」tab——旧入口兼容）。
 *
 * 三个视图：按用户（直授权限/所属组/有效并集）、按功能（目录下钻持有者）、
 * 按组（组管理，复用 `PermissionGroupsPage` 的 `GroupManagementContent`）。
 *
 * shared/ui 没有 tabs 组件，这里自绘按钮组（与侧边栏 NavLink 同一套选中样式）。
 * 初始 tab 由 `?tab=users|groups` 指定（二选一简单实现，切 tab 不写回 URL——
 * 刷新回默认的代价是回到初始视图，可以接受）。
 *
 * 会话刷新（`yang:session-refreshed`）监听挂在壳上：别处对授权事实的改动
 * （如权限授予）要目标用户刷新会话才生效，事件到达就是「现在生效了」——
 * 三个视图的数据一次前缀失效整块回读（与权限组页同一纪律；组 tab 自己的
 * 监听保留，独立页壳（注册表路径）仍靠它）。
 */

import { useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useLocation, useSearchParams } from "react-router";

import { SESSION_REFRESHED_EVENT } from "@/engine/session/auth-session";
import { cn } from "@/shared/lib/utils";

import { accessGroupQueryKeys } from "../api";
import { PermissionBrowseView } from "./PermissionBrowseView";
import { GroupManagementContent } from "./PermissionGroupsPage";
import { UserPermissionView } from "./UserPermissionView";

type WorkspaceTab = "users" | "functions" | "groups";

const TABS: { id: WorkspaceTab; label: string }[] = [
  { id: "users", label: "按用户" },
  { id: "functions", label: "按功能" },
  { id: "groups", label: "按组" },
];

function TabButton({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: string;
}) {
  return (
    <button
      type="button"
      role="tab"
      aria-selected={active}
      onClick={onClick}
      className={cn(
        "rounded-md px-3 py-1.5 text-sm transition-colors",
        active
          ? "bg-accent font-medium text-accent-foreground"
          : "hover:bg-accent/60",
      )}
    >
      {children}
    </button>
  );
}

export default function PermissionWorkspacePage() {
  const queryClient = useQueryClient();
  const [searchParams] = useSearchParams();
  const location = useLocation();

  /// 初始 tab：`?tab=` 参数优先（users|groups）；无参数时旧 URL `/access/groups`
  /// 默认「按组」，其余默认「按用户」。
  const rawTab = searchParams.get("tab");
  const initialTab: WorkspaceTab =
    rawTab === "users" || rawTab === "groups"
      ? rawTab
      : location.pathname === "/access/groups"
        ? "groups"
        : "users";
  const [tab, setTab] = useState<WorkspaceTab>(initialTab);

  /// 会话刷新后把 access 前缀查询一起作废重拉（与组页同一粒失效调用；
  /// `queryClient` 稳定，订阅只挂一次）。
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

  return (
    <main className="mx-auto w-full max-w-6xl space-y-6 p-6">
      <div className="space-y-1">
        <h1 className="text-xl font-semibold">权限工作台</h1>
        <p className="text-sm text-muted-foreground">
          按用户、按功能、按组三个视角查看与管理权限：直授权限与权限组条目
          共同决定一个账号最终能做什么。
        </p>
      </div>

      <div
        role="tablist"
        aria-label="权限工作台视图"
        className="flex w-fit gap-1 rounded-lg border border-border bg-card p-1"
      >
        {TABS.map((item) => (
          <TabButton
            key={item.id}
            active={tab === item.id}
            onClick={() => setTab(item.id)}
          >
            {item.label}
          </TabButton>
        ))}
      </div>

      {tab === "users" ? (
        <UserPermissionView />
      ) : tab === "functions" ? (
        <PermissionBrowseView />
      ) : (
        <GroupManagementContent />
      )}
    </main>
  );
}

/**
 * 用户选择器：搜索下拉（防抖）+ 已选 chips。
 *
 * 两种模式由 `multiple` 区分：
 * - 单选（视图一：直授管理页选目标用户）——点候选替换选中，再点一次取消；
 * - 多选（视图三：权限组选成员）——点候选开关，chips 可逐个移除。
 *
 * 候选来自 `account.user.lookup`（目录里没有该操作时整块不渲染——省一次注定
 * 403 的往返）；已选项的展示名回退到全量用户目录（`useUserDirectory`），
 * 目录没拉到时退化成 `#id`。
 *
 * disabled/deleted 用户置灰并标「已停用/已删除」：停用用户的授权事实仍可查
 * （审计需要），但作为**新授予对象**应保持可见可辨，不能和可用用户混在一起。
 */

import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { X } from "lucide-react";

import { hasOperation, useSessionCredentials, useUiCatalog } from "@/engine";
import { Badge } from "@/shared/ui/badge";
import { Input } from "@/shared/ui/input";
import { cn } from "@/shared/lib/utils";

import {
  accessWorkspaceQueryKeys,
  listUserLookup,
  useUserDirectory,
  WORKSPACE_OPERATION_IDS,
} from "../workspace-api";

/// 值去抖（与 feishu 域的同名工具同一实现；域间禁止互相 import，这里保留本域副本）。
function useDebouncedValue<T>(value: T, delayMs: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = window.setTimeout(() => setDebounced(value), delayMs);
    return () => window.clearTimeout(timer);
  }, [value, delayMs]);
  return debounced;
}

/// 非 active 状态的界面文案；active 不标注（只有异常才值得占一个徽标位）。
function statusLabel(status: string): string | null {
  if (status === "disabled") return "已停用";
  if (status === "deleted") return "已删除";
  return null;
}

export type UserPickerProps =
  | {
      multiple: false;
      value: number | null;
      onChange: (userId: number | null) => void;
    }
  | {
      multiple: true;
      value: number[];
      onChange: (userIds: number[]) => void;
    };

export function UserPicker(props: UserPickerProps) {
  const { multiple } = props;
  const session = useSessionCredentials();
  const catalog = useUiCatalog();
  const catalogData = catalog.data;
  const directory = useUserDirectory();

  const [query, setQuery] = useState("");
  const debouncedQuery = useDebouncedValue(query, 300);

  const lookup = useQuery({
    enabled: hasOperation(catalogData, WORKSPACE_OPERATION_IDS.lookup),
    queryKey: accessWorkspaceQueryKeys.userLookup(debouncedQuery),
    queryFn: ({ signal }) =>
      listUserLookup({ catalog: catalogData, session }, debouncedQuery, signal),
    staleTime: 30_000,
  });

  /// 两种模式归一：已选恒是数组，单选数组至多一项。
  const selectedIds: number[] = multiple
    ? props.value
    : props.value === null
      ? []
      : [props.value];

  function choose(userId: number) {
    if (multiple) {
      const next = selectedIds.includes(userId)
        ? selectedIds.filter((id) => id !== userId)
        : [...selectedIds, userId];
      props.onChange(next);
      return;
    }
    // 单选：点同一个人 = 取消；点别人 = 替换
    props.onChange(props.value === userId ? null : userId);
  }

  /// 多选时把已选的从候选里挪出去（选了的不该再出现在可选项里）；
  /// 单选保持全量可见，方便直接换人。
  const candidates = multiple
    ? (lookup.data ?? []).filter((user) => !selectedIds.includes(user.id))
    : (lookup.data ?? []);

  return (
    <div className="space-y-2">
      {selectedIds.length > 0 ? (
        <ul aria-label="已选用户" className="flex flex-wrap gap-1">
          {selectedIds.map((id) => (
            <li key={id}>
              <span className="inline-flex items-center gap-1 rounded-md border border-border bg-muted/50 px-2 py-0.5 text-xs">
                {directory.byId.get(id)?.username ?? `#${id}`}
                <button
                  type="button"
                  aria-label={`移除用户 #${id}`}
                  className="rounded-sm opacity-60 transition-opacity hover:opacity-100"
                  onClick={() => choose(id)}
                >
                  <X className="size-3" aria-hidden="true" />
                </button>
              </span>
            </li>
          ))}
        </ul>
      ) : null}

      <Input
        value={query}
        onChange={(event) => setQuery(event.target.value)}
        placeholder="输入用户名或邮箱搜索用户…"
        aria-label="搜索用户"
      />

      {lookup.isPending ? (
        <p className="text-xs text-muted-foreground">正在搜索…</p>
      ) : lookup.isError ? (
        <p role="alert" className="text-xs text-destructive">
          用户搜索失败，请稍后重试。
        </p>
      ) : candidates.length === 0 ? (
        <p className="text-xs text-muted-foreground">没有匹配的用户。</p>
      ) : (
        <ul aria-label="用户候选" className="space-y-1">
          {candidates.map((user) => {
            const label = statusLabel(user.status);
            const inactive = label !== null;
            return (
              <li key={user.id}>
                <button
                  type="button"
                  aria-pressed={selectedIds.includes(user.id)}
                  onClick={() => choose(user.id)}
                  className={cn(
                    "flex w-full items-center justify-between gap-2 rounded-md border px-3 py-2 text-left transition-colors",
                    selectedIds.includes(user.id)
                      ? "border-primary bg-accent"
                      : "border-transparent hover:bg-accent",
                    inactive && "opacity-60",
                  )}
                >
                  <span className="min-w-0">
                    <span className="block text-sm font-medium">
                      {user.username}
                    </span>
                    <span className="block truncate font-mono text-xs text-muted-foreground">
                      {user.email ?? "（无邮箱）"}
                    </span>
                  </span>
                  {inactive ? <Badge variant="secondary">{label}</Badge> : null}
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

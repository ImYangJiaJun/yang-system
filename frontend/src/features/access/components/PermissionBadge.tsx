/**
 * 权限行上的状态徽标：危险 / 孤儿 / 过期 / 来源 / 内置。
 *
 * 五种面孔各有各的语义，颜色不是装饰（对齐 `PermissionGroupsPage` 的
 * 「警示样式是为了让事实一眼可辨」纪律）：
 * - **危险**：管理员等价权限（G2），红底，⚠ + 警示文案（reason 放 title 上悬停可读）；
 * - **孤儿**：权限已不在目录里（失效），琥珀；
 * - **过期**：直授已过期（解析侧已失效，行留作审计），灰；
 * - **来源**：「来自 XX 组」（通过权限组间接持有），描边；
 * - **内置**：内置全权组（锁），灰。
 */

import { AlertTriangle, Lock } from "lucide-react";

import { Badge } from "@/shared/ui/badge";
import { cn } from "@/shared/lib/utils";

export type PermissionBadgeKind =
  "danger" | "orphan" | "expired" | "source" | "builtin";

export function PermissionBadge({
  kind,
  reason,
  sourceTitle,
  className,
}: {
  kind: PermissionBadgeKind;
  /// danger 的警示文案（管理员等价 reason），悬停可见。
  reason?: string | null;
  /// source 的来源组名（「来自 XX 组」）。
  sourceTitle?: string;
  className?: string;
}) {
  if (kind === "danger") {
    return (
      <Badge
        variant="destructive"
        title={reason ?? undefined}
        className={className}
      >
        <AlertTriangle aria-hidden="true" />
        管理员等价
      </Badge>
    );
  }
  if (kind === "orphan") {
    // 琥珀是自定义样式：Badge 变体里没有这一档，按页面孤儿警示的写法自绘
    return (
      <span
        className={cn(
          "inline-flex items-center gap-1 rounded-md border border-amber-500/40 bg-amber-500/10 px-2 py-0.5 text-xs font-medium text-amber-600",
          className,
        )}
      >
        已失效
      </span>
    );
  }
  if (kind === "expired") {
    return (
      <Badge variant="secondary" className={className}>
        已过期
      </Badge>
    );
  }
  if (kind === "source") {
    return (
      <Badge variant="outline" className={className}>
        来自 {sourceTitle ?? "权限组"}
      </Badge>
    );
  }
  return (
    <Badge variant="secondary" className={className}>
      <Lock aria-hidden="true" />
      内置
    </Badge>
  );
}

/**
 * 审批派发配置行操作：查看映射 / 编辑映射 / 启停 / 删除。
 *
 * 只读身份可查看映射；编辑、启停和删除需要 `feishu.approval.write`。
 *
 * 删除的二次确认由页面持有（ConfirmDialog 形态，成功后 toast），这里只负责发出
 * 请求并回报；启停是 `update_config` 的 `enabled` 翻转，可逆，不走确认。
 */

import { Power, Trash2, ListTree } from "lucide-react";

import { Button } from "@/shared/ui/button";

import type { ApprovalConfigItem } from "../types";

export type ApprovalConfigRowActionsProps = {
  item: ApprovalConfigItem;
  canWrite: boolean;
  /// 某一行正在提交（启停或删除）时整行按钮禁用，防止连点。
  pending: boolean;
  /// 映射明细展开开关：`expanded` 由页面持有，这里只切换。
  expanded: boolean;
  onToggleExpanded: () => void;
  onToggleEnabled: (item: ApprovalConfigItem) => void;
  onRequestDelete: (item: ApprovalConfigItem) => void;
  onEdit?: (item: ApprovalConfigItem) => void;
};

export function ApprovalConfigRowActions({
  item,
  canWrite,
  pending,
  expanded,
  onToggleExpanded,
  onToggleEnabled,
  onRequestDelete,
  onEdit,
}: ApprovalConfigRowActionsProps) {
  return (
    <div className="flex items-center justify-end gap-1">
      <Button
        variant="ghost"
        size="sm"
        disabled={pending}
        onClick={onToggleExpanded}
        aria-expanded={expanded}
      >
        <ListTree aria-hidden="true" />
        {expanded ? "收起映射" : "映射明细"}
      </Button>
      {canWrite ? (
        <>
          <Button
            variant="ghost"
            size="sm"
            disabled={pending}
            onClick={() => onEdit?.(item)}
          >
            编辑映射
          </Button>
          <Button
            variant="ghost"
            size="sm"
            disabled={pending}
            onClick={() => onToggleEnabled(item)}
          >
            <Power aria-hidden="true" />
            {item.enabled ? "停用" : "启用"}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            disabled={pending}
            onClick={() => onRequestDelete(item)}
            className="text-destructive hover:text-destructive"
          >
            <Trash2 aria-hidden="true" />
            删除
          </Button>
        </>
      ) : (
        <span className="text-xs text-muted-foreground">只读</span>
      )}
    </div>
  );
}

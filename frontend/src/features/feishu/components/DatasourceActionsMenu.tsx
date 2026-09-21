/**
 * 列表项上的「⋯」操作菜单（卡片与台账共用，避免两份文案漂移）。
 *
 * **渲染条件由调用方给**：没有 `feishu.datasource.write` 时这个菜单整个不渲染
 * （不是禁用）。禁用表示「此刻不可用」，而这里的语义是「这个入口不属于你」。
 *
 * 「已停用」的数据源直接给「启用」——只多一个条目，却消掉了「唯一恢复路径藏在
 * 看不见的菜单里」这个状态。
 */

import { MoreHorizontal } from "lucide-react";

import { Button } from "@/shared/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/shared/ui/dropdown-menu";

import type { DatasourceItem } from "../types";

export type DatasourceActionsMenuProps = {
  item: DatasourceItem;
  onRename: (item: DatasourceItem) => void;
  onToggleStatus: (item: DatasourceItem) => void;
  onDelete: (item: DatasourceItem) => void;
  align?: "start" | "end";
};

export function DatasourceActionsMenu({
  item,
  onRename,
  onToggleStatus,
  onDelete,
  align = "end",
}: DatasourceActionsMenuProps) {
  const disabling = item.status === "active";

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          className="size-7 shrink-0"
          aria-label={`${item.title} 的操作`}
          onClick={(event) => event.stopPropagation()}
        >
          <MoreHorizontal aria-hidden="true" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align={align}
        onClick={(event) => event.stopPropagation()}
      >
        <DropdownMenuItem onSelect={() => onRename(item)}>
          重命名
        </DropdownMenuItem>
        <DropdownMenuItem onSelect={() => onToggleStatus(item)}>
          {disabling ? "停用" : "启用"}
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem variant="destructive" onSelect={() => onDelete(item)}>
          删除
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

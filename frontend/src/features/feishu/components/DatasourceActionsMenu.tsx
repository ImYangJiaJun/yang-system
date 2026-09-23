/**
 * 列表项上的「⋯」操作菜单（卡片与台账共用，避免两份文案漂移）。
 *
 * **渲染条件由调用方给**：没有 `feishu.datasource.write` 时这个菜单整个不渲染
 * （不是禁用）。禁用表示「此刻不可用」，而这里的语义是「这个入口不属于你」。
 *
 * # 为什么没有「停用 / 启用」
 *
 * 服务端退役字段级可写入口时，唯一能改 `status` 的端点（字段级 `update_datasource`）
 * 一并消失了：表级的 `update_datasource_table` 入参里根本没有这一项。所以这个菜单
 * 不放一个点了必然报错的条目——「停用」要等后端补上表级的状态写入再回来。
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
  /// 编辑（名称）。按**表级主键**定位，所以回调只认那一行。
  onEdit: (item: DatasourceItem) => void;
  onDelete: (item: DatasourceItem) => void;
  align?: "start" | "end";
};

export function DatasourceActionsMenu({
  item,
  onEdit,
  onDelete,
  align = "end",
}: DatasourceActionsMenuProps) {
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
        <DropdownMenuItem onSelect={() => onEdit(item)}>编辑</DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem variant="destructive" onSelect={() => onDelete(item)}>
          删除
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

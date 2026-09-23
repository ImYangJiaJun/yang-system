/**
 * 台账视图（默认视图）：上百条量级下的查找、排序与横向对比。
 *
 * 只有「名称」一列画排序箭头：表级行上可排的另一个列是主键 `id`，而它恒作
 * `withStableOrder` 的收尾键；「标识」在表级行上根本没有（它在字段绑定那一层）。行高受 `--density-cell-y` 驱动
 * （页头的「密度」菜单实时切换），所以这里**不写死 `py-3`**。
 *
 * 空结果集不在这里表达，理由同卡片视图。
 */

import { ArrowDown, ArrowUp, ArrowUpDown } from "lucide-react";

import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/shared/ui/table";
import { Skeleton } from "@/shared/ui/skeleton";

import type { DatasourceItem, OrderByClause } from "../types";
import { identityLabel } from "../types";
import { DatasourceActionsMenu } from "./DatasourceActionsMenu";
import { DatasourceStatusBadge } from "./StatusBadge";

const SKELETON_ROWS = 6;

/// 行高由密度变量驱动（ADR-5 §2.1），不要写死 padding。
const DENSITY_STYLE = { paddingBlock: "var(--density-cell-y)" } as const;

function SortableHeader({
  field,
  label,
  orderBy,
  onSort,
}: {
  field: string;
  label: string;
  orderBy: OrderByClause[];
  onSort: (field: string) => void;
}) {
  const current = orderBy.find((clause) => clause.field === field);
  const Icon = current
    ? current.direction === "Asc"
      ? ArrowUp
      : ArrowDown
    : ArrowUpDown;

  return (
    <button
      type="button"
      className="inline-flex items-center gap-1 rounded-sm font-medium focus-visible:ring-ring/50 focus-visible:ring-[3px] focus-visible:outline-none"
      onClick={() => onSort(field)}
      aria-label={`按${label}排序`}
    >
      {label}
      <Icon className="size-3.5 text-muted-foreground" aria-hidden="true" />
    </button>
  );
}

export type DatasourceLedgerProps = {
  items: DatasourceItem[];
  orderBy: OrderByClause[];
  /// 有写权限才渲染行末的「⋯」菜单。
  canWrite: boolean;
  pending?: boolean;
  onOpen: (item: DatasourceItem) => void;
  onSort: (field: string) => void;
  onEdit: (item: DatasourceItem) => void;
  onDelete: (item: DatasourceItem) => void;
};

export function DatasourceLedger({
  items,
  orderBy,
  canWrite,
  pending = false,
  onOpen,
  onSort,
  onEdit,
  onDelete,
}: DatasourceLedgerProps) {
  const nameSorted = orderBy.find((clause) => clause.field === "title");

  // 空结果集不在这里表达（要么是四步指引，要么是「没有匹配的数据源」），
  // 更不该渲染一张只有表头的空表。
  if (items.length === 0 && !pending) return null;

  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead
            style={DENSITY_STYLE}
            aria-sort={
              nameSorted
                ? nameSorted.direction === "Asc"
                  ? "ascending"
                  : "descending"
                : "none"
            }
          >
            <SortableHeader
              field="title"
              label="名称"
              orderBy={orderBy}
              onSort={onSort}
            />
          </TableHead>
          {/* 标识列**不放排序箭头**：表级行上没有单一的 `source_key`（它在字段绑定
              那一层，一条行有 N 个），而后端对不存在的排序列是 FieldNotFound——
              点一下会把整个列表请求打成 400。 */}
          <TableHead style={DENSITY_STYLE}>标识（首个字段）</TableHead>
          <TableHead style={DENSITY_STYLE}>状态</TableHead>
          {/* 「加密返回」与「默认语言」两列**删掉了**：它们属于绑定层——一条数据源
              有 N 个字段，可以各自加密、各自语言，所以表级行上根本没有单一值可显示。
              这两列读的键在表级投影里不存在，于是对每一条数据源都恒画「—」与一个
              空语言徽标。逐字段的取值在详情页的字段绑定表里。 */}
          <TableHead style={DENSITY_STYLE} className="w-10 text-right">
            <span className="sr-only">操作</span>
          </TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {pending
          ? Array.from({ length: SKELETON_ROWS }, (_, index) => (
              <TableRow key={index}>
                <TableCell style={DENSITY_STYLE} colSpan={4}>
                  <Skeleton className="h-4 w-full" />
                </TableCell>
              </TableRow>
            ))
          : items.map((item) => (
              <TableRow
                // 同 `DatasourceCardGrid`：`id` 是唯一的行身份，解析器保证非空。
                key={item.id ?? "—"}
                data-slot="datasource-ledger-row"
                className="group cursor-pointer"
                onClick={() => onOpen(item)}
              >
                <TableCell style={DENSITY_STYLE}>
                  <button
                    type="button"
                    className="rounded-sm text-left font-medium focus-visible:ring-ring/50 focus-visible:ring-[3px] focus-visible:outline-none"
                    onClick={(event) => {
                      event.stopPropagation();
                      onOpen(item);
                    }}
                  >
                    {item.title}
                  </button>
                </TableCell>
                <TableCell style={DENSITY_STYLE} className="font-mono text-xs">
                  {identityLabel(item)}
                </TableCell>
                <TableCell style={DENSITY_STYLE}>
                  <DatasourceStatusBadge status={item.status} />
                </TableCell>
                <TableCell style={DENSITY_STYLE} className="text-right">
                  {canWrite ? (
                    // 悬停/聚焦时才显形；权限不足时整个不渲染
                    <span className="inline-flex opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100">
                      <DatasourceActionsMenu
                        item={item}
                        onEdit={onEdit}
                        onDelete={onDelete}
                      />
                    </span>
                  ) : null}
                </TableCell>
              </TableRow>
            ))}
      </TableBody>
    </Table>
  );
}

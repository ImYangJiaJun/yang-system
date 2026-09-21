/**
 * 台账视图（默认视图）：上百条量级下的查找、排序与横向对比。
 *
 * 只有「名称」与「标识」两列画排序箭头——后端只有它们声明了 `sortable`，
 * 给别的列画箭头等于在要求后端改动。行高受 `--density-cell-y` 驱动
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
import { DatasourceActionsMenu } from "./DatasourceActionsMenu";
import {
  DatasourceStatusBadge,
  EncryptBadge,
  LocaleBadge,
} from "./StatusBadge";

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
  onRename: (item: DatasourceItem) => void;
  onToggleStatus: (item: DatasourceItem) => void;
  onDelete: (item: DatasourceItem) => void;
};

export function DatasourceLedger({
  items,
  orderBy,
  canWrite,
  pending = false,
  onOpen,
  onSort,
  onRename,
  onToggleStatus,
  onDelete,
}: DatasourceLedgerProps) {
  const nameSorted = orderBy.find((clause) => clause.field === "title");
  const keySorted = orderBy.find((clause) => clause.field === "source_key");

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
          <TableHead
            style={DENSITY_STYLE}
            aria-sort={
              keySorted
                ? keySorted.direction === "Asc"
                  ? "ascending"
                  : "descending"
                : "none"
            }
          >
            <SortableHeader
              field="source_key"
              label="标识"
              orderBy={orderBy}
              onSort={onSort}
            />
          </TableHead>
          <TableHead style={DENSITY_STYLE}>状态</TableHead>
          <TableHead style={DENSITY_STYLE}>加密返回</TableHead>
          <TableHead style={DENSITY_STYLE}>默认语言</TableHead>
          <TableHead style={DENSITY_STYLE} className="w-10 text-right">
            <span className="sr-only">操作</span>
          </TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {pending
          ? Array.from({ length: SKELETON_ROWS }, (_, index) => (
              <TableRow key={index}>
                <TableCell style={DENSITY_STYLE} colSpan={6}>
                  <Skeleton className="h-4 w-full" />
                </TableCell>
              </TableRow>
            ))
          : items.map((item) => (
              <TableRow
                key={item.sourceKey}
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
                  {item.sourceKey}
                </TableCell>
                <TableCell style={DENSITY_STYLE}>
                  <DatasourceStatusBadge status={item.status} />
                </TableCell>
                <TableCell style={DENSITY_STYLE}>
                  {item.encryptEnabled ? (
                    <EncryptBadge enabled />
                  ) : (
                    <span className="text-xs text-muted-foreground">—</span>
                  )}
                </TableCell>
                <TableCell style={DENSITY_STYLE}>
                  <LocaleBadge locale={item.defaultLocale} />
                </TableCell>
                <TableCell style={DENSITY_STYLE} className="text-right">
                  {canWrite ? (
                    // 悬停/聚焦时才显形；权限不足时整个不渲染
                    <span className="inline-flex opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100">
                      <DatasourceActionsMenu
                        item={item}
                        onRename={onRename}
                        onToggleStatus={onToggleStatus}
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

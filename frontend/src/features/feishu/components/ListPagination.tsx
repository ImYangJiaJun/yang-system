/**
 * 分页控件：两个视图共用同一份分页状态（外观一致，视图切换不改变页码）。
 *
 * `total` 为 null 时只说明「这次查询没要总数」——那就只给上一页/下一页，
 * 不编造「共 N 个」。
 */

import { ChevronLeft, ChevronRight } from "lucide-react";

import { Button } from "@/shared/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/shared/ui/select";

import { PAGE_SIZE_OPTIONS } from "../list-query";

export type ListPaginationProps = {
  page: number;
  pageSize: number;
  /// null = 本次查询没有请求总数（响应里的 total 为 null）。
  total: number | null;
  pending?: boolean;
  onPageChange: (page: number) => void;
  onPageSizeChange: (pageSize: number) => void;
};

export function ListPagination({
  page,
  pageSize,
  total,
  pending = false,
  onPageChange,
  onPageSizeChange,
}: ListPaginationProps) {
  const pageCount =
    total === null ? null : Math.max(1, Math.ceil(total / pageSize));
  const firstPage = page <= 1;
  const lastPage = pageCount !== null && page >= pageCount;

  return (
    <div className="flex flex-wrap items-center justify-between gap-2 text-sm">
      <p className="text-xs text-muted-foreground tabular-nums">
        {total === null
          ? `第 ${page} 页`
          : `共 ${total} 个 · 第 ${page} / ${pageCount} 页`}
      </p>
      <div className="flex items-center gap-2">
        <span className="text-xs text-muted-foreground">每页</span>
        <Select
          value={String(pageSize)}
          onValueChange={(value) => onPageSizeChange(Number(value))}
        >
          <SelectTrigger size="sm" aria-label="每页条数" className="w-20">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {PAGE_SIZE_OPTIONS.map((option) => (
              <SelectItem key={option} value={String(option)}>
                {option}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Button
          variant="outline"
          size="sm"
          aria-label="上一页"
          disabled={pending || firstPage}
          onClick={() => onPageChange(page - 1)}
        >
          <ChevronLeft aria-hidden="true" />
        </Button>
        <Button
          variant="outline"
          size="sm"
          aria-label="下一页"
          disabled={pending || lastPage}
          onClick={() => onPageChange(page + 1)}
        >
          <ChevronRight aria-hidden="true" />
        </Button>
      </div>
    </div>
  );
}

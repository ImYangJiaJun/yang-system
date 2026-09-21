/**
 * 列表工具栏：视图切换 + 搜索 + 状态筛选（+ 有写权限时的「添加数据源」）。
 *
 * 两个视图共用同一份搜索词与筛选条件，所以工具栏只有一份，切视图不丢上下文。
 * 「添加数据源」**整个不渲染**而不是禁用——禁用表示「此刻不可用」，
 * 而这里的语义是「这个入口不属于你」。
 */

import { Plus, Search } from "lucide-react";

import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import { cn } from "@/shared/lib/utils";

import type { DatasourceStatusFilter, DatasourceView } from "../types";

const VIEW_OPTIONS: ReadonlyArray<{
  value: DatasourceView;
  label: string;
}> = [
  { value: "ledger", label: "台账" },
  { value: "cards", label: "卡片" },
];

const STATUS_OPTIONS: ReadonlyArray<{
  value: DatasourceStatusFilter;
  label: string;
}> = [
  { value: "all", label: "全部" },
  { value: "active", label: "启用" },
  { value: "disabled", label: "已停用" },
];

function Segmented<T extends string>({
  label,
  options,
  value,
  onChange,
}: {
  label: string;
  options: ReadonlyArray<{ value: T; label: string }>;
  value: T;
  onChange: (value: T) => void;
}) {
  return (
    <div
      role="group"
      aria-label={label}
      className="inline-flex rounded-md border border-border p-0.5"
    >
      {options.map((option) => {
        const active = option.value === value;
        return (
          <button
            key={option.value}
            type="button"
            aria-pressed={active}
            onClick={() => onChange(option.value)}
            className={cn(
              "rounded-sm px-2 py-1 text-xs font-medium transition-colors",
              active
                ? "bg-accent text-accent-foreground"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            {option.label}
          </button>
        );
      })}
    </div>
  );
}

export type ListToolbarProps = {
  view: DatasourceView;
  onViewChange: (view: DatasourceView) => void;
  search: string;
  onSearchChange: (search: string) => void;
  status: DatasourceStatusFilter;
  onStatusChange: (status: DatasourceStatusFilter) => void;
  /// 有写权限且给了回调时才渲染「添加数据源」。
  canWrite?: boolean;
  onAdd?: () => void;
};

export function ListToolbar({
  view,
  onViewChange,
  search,
  onSearchChange,
  status,
  onStatusChange,
  canWrite = false,
  onAdd,
}: ListToolbarProps) {
  return (
    <div className="flex flex-wrap items-center gap-2">
      <Segmented
        label="视图"
        options={VIEW_OPTIONS}
        value={view}
        onChange={onViewChange}
      />
      <div className="relative">
        <Search
          className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-muted-foreground"
          aria-hidden="true"
        />
        <Input
          value={search}
          onChange={(event) => onSearchChange(event.target.value)}
          placeholder="搜索名称或标识"
          aria-label="搜索数据源"
          className="h-8 w-56 pl-7"
          autoComplete="off"
        />
      </div>
      <Segmented
        label="状态筛选"
        options={STATUS_OPTIONS}
        value={status}
        onChange={onStatusChange}
      />
      {canWrite && onAdd ? (
        <Button size="sm" className="ml-auto" onClick={onAdd}>
          <Plus aria-hidden="true" />
          添加数据源
        </Button>
      ) : null}
    </div>
  );
}

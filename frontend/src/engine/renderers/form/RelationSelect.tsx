import { useEffect, useMemo, useState } from "react";

import { invokeAction } from "@/engine/http/client";
import { useSessionCredentials } from "@/engine/session/use-session";
import { Input } from "@/shared/ui/input";
import { Button } from "@/shared/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/shared/ui/select";
import { parseRelationOptions } from "@/engine/contracts/table-data";
import type {
  ActionDemoSchema,
  FormFieldSchema,
} from "@/engine/contracts/ui-catalog";

type RelationOption = { value: string | number; label: string };

const PAGE_SIZE = 20;

/**
 * 关系选择器（旧 RelationSelect 语义）：初始/选中值加载 + 250ms 防抖远程搜索，
 * 请求契约 {search, selected, filter, page, limit} 与旧实现一致；
 * 选项控件为 Radix Select（shadcn 组件）。
 *
 * 三处与"能用"直接相关的处理：
 * - **已选值的标签跨搜索保留**：搜"张"时结果里没有当前选中的 7 号记录，
 *   以前下拉框会显示成裸 `7`，看起来像填错了。现在每次拉取都累积 value→label，
 *   已选值始终显示它第一次出现时的标签。
 * - **可清空**：非必填关系字段选错后，以前无法撤销（Radix Select 不接受空串 item）。
 * - **可继续加载**：结果多于一次请求时给「加载更多」，不再只能靠搜索命中。
 */
export function RelationSelect({
  value,
  onChange,
  label,
  field,
  action,
  disabled,
}: {
  value: unknown;
  onChange: (value: unknown) => void;
  label: string;
  field: FormFieldSchema;
  action?: ActionDemoSchema;
  disabled?: boolean;
}) {
  const session = useSessionCredentials();
  const [options, setOptions] = useState<RelationOption[]>([]);
  const [labels, setLabels] = useState<Record<string, string>>({});
  const [error, setError] = useState("");
  const [search, setSearch] = useState("");
  const [page, setPage] = useState(1);
  const [total, setTotal] = useState<number | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);

  const selectedValues = useMemo(
    () =>
      typeof value === "string" || typeof value === "number" ? [value] : [],
    [value],
  );

  // 搜索词变化时回到第一页；翻页只累积当前搜索词的结果。
  useEffect(() => {
    setPage(1);
  }, [search]);

  useEffect(() => {
    if (!action) {
      setError(
        `目录缺少关系 Action：${field.relation?.operation_id ?? "unknown"}`,
      );
      return;
    }
    const controller = new AbortController();
    const timer = window.setTimeout(
      () => {
        invokeAction(
          action,
          {
            search: search.trim() || null,
            selected: selectedValues,
            filter: {},
            page,
            limit: PAGE_SIZE,
          },
          session,
          controller.signal,
        )
          .then((result) => {
            if (result.kind !== "json")
              throw new Error("关系 Action 必须返回 JSON");
            const parsed = parseRelationOptions(result.data);
            setTotal(parsed.total ?? null);
            setOptions((previous) =>
              page === 1 ? parsed.items : mergeOptions(previous, parsed.items),
            );
            setLabels((previous) => {
              const next = { ...previous };
              for (const option of parsed.items) {
                next[String(option.value)] = option.label;
              }
              return next;
            });
            setError("");
          })
          .catch((cause: unknown) => {
            if (cause instanceof Error && cause.name === "AbortError") return;
            setError(cause instanceof Error ? cause.message : String(cause));
          })
          .finally(() => setLoadingMore(false));
      },
      search.trim() ? 250 : 0,
    );
    return () => {
      window.clearTimeout(timer);
      controller.abort();
    };
  }, [
    action,
    search,
    selectedValues,
    session,
    field.relation?.operation_id,
    page,
  ]);

  // 已选项即使不在当前结果里也要能显示：优先用累积标签，退化为原始值。
  const displayOptions = useMemo(() => {
    const merged = [...options];
    for (const selected of selectedValues) {
      if (merged.some((option) => Object.is(option.value, selected))) continue;
      merged.push({
        value: selected,
        label: labels[String(selected)] ?? String(selected),
      });
    }
    return merged;
  }, [options, selectedValues, labels]);

  const canLoadMore =
    total !== null && options.length > 0 && options.length < total;

  return (
    <div className="space-y-1.5">
      <Input
        placeholder={`搜索${label}`}
        aria-label={`搜索${label}`}
        className="h-8"
        value={search}
        onChange={(event) => setSearch(event.target.value)}
        disabled={disabled || !action}
      />
      <div className="flex items-center gap-1">
        <Select
          disabled={disabled || !action}
          // 受控空值用 ""（Radix Select 允许根值 ""，Item 不允许），避免受控/非受控切换。
          value={
            selectedValues[0] === undefined ? "" : String(selectedValues[0])
          }
          onValueChange={(raw) => {
            const option = displayOptions.find(
              (candidate) => String(candidate.value) === raw,
            );
            onChange(option?.value);
          }}
        >
          <SelectTrigger aria-label={label} className="w-full">
            <SelectValue placeholder="未选择" />
          </SelectTrigger>
          <SelectContent>
            {displayOptions.map((option) => (
              <SelectItem
                key={String(option.value)}
                value={String(option.value)}
              >
                {option.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        {selectedValues[0] !== undefined && !disabled && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            aria-label={`清除${label}`}
            onClick={() => onChange(undefined)}
          >
            清除
          </Button>
        )}
      </div>
      {canLoadMore && (
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="h-7 px-1 text-xs"
          disabled={loadingMore}
          onClick={() => {
            setLoadingMore(true);
            setPage((current) => current + 1);
          }}
        >
          {loadingMore
            ? "加载中…"
            : `加载更多（已显示 ${options.length} / ${total}）`}
        </Button>
      )}
      {error && (
        <p role="alert" className="text-xs text-destructive">
          {error}
        </p>
      )}
    </div>
  );
}

function mergeOptions(
  previous: RelationOption[],
  incoming: RelationOption[],
): RelationOption[] {
  const seen = new Set(previous.map((option) => String(option.value)));
  return [
    ...previous,
    ...incoming.filter((option) => !seen.has(String(option.value))),
  ];
}

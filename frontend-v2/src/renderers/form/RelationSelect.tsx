import { useEffect, useMemo, useState } from "react";

import { invokeAction } from "@/api/client";
import { useSessionCredentials } from "@/api/use-session";
import { parseRelationOptions } from "@/contracts/table-data";
import type { ActionDemoSchema, FormFieldSchema } from "@/contracts/ui-catalog";

type RelationOption = { value: string | number; label: string };

/**
 * 关系选择器（旧 RelationSelect 语义）：初始/选中值加载 + 250ms 防抖远程搜索，
 * 请求契约 {search, selected, filter, page, limit} 与旧实现一致。
 * M1 用原生 select 承载选项（jsdom 可测、无障碍语义完整），搜索框独立于选择器。
 */
export function RelationSelect({
  id,
  value,
  onChange,
  label,
  field,
  action,
  disabled,
}: {
  id?: string;
  value: unknown;
  onChange: (value: unknown) => void;
  label: string;
  field: FormFieldSchema;
  action?: ActionDemoSchema;
  disabled?: boolean;
}) {
  const session = useSessionCredentials();
  const [options, setOptions] = useState<RelationOption[]>([]);
  const [error, setError] = useState("");
  const [search, setSearch] = useState("");

  const selectedValues = useMemo(
    () =>
      typeof value === "string" || typeof value === "number" ? [value] : [],
    [value],
  );

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
            page: 1,
            limit: 20,
          },
          session,
          controller.signal,
        )
          .then((result) => {
            if (result.kind !== "json")
              throw new Error("关系 Action 必须返回 JSON");
            setOptions(parseRelationOptions(result.data).items);
            setError("");
          })
          .catch((cause: unknown) => {
            if (cause instanceof Error && cause.name === "AbortError") return;
            setError(cause instanceof Error ? cause.message : String(cause));
          });
      },
      search.trim() ? 250 : 0,
    );
    return () => {
      window.clearTimeout(timer);
      controller.abort();
    };
  }, [action, search, selectedValues, session, field.relation?.operation_id]);

  const displayOptions = useMemo(() => {
    const merged = [...options];
    for (const selected of selectedValues) {
      if (!merged.some((option) => Object.is(option.value, selected))) {
        merged.push({ value: selected, label: String(selected) });
      }
    }
    return merged;
  }, [options, selectedValues]);

  return (
    <div className="space-y-1">
      <input
        className="border-input mb-1 flex h-8 w-full rounded-md border bg-transparent px-2 text-sm outline-none focus-visible:ring-ring/50 focus-visible:ring-[3px]"
        placeholder={`搜索${label}`}
        aria-label={`搜索${label}`}
        value={search}
        onChange={(event) => setSearch(event.target.value)}
        disabled={disabled || !action}
      />
      <select
        id={id}
        aria-label={label}
        className="border-input flex h-9 w-full rounded-md border bg-transparent px-2 text-sm shadow-xs outline-none focus-visible:ring-ring/50 focus-visible:ring-[3px] disabled:opacity-50"
        disabled={disabled || !action}
        value={selectedValues[0] === undefined ? "" : String(selectedValues[0])}
        onChange={(event) => {
          const raw = event.target.value;
          if (!raw) return onChange(undefined);
          const option = displayOptions.find(
            (candidate) => String(candidate.value) === raw,
          );
          onChange(option?.value);
        }}
      >
        <option value="">未选择</option>
        {displayOptions.map((option) => (
          <option key={String(option.value)} value={String(option.value)}>
            {option.label}
          </option>
        ))}
      </select>
      {error && <p className="text-xs text-destructive">{error}</p>}
    </div>
  );
}

import { useEffect, useMemo, useState } from "react";

import { invokeAction } from "@/engine/http/client";
import { useSessionCredentials } from "@/engine/session/use-session";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { parseRelationOptions } from "@/engine/contracts/table-data";
import type {
  ActionDemoSchema,
  FormFieldSchema,
} from "@/engine/contracts/ui-catalog";

type RelationOption = { value: string | number; label: string };

/**
 * 关系选择器（旧 RelationSelect 语义）：初始/选中值加载 + 250ms 防抖远程搜索，
 * 请求契约 {search, selected, filter, page, limit} 与旧实现一致；
 * 选项控件为 Radix Select（shadcn 组件）。
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
    <div className="space-y-1.5">
      <Input
        placeholder={`搜索${label}`}
        aria-label={`搜索${label}`}
        className="h-8"
        value={search}
        onChange={(event) => setSearch(event.target.value)}
        disabled={disabled || !action}
      />
      <Select
        disabled={disabled || !action}
        // 受控空值用 ""（Radix Select 允许根值 ""，Item 不允许），避免受控/非受控切换。
        value={selectedValues[0] === undefined ? "" : String(selectedValues[0])}
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
            <SelectItem key={String(option.value)} value={String(option.value)}>
              {option.label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      {error && <p className="text-xs text-destructive">{error}</p>}
    </div>
  );
}

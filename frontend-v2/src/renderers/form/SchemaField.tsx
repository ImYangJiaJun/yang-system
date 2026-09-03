import { useId, useMemo, useState } from "react";
import { Controller, type Control } from "react-hook-form";

import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { effectiveSchema, type JsonSchemaNode } from "@/contracts/json-schema";
import type { ActionDemoSchema, FormFieldSchema } from "@/contracts/ui-catalog";
import { RelationSelect } from "./RelationSelect";
import { resolveFormControl } from "./form-control";

/**
 * 单字段控件分支表（对齐旧 SchemaField.vue）：widget hint → 显式控件落点，
 * 无 hint 时按 JSON Schema 类型/格式选择（分支逻辑见 form-control.ts）。
 */
export function SchemaField({
  name,
  schema,
  rootSchema,
  control,
  required,
  title,
  description,
  businessField,
  actions,
}: {
  name: string;
  schema: JsonSchemaNode;
  rootSchema: JsonSchemaNode;
  control: Control<Record<string, unknown>>;
  required: boolean;
  title?: string;
  description?: string;
  businessField?: FormFieldSchema;
  actions?: ActionDemoSchema[];
}) {
  const id = useId();
  const resolved = effectiveSchema(rootSchema, schema);
  const type = Array.isArray(resolved.type)
    ? resolved.type.find((item) => item !== "null")
    : resolved.type;
  const label = title || resolved.title || name;
  const fieldControl = resolveFormControl(
    businessField?.widget,
    type,
    Boolean(resolved.enum),
    resolved.format,
  );
  const relationAction = businessField?.relation
    ? actions?.find(
        (action) =>
          action.operation_id === businessField.relation?.operation_id,
      )
    : undefined;
  const help =
    description || businessField?.description || resolved.description;

  return (
    <Controller
      control={control}
      name={name}
      render={({ field, fieldState }) => (
        <div className="space-y-1.5">
          {fieldControl !== "toggle" && (
            <Label htmlFor={id}>
              {label}
              {required && <span className="text-destructive"> *</span>}
            </Label>
          )}
          <FieldControl
            id={id}
            control={fieldControl}
            type={type}
            value={field.value}
            onChange={field.onChange}
            resolved={resolved}
            disabled={businessField?.read_only}
            label={label}
            businessField={businessField}
            relationAction={relationAction}
          />
          {help && <p className="text-xs text-muted-foreground">{help}</p>}
          {fieldState.error && (
            <p className="text-xs text-destructive" role="alert">
              {fieldState.error.message}
            </p>
          )}
        </div>
      )}
    />
  );
}

function FieldControl({
  id,
  control,
  type,
  value,
  onChange,
  resolved,
  disabled,
  label,
  businessField,
  relationAction,
}: {
  id: string;
  control: ReturnType<typeof resolveFormControl>;
  type: string | undefined;
  value: unknown;
  onChange: (value: unknown) => void;
  resolved: JsonSchemaNode;
  disabled?: boolean;
  label: string;
  businessField?: FormFieldSchema;
  relationAction?: ActionDemoSchema;
}) {
  if (control === "relation") {
    if (!businessField?.relation) {
      return (
        <p className="text-xs text-destructive">关系控件缺少 relation 契约</p>
      );
    }
    return (
      <RelationSelect
        id={id}
        value={value}
        onChange={onChange}
        label={label}
        field={businessField}
        action={relationAction}
        disabled={disabled}
      />
    );
  }

  if (control === "enum") {
    const options = (resolved.enum ?? []).map((entry) => ({
      key: JSON.stringify(entry),
      label: typeof entry === "string" ? entry : JSON.stringify(entry),
      value: entry,
    }));
    const selected = JSON.stringify(value);
    return (
      <select
        id={id}
        aria-label={label}
        className="border-input flex h-9 w-full rounded-md border bg-transparent px-2 text-sm shadow-xs outline-none focus-visible:ring-ring/50 focus-visible:ring-[3px] disabled:opacity-50"
        disabled={disabled}
        value={
          options.some((option) => option.key === selected) ? selected : ""
        }
        onChange={(event) => {
          const option = options.find(
            (candidate) => candidate.key === event.target.value,
          );
          onChange(option?.value);
        }}
      >
        <option value="">未选择</option>
        {options.map((option) => (
          <option key={option.key} value={option.key}>
            {option.label}
          </option>
        ))}
      </select>
    );
  }

  if (control === "toggle") {
    return (
      <div className="flex items-center gap-2">
        <Checkbox
          id={id}
          checked={Boolean(value)}
          disabled={disabled}
          onCheckedChange={(checked) => onChange(checked === true)}
        />
        <Label htmlFor={id}>{label}</Label>
      </div>
    );
  }

  if (control === "number") {
    return (
      <Input
        id={id}
        type="number"
        step={type === "integer" ? "1" : "0.1"}
        min={resolved.minimum}
        max={resolved.maximum}
        disabled={disabled}
        value={typeof value === "number" ? value : ""}
        onChange={(event) => {
          const raw = event.target.value;
          if (!raw) return onChange(undefined);
          const parsed = Number(raw);
          if (Number.isFinite(parsed)) onChange(parsed);
        }}
      />
    );
  }

  if (control === "date_time") {
    return (
      <Input
        id={id}
        type="datetime-local"
        disabled={disabled}
        value={
          typeof value === "string" || typeof value === "number" ? value : ""
        }
        onChange={(event) => onChange(event.target.value || undefined)}
      />
    );
  }

  if (control === "color") {
    return (
      <input
        id={id}
        type="color"
        aria-label={label}
        disabled={disabled}
        value={typeof value === "string" ? value : "#000000"}
        onChange={(event) => onChange(event.target.value)}
      />
    );
  }

  if (control === "json") {
    return (
      <JsonDraftInput
        id={id}
        value={value}
        onChange={onChange}
        disabled={disabled}
      />
    );
  }

  if (control === "textarea") {
    return (
      <textarea
        id={id}
        rows={4}
        disabled={disabled}
        className="border-input flex w-full rounded-md border bg-transparent px-3 py-2 text-sm shadow-xs outline-none focus-visible:ring-ring/50 focus-visible:ring-[3px] disabled:opacity-50"
        maxLength={resolved.maxLength}
        value={typeof value === "string" ? value : String(value ?? "")}
        onChange={(event) => onChange(event.target.value || undefined)}
      />
    );
  }

  const inputType =
    control === "password" || control === "email" || control === "url"
      ? control
      : "text";
  return (
    <Input
      id={id}
      type={inputType}
      disabled={disabled}
      maxLength={resolved.maxLength}
      value={typeof value === "string" ? value : String(value ?? "")}
      onChange={(event) => onChange(event.target.value || undefined)}
    />
  );
}

/// JSON 控件：草稿失焦时解析，非法 JSON 就地提示（对齐旧 SchemaField 的 commitJson）。
function JsonDraftInput({
  id,
  value,
  onChange,
  disabled,
}: {
  id: string;
  value: unknown;
  onChange: (value: unknown) => void;
  disabled?: boolean;
}) {
  const [draft, setDraft] = useState(() =>
    value === undefined ? "" : JSON.stringify(value, null, 2),
  );
  const [error, setError] = useState("");
  const serialized = useMemo(
    () => (value === undefined ? "" : JSON.stringify(value, null, 2)),
    [value],
  );

  return (
    <div className="space-y-1">
      <textarea
        id={id}
        rows={6}
        disabled={disabled}
        className="border-input flex w-full rounded-md border bg-transparent px-3 py-2 font-mono text-xs shadow-xs outline-none focus-visible:ring-ring/50 focus-visible:ring-[3px] disabled:opacity-50"
        value={draft}
        onFocus={() => setDraft(serialized)}
        onChange={(event) => setDraft(event.target.value)}
        onBlur={() => {
          if (!draft.trim()) {
            onChange(undefined);
            setError("");
            return;
          }
          try {
            onChange(JSON.parse(draft));
            setError("");
          } catch (cause) {
            setError(cause instanceof Error ? cause.message : String(cause));
          }
        }}
      />
      {error && (
        <p className="text-xs text-destructive" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

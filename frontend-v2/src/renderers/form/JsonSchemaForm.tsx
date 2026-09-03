import { useMemo } from "react";
import { useForm } from "react-hook-form";

import { asJsonSchema, effectiveSchema } from "@/contracts/json-schema";
import type { ActionDemoSchema, FormFieldSchema } from "@/contracts/ui-catalog";
import { ajvResolver } from "./ajv-resolver";
import { SchemaField } from "./SchemaField";

/**
 * 动态 JSON Schema 表单（对齐旧 JsonSchemaForm.vue 编排）：
 * 字段集 = input_schema.properties；必填 = schema.required ∪ params[].required；
 * 校验 = Ajv 白名单编译器（ajv-resolver）。
 */
export function JsonSchemaForm({
  formId,
  schema,
  params,
  businessFields,
  actions,
  defaultValues,
  onSubmit,
}: {
  formId: string;
  schema: unknown;
  params?: ActionDemoSchema["params"];
  businessFields?: FormFieldSchema[];
  actions?: ActionDemoSchema[];
  defaultValues: Record<string, unknown>;
  onSubmit: (values: Record<string, unknown>) => void;
}) {
  const root = useMemo(() => asJsonSchema(schema), [schema]);
  const resolved = useMemo(() => effectiveSchema(root, root), [root]);
  const properties = resolved.properties ?? {};
  const requiredNames = useMemo(() => {
    const names = new Set(resolved.required ?? []);
    for (const parameter of params ?? []) {
      if (parameter.required) names.add(parameter.name);
    }
    return names;
  }, [resolved.required, params]);
  const paramByName = useMemo(
    () =>
      new Map((params ?? []).map((parameter) => [parameter.name, parameter])),
    [params],
  );
  const businessFieldByName = useMemo(
    () => new Map((businessFields ?? []).map((field) => [field.field, field])),
    [businessFields],
  );

  const form = useForm<Record<string, unknown>>({
    defaultValues,
    resolver: ajvResolver(schema),
  });

  if (Object.keys(properties).length === 0) {
    return (
      <form id={formId} onSubmit={form.handleSubmit(onSubmit)}>
        <p className="py-6 text-center text-sm text-muted-foreground">
          此 Action 无输入字段
        </p>
      </form>
    );
  }

  return (
    <form
      id={formId}
      noValidate
      className="space-y-4"
      onSubmit={form.handleSubmit(onSubmit)}
    >
      {Object.entries(properties).map(([name, property]) => (
        <SchemaField
          key={name}
          name={name}
          schema={property}
          rootSchema={root}
          control={form.control}
          required={requiredNames.has(name)}
          title={paramByName.get(name)?.title}
          description={paramByName.get(name)?.description}
          businessField={businessFieldByName.get(name)}
          actions={actions}
        />
      ))}
    </form>
  );
}

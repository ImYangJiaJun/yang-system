import { z } from "zod";

export const SUPPORTED_UI_SCHEMA_VERSIONS = ["2.2"] as const;
type SupportedUiSchemaVersion = (typeof SUPPORTED_UI_SCHEMA_VERSIONS)[number];

const jsonSchema = z.record(z.string(), z.unknown());

const paramSource = z.enum(["body", "query", "path", "header"]);

const multipartSpecSchema = z.object({
  max_fields: z.number().int().nonnegative(),
  max_files: z.number().int().nonnegative(),
  max_file_bytes: z.number().int().positive(),
  max_text_field_bytes: z.number().int().positive(),
  max_total_bytes: z.number().int().positive(),
  allowed_content_types: z.array(z.string().min(1)).min(1),
  lifecycle: z.enum(["request_scoped"]).catch("request_scoped"),
});

export const actionDemoSchema = z.object({
  operation_id: z.string().min(1),
  title: z.string(),
  description: z.string(),
  method: z.enum([
    "GET",
    "POST",
    "PUT",
    "PATCH",
    "DELETE",
    "HEAD",
    "OPTIONS",
    "TRACE",
  ]),
  path: z.string().startsWith("/"),
  params: z.array(
    z.object({
      name: z.string().min(1),
      source: paramSource,
      required: z.boolean(),
      title: z.string(),
      description: z.string(),
    }),
  ),
  input_schema: jsonSchema,
  output_schema: jsonSchema,
  request_media_type: z.enum(["json", "multipart"]).catch("json"),
  multipart: multipartSpecSchema.nullable().optional(),
  response_kind: z
    .enum(["json", "download", "preview", "redirect"])
    .catch("json"),
  requires_auth: z.boolean(),
});

const widgetHint = z
  .enum([
    "text",
    "textarea",
    "password",
    "email",
    "url",
    "color",
    "editor",
    "integer",
    "decimal",
    "switch",
    "radio",
    "relation_select",
    "tree_select",
    "date_time",
    "json",
  ])
  .catch("json");

const relationOptionsSchema = z.object({
  operation_id: z.string().min(1),
  value_field: z.string().min(1),
  label_fields: z.array(z.string().min(1)),
});

const fieldValidationSchema = z.object({
  min_length: z.number().int().nonnegative().optional(),
  max_length: z.number().int().nonnegative().optional(),
  minimum: z.string().optional(),
  maximum: z.string().optional(),
  pattern: z.string().optional(),
});

const tableColumnSchema = z.object({
  field: z.string().min(1),
  title: z.string(),
  description: z.string(),
  widget: widgetHint,
  required: z.boolean(),
  searchable: z.boolean(),
  filterable: z.boolean(),
  sortable: z.boolean(),
  relation: relationOptionsSchema.optional(),
});

export const formFieldSchema = z.object({
  field: z.string().min(1),
  title: z.string(),
  description: z.string(),
  widget: widgetHint,
  required: z.boolean(),
  read_only: z.boolean(),
  write_only: z.boolean(),
  relation: relationOptionsSchema.optional(),
  validation: fieldValidationSchema.optional(),
});

const actionPresentationSchema = z.object({
  operation_id: z.string().min(1),
  title: z.string(),
  placement: z.enum(["row", "bulk", "toolbar"]).catch("toolbar"),
  interaction: z.enum([
    "form",
    "download",
    "preview",
    "navigate",
    "custom",
    "invoke",
  ]),
  confirmation: z
    .object({ title: z.string(), message: z.string() })
    .nullable()
    .optional(),
  availability: z
    .object({
      state: z.enum(["hidden", "disabled"]).catch("disabled"),
      reason: z.string(),
    })
    .nullable()
    .optional(),
  view_id: z.string().nullable().optional(),
});

export const tableViewSchema = z.object({
  view_id: z.string().min(1),
  title: z.string(),
  table: z.string().min(1),
  data_action: z.string().min(1),
  columns: z.array(tableColumnSchema),
  form: z.object({ fields: z.array(formFieldSchema) }),
  tree: z
    .object({
      id_field: z.string().min(1),
      parent_field: z.string().min(1),
      label_field: z.string().min(1),
      max_nodes: z.number().int().positive(),
    })
    .optional(),
  query: z.object({
    search_fields: z.array(z.string().min(1)),
    filter_fields: z.array(z.string().min(1)),
    default_sort: z.array(
      z.object({
        field: z.string().min(1),
        direction: z.enum(["asc", "desc"]).catch("asc"),
      }),
    ),
    default_page_size: z.number().int().positive(),
    max_page_size: z.number().int().positive(),
  }),
  actions: z.array(z.string().min(1)),
  action_presentations: z.array(actionPresentationSchema),
});

export const uiCatalogSchema = z
  .object({
    schema_version: z.string().min(1),
    revision: z.string().regex(/^[0-9a-f]{64}$/i),
    actions: z.array(actionDemoSchema),
    table_views: z.array(tableViewSchema),
  })
  .superRefine((catalog, context) => {
    if (
      !SUPPORTED_UI_SCHEMA_VERSIONS.includes(
        catalog.schema_version as SupportedUiSchemaVersion,
      )
    ) {
      context.addIssue({
        code: "custom",
        path: ["schema_version"],
        message: `不支持 UI schema 版本 ${catalog.schema_version}，当前支持 ${SUPPORTED_UI_SCHEMA_VERSIONS.join(", ")}`,
      });
    }
  });

export const apiEnvelopeSchema = <T extends z.ZodType>(data: T) =>
  z.object({
    code: z.number().int(),
    message: z.string(),
    data: data.optional(),
  });

export const uiCatalogEnvelopeSchema = apiEnvelopeSchema(uiCatalogSchema);

export type ActionDemoSchema = z.infer<typeof actionDemoSchema>;
export type UiCatalog = z.infer<typeof uiCatalogSchema>;
export type UiParamSource = z.infer<typeof paramSource>;
export type TableViewSchema = z.infer<typeof tableViewSchema>;
export type FormFieldSchema = z.infer<typeof formFieldSchema>;
export type ActionPresentationSchema = z.infer<typeof actionPresentationSchema>;

export class ContractError extends Error {
  readonly details: string[];

  constructor(message: string, details: string[] = []) {
    super(message);
    this.name = "ContractError";
    this.details = details;
  }
}

export function parseUiCatalog(payload: unknown): UiCatalog {
  const parsed = uiCatalogEnvelopeSchema.safeParse(payload);
  if (!parsed.success) {
    throw new ContractError(
      "UI catalog 契约校验失败",
      parsed.error.issues.map(
        (issue) => `${issue.path.join(".") || "<root>"}: ${issue.message}`,
      ),
    );
  }
  if (parsed.data.code !== 0) {
    throw new ContractError(`UI catalog 请求失败：${parsed.data.message}`);
  }
  if (!parsed.data.data) {
    throw new ContractError("UI catalog 成功响应缺少 data");
  }
  return parsed.data.data;
}

import type { FormFieldSchema } from "@/contracts/ui-catalog";

export type FormControl =
  | "text"
  | "textarea"
  | "password"
  | "email"
  | "url"
  | "color"
  | "number"
  | "toggle"
  | "enum"
  | "relation"
  | "date_time"
  | "json";

type WidgetHint = FormFieldSchema["widget"];

// WidgetHint 是可降级提示，但每一种提示都必须在前端有显式、可审查的落点。
// editor 降级为多行纯文本；tree_select 在当前扁平 options 契约下安全降级为关系选择器。
const WIDGET_CONTROLS = {
  text: "text",
  textarea: "textarea",
  password: "password",
  email: "email",
  url: "url",
  color: "color",
  editor: "textarea",
  integer: "number",
  decimal: "number",
  switch: "toggle",
  radio: "enum",
  relation_select: "relation",
  tree_select: "relation",
  date_time: "date_time",
  json: "json",
} satisfies Record<WidgetHint, FormControl>;

export function resolveFormControl(
  widget: WidgetHint | undefined,
  schemaType: string | undefined,
  hasEnum: boolean,
  format: string | undefined,
): FormControl {
  if (widget) return WIDGET_CONTROLS[widget];
  if (hasEnum) return "enum";
  if (schemaType === "object" || schemaType === "array") return "json";
  if (schemaType === "boolean") return "toggle";
  if (schemaType === "integer" || schemaType === "number") return "number";
  if (format === "password") return "password";
  if (format === "email") return "email";
  if (format === "uri" || format === "url") return "url";
  if (format === "date-time") return "date_time";
  return "text";
}

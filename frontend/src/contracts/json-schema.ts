export type JsonSchemaNode = {
  $ref?: string;
  type?: string | string[];
  title?: string;
  description?: string;
  default?: unknown;
  enum?: unknown[];
  format?: string;
  properties?: Record<string, JsonSchemaNode>;
  required?: string[];
  items?: JsonSchemaNode;
  anyOf?: JsonSchemaNode[];
  oneOf?: JsonSchemaNode[];
  allOf?: JsonSchemaNode[];
  $defs?: Record<string, JsonSchemaNode>;
  definitions?: Record<string, JsonSchemaNode>;
  minimum?: number;
  maximum?: number;
  minLength?: number;
  maxLength?: number;
  pattern?: string;
  readOnly?: boolean;
  writeOnly?: boolean;
};

export function asJsonSchema(value: unknown): JsonSchemaNode {
  return value !== null && typeof value === "object"
    ? (value as JsonSchemaNode)
    : {};
}

export function resolveSchema(
  root: JsonSchemaNode,
  node: JsonSchemaNode,
): JsonSchemaNode {
  if (!node.$ref) return node;
  const segments = node.$ref.split("/");
  if (segments[0] !== "#" || segments.length < 3) return node;
  let current: unknown = root;
  for (const segment of segments.slice(1)) {
    const key = segment.replaceAll("~1", "/").replaceAll("~0", "~");
    if (current === null || typeof current !== "object" || !(key in current))
      return node;
    current = (current as Record<string, unknown>)[key];
  }
  if (current === null || typeof current !== "object") return node;
  return { ...(current as JsonSchemaNode), ...node, $ref: undefined };
}

export function effectiveSchema(
  root: JsonSchemaNode,
  node: JsonSchemaNode,
): JsonSchemaNode {
  const resolved = resolveSchema(root, node);
  const branches = resolved.anyOf ?? resolved.oneOf;
  if (!branches) return resolved;
  const nonNull = branches.find((branch) => branch.type !== "null");
  return nonNull ? effectiveSchema(root, nonNull) : resolved;
}

export function initialValue(
  root: JsonSchemaNode,
  node: JsonSchemaNode,
): unknown {
  const schema = effectiveSchema(root, node);
  if (schema.default !== undefined) return structuredClone(schema.default);
  const type = Array.isArray(schema.type)
    ? schema.type.find((item) => item !== "null")
    : schema.type;
  if (type === "object" || schema.properties) {
    return Object.fromEntries(
      Object.entries(schema.properties ?? {}).map(([name, property]) => [
        name,
        initialValue(root, property),
      ]),
    );
  }
  return undefined;
}

export function initialObject(schemaValue: unknown): Record<string, unknown> {
  const root = asJsonSchema(schemaValue);
  const value = initialValue(root, root);
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

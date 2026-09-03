import Ajv2020, { type ValidateFunction } from "ajv/dist/2020";
import draft7MetaSchema from "ajv/dist/refs/json-schema-draft-07.json";
import addFormats from "ajv-formats";

/**
 * ADR-4 动态 JSON Schema 校验轨。
 *
 * 后端 Action 的 input_schema 是数据驱动的动态 Schema（见 contracts/ui-catalog.ts），
 * 运行时校验用 Ajv 2020-12；固定协议（Catalog envelope 等）用 zod，两条轨不混用。
 *
 * 关键词白名单参照 contracts/json-schema.ts 的 JsonSchemaNode 子集：编译动态 Schema 前
 * 先扫描全部节点，出现白名单外关键词必须显式报错，禁止静默忽略（fail-closed）。
 */

export const SUPPORTED_SCHEMA_KEYWORDS: ReadonlySet<string> = new Set([
  // 引用与定义
  "$schema",
  "$id",
  "$ref",
  "$defs",
  "definitions",
  // 结构
  "type",
  "properties",
  "required",
  "additionalProperties",
  "items",
  "allOf",
  "anyOf",
  "oneOf",
  "enum",
  // 注解
  "title",
  "description",
  "default",
  "format",
  "readOnly",
  "writeOnly",
  // string 约束
  "minLength",
  "maxLength",
  "pattern",
  // 数值约束
  "minimum",
  "maximum",
  "exclusiveMinimum",
  "exclusiveMaximum",
  // array 约束
  "minItems",
  "maxItems",
]);

/// 值为“子 Schema”或“子 Schema 集合”的关键词，扫描时按对应形态递归。
const SUB_SCHEMA_KEYS: Record<string, "map" | "single" | "list"> = {
  properties: "map",
  $defs: "map",
  definitions: "map",
  items: "single",
  additionalProperties: "single",
  allOf: "list",
  anyOf: "list",
  oneOf: "list",
};

export type UnsupportedKeywordViolation = {
  path: string;
  keyword: string;
};

export class UnsupportedSchemaKeywordError extends Error {
  readonly violations: UnsupportedKeywordViolation[];

  constructor(violations: UnsupportedKeywordViolation[]) {
    super(
      `动态 Schema 包含白名单外关键词：${violations
        .map(
          (violation) =>
            `${violation.path} 的 ${JSON.stringify(violation.keyword)}`,
        )
        .join("；")}`,
    );
    this.name = "UnsupportedSchemaKeywordError";
    this.violations = violations;
  }
}

/// 递归扫描动态 Schema，收集全部白名单外关键词；为空数组表示通过。
export function findUnsupportedKeywords(
  schema: unknown,
): UnsupportedKeywordViolation[] {
  const violations: UnsupportedKeywordViolation[] = [];
  const visit = (node: unknown, path: string) => {
    if (node === null || typeof node !== "object" || Array.isArray(node))
      return;
    for (const [keyword, value] of Object.entries(node)) {
      if (!SUPPORTED_SCHEMA_KEYWORDS.has(keyword)) {
        violations.push({ path, keyword });
      }
      const shape = SUB_SCHEMA_KEYS[keyword];
      if (shape === "map" && value !== null && typeof value === "object") {
        for (const [name, child] of Object.entries(value)) {
          visit(child, `${path}.${keyword}.${name}`);
        }
      } else if (shape === "list" && Array.isArray(value)) {
        value.forEach((child, index) =>
          visit(child, `${path}.${keyword}[${index}]`),
        );
      } else if (shape === "single") {
        visit(value, `${path}.${keyword}`);
      }
    }
  };
  visit(schema, "$");
  return violations;
}

export function assertSupportedDynamicSchema(schema: unknown): void {
  const violations = findUnsupportedKeywords(schema);
  if (violations.length > 0)
    throw new UnsupportedSchemaKeywordError(violations);
}

/// 后端（schemars）下发的 Rust 整数格式；显式注册为带取值域的数值校验，strict 模式下
/// 未注册格式会报错，这里不是静默忽略而是落实真实边界。
const INTEGER_FORMAT_RANGES: Record<string, [number, number]> = {
  int32: [-(2 ** 31), 2 ** 31 - 1],
  uint32: [0, 2 ** 32 - 1],
  int64: [-(2 ** 63), 2 ** 63 - 1],
  uint64: [0, 2 ** 64 - 1],
};

export function createDynamicSchemaAjv(): Ajv2020 {
  const ajv = new Ajv2020({ strict: true, allErrors: true });
  addFormats(ajv);
  for (const [name, [min, max]] of Object.entries(INTEGER_FORMAT_RANGES)) {
    ajv.addFormat(name, {
      type: "number",
      validate: (value: number) =>
        Number.isInteger(value) && value >= min && value <= max,
    });
  }
  // 后端 input_schema 以 draft-07 方言标记（$schema + 本地 definitions），
  // 注册 draft-07 元 Schema 让 Ajv 2020-12 实例显式接受该方言而非静默忽略。
  ajv.addMetaSchema(draft7MetaSchema);
  return ajv;
}

const sharedAjv = { current: undefined as Ajv2020 | undefined };

function defaultAjv(): Ajv2020 {
  sharedAjv.current ??= createDynamicSchemaAjv();
  return sharedAjv.current;
}

/// 编译动态 Schema：先过白名单扫描（白名单外关键词显式报错），再交给 Ajv 编译。
export function compileDynamicSchema(schema: unknown): ValidateFunction {
  assertSupportedDynamicSchema(schema);
  return defaultAjv().compile(schema as Record<string, unknown>);
}

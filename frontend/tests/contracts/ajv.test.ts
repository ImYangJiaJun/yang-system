import { describe, expect, it } from "vitest";

import {
  assertSupportedDynamicSchema,
  compileDynamicSchema,
  findUnsupportedKeywords,
  UnsupportedSchemaKeywordError,
} from "@/contracts/ajv";

describe("动态 Schema 关键词白名单", () => {
  it("合法子集（后端 input_schema 真实形态）编译通过并可校验", () => {
    // 形态取自后端 demo.notes.query 的 input_schema：draft-07 方言标记 + 本地
    // definitions + allOf 包装 $ref + default 注解。
    const schema = {
      $schema: "http://json-schema.org/draft-07/schema#",
      type: "object",
      additionalProperties: false,
      definitions: {
        SortOrder: {
          oneOf: [
            { type: "string", enum: ["Asc"] },
            { type: "string", enum: ["Desc"] },
          ],
        },
      },
      properties: {
        direction: {
          allOf: [{ $ref: "#/definitions/SortOrder" }],
          default: "Asc",
          description: "排序方向",
        },
        keyword: {
          type: "string",
          minLength: 1,
          maxLength: 64,
          pattern: "^\\w+$",
        },
        page: { type: "integer", minimum: 1 },
        email: { type: "string", format: "email" },
        tags: { type: "array", items: { type: "string" }, maxItems: 8 },
        enabled: { type: "boolean" },
      },
      required: ["direction"],
    };

    const validate = compileDynamicSchema(schema);

    expect(validate({ direction: "Desc", keyword: "abc", page: 2 })).toBe(true);
    expect(validate({ direction: "Sideways" })).toBe(false);
    expect(validate({ direction: "Asc", email: "not-an-email" })).toBe(false);
    expect(validate({ direction: "Asc", keyword: "带中文" })).toBe(false);
  });

  it("anyOf 可空模式编译通过", () => {
    const validate = compileDynamicSchema({
      type: "object",
      properties: {
        nickname: { anyOf: [{ type: "string" }, { type: "null" }] },
      },
    });
    expect(validate({ nickname: null })).toBe(true);
    expect(validate({ nickname: 1 })).toBe(false);
  });

  it("后端 Rust 整数格式（uint32 等）显式落实取值域而非静默忽略", () => {
    const validate = compileDynamicSchema({
      type: "object",
      properties: {
        page: { type: "integer", format: "uint32" },
      },
    });
    expect(validate({ page: 7 })).toBe(true);
    expect(validate({ page: -1 })).toBe(false);
    expect(validate({ page: 2 ** 32 })).toBe(false);
    expect(validate({ page: 1.5 })).toBe(false);
  });

  it("白名单外关键词在顶层显式报错", () => {
    expect(() =>
      compileDynamicSchema({ type: "string", patternProperties: { "^x": {} } }),
    ).toThrow(UnsupportedSchemaKeywordError);
    expect(() =>
      compileDynamicSchema({ type: "string", patternProperties: { "^x": {} } }),
    ).toThrow(/patternProperties/);
  });

  it("白名单外关键词在嵌套节点显式报错并带定位路径", () => {
    const schema = {
      type: "object",
      properties: {
        status: { not: { const: "archived" } },
      },
    };
    try {
      assertSupportedDynamicSchema(schema);
      expect.unreachable("必须拒绝白名单外关键词");
    } catch (error) {
      expect(error).toBeInstanceOf(UnsupportedSchemaKeywordError);
      const failure = error as UnsupportedSchemaKeywordError;
      // 未知关键词的子树不再递归（fail-closed 已在父级报错），只定位到 not 本身。
      expect(failure.violations).toEqual([
        { path: "$.properties.status", keyword: "not" },
      ]);
    }
  });

  it("definitions 内的白名单外关键词同样被拒绝", () => {
    expect(
      findUnsupportedKeywords({
        definitions: { Evil: { type: "string", contentEncoding: "base64" } },
      }),
    ).toEqual([{ path: "$.definitions.Evil", keyword: "contentEncoding" }]);
  });

  it("白名单扫描先于 Ajv 报错，给出可识别错误而不是静默忽略", () => {
    // propertyNames 在 strict 模式下 Ajv 可编译但属于白名单外能力；
    // 必须先被白名单拦截，错误类型可识别。
    expect(() =>
      compileDynamicSchema({ type: "object", propertyNames: { maxLength: 1 } }),
    ).toThrow(UnsupportedSchemaKeywordError);
  });
});

import { describe, expect, it } from "vitest";

import { ajvResolver, mapAjvErrors } from "./ajv-resolver";

const schema = {
  type: "object",
  additionalProperties: false,
  properties: {
    name: { type: "string", minLength: 2 },
    category_id: { type: "integer", format: "int64" },
    status: { type: "string" },
  },
  required: ["name", "category_id", "status"],
};

/// RHF Resolver 返回同步值或 Promise，测试统一归一为 Promise。
const settle = <T>(value: T | Promise<T>) => Promise.resolve(value);

describe("ajvResolver", () => {
  it("合法值原样通过", async () => {
    const values = { name: "项目", category_id: 1, status: "active" };
    const result = await settle(
      ajvResolver(schema)(values, undefined, {} as never),
    );
    expect(result).toEqual({ values, errors: {} });
  });

  it("缺失必填字段映射为字段级 required 错误", async () => {
    const result = await settle(
      ajvResolver(schema)({ status: "active" }, undefined, {} as never),
    );
    expect(result.values).toEqual({});
    expect(Object.keys(result.errors).sort()).toEqual(["category_id", "name"]);
    expect(result.errors.name).toMatchObject({ type: "required" });
  });

  it("约束违例按字段路径映射并保留 Ajv 消息", async () => {
    const result = await settle(
      ajvResolver(schema)(
        { name: "x", category_id: 1, status: "active" },
        undefined,
        {} as never,
      ),
    );
    expect(result.errors.name).toMatchObject({ type: "minLength" });
    expect(result.errors.name?.message).toBeTruthy();
  });

  it("类型错误映射到对应字段", async () => {
    const result = await settle(
      ajvResolver(schema)(
        { name: "项目", category_id: "not-a-number", status: "active" },
        undefined,
        {} as never,
      ),
    );
    expect(result.errors.category_id).toMatchObject({ type: "type" });
  });
});

describe("mapAjvErrors", () => {
  it("嵌套路径拍平为点路径，同一字段只保留首个错误", () => {
    const mapped = mapAjvErrors([
      {
        keyword: "type",
        instancePath: "/data/name",
        schemaPath: "",
        params: {},
        message: "must be string",
      },
      {
        keyword: "minLength",
        instancePath: "/data/name",
        schemaPath: "",
        params: {},
        message: "too short",
      },
    ]);
    expect(Object.keys(mapped)).toEqual(["data.name"]);
  });
});

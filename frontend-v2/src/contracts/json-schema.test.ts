import { describe, expect, it } from "vitest";
import { initialObject } from "./json-schema";

describe("initialObject", () => {
  it("不为未填写的可选字段伪造业务值", () => {
    const values = initialObject({
      type: "object",
      properties: {
        enabled: { type: "boolean" },
        roles: { type: "array", items: { type: "string" } },
        mode: { type: "string", enum: ["safe", "fast"] },
        title: { type: "string" },
      },
    });

    expect(values).toEqual({});
    expect(JSON.parse(JSON.stringify(values))).toEqual({});
  });

  it("只采用契约明确声明的默认值并保留显式空值", () => {
    expect(
      initialObject({
        type: "object",
        properties: {
          enabled: { type: "boolean", default: false },
          roles: { type: "array", default: [] },
          mode: { type: "string", enum: ["safe", "fast"], default: "fast" },
          nested: {
            type: "object",
            default: { retries: 0 },
            properties: {
              retries: { type: "integer", default: 0 },
              note: { type: "string" },
            },
          },
        },
      }),
    ).toEqual({
      enabled: false,
      roles: [],
      mode: "fast",
      nested: { retries: 0 },
    });
  });

  it("不因可选对象内部存在默认值而伪造父对象", () => {
    expect(
      initialObject({
        type: "object",
        properties: {
          optional: {
            type: "object",
            properties: { enabled: { type: "boolean", default: true } },
          },
        },
      }),
    ).toEqual({});
  });
});

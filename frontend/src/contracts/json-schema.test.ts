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

    expect(values).toEqual({
      enabled: undefined,
      roles: undefined,
      mode: undefined,
      title: undefined,
    });
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
      nested: { retries: 0, note: undefined },
    });
  });
});

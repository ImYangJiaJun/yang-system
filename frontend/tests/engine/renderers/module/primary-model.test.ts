import { describe, expect, it } from "vitest";

import type { ActionDemoSchema } from "@/engine/contracts/ui-catalog";
import {
  buildPrimaryActionValues,
  outputProperties,
  schemaColumn,
} from "@/engine/renderers/module/primary-model";

const action: ActionDemoSchema = {
  operation_id: "demo.items.list",
  title: "列表",
  description: "",
  method: "POST",
  path: "/api/v1/demo/items/query",
  params: [],
  input_schema: {
    type: "object",
    properties: {
      page: { type: "integer" },
      limit: { type: "integer" },
      search: { anyOf: [{ type: "string" }, { type: "null" }] },
    },
  },
  output_schema: {
    type: "object",
    properties: {
      items: {
        type: "array",
        items: {
          type: "object",
          properties: {
            id: { type: "integer", title: "ID" },
            created_at: { type: "string", format: "date-time" },
            payload: { type: "object" },
          },
        },
      },
    },
  },
  request_media_type: "json",
  response_kind: "json",
  requires_auth: false,
};

describe("primaryAction 回退模型", () => {
  it("请求参数只下发输入契约声明的 page/limit/search", () => {
    expect(
      buildPrimaryActionValues(action, {
        page: 2,
        pageSize: 20,
        search: " x ",
      }),
    ).toEqual({ page: 2, limit: 20, search: "x" });
    expect(
      buildPrimaryActionValues(action, { page: 1, pageSize: 20, search: " " }),
    ).toEqual({ page: 1, limit: 20 });

    const minimal: ActionDemoSchema = { ...action, input_schema: {} };
    expect(
      buildPrimaryActionValues(minimal, { page: 9, pageSize: 20, search: "x" }),
    ).toEqual({});
  });

  it("从输出契约定位行 items 的属性集合", () => {
    expect(Object.keys(outputProperties(action, true))).toEqual([
      "id",
      "created_at",
      "payload",
    ]);
  });

  it("schemaColumn 按类型与格式推导展示种类", () => {
    expect(schemaColumn("id", { type: "integer", title: "ID" })).toMatchObject({
      title: "ID",
      widget: "integer",
      display: { kind: "number" },
    });
    expect(
      schemaColumn("created_at", { type: "string", format: "date-time" }),
    ).toMatchObject({ widget: "date_time", display: { kind: "date_time" } });
    expect(schemaColumn("payload", { type: "object" })).toMatchObject({
      display: { kind: "json" },
    });
    expect(schemaColumn("other", undefined)).toMatchObject({
      title: "other",
      display: { kind: "text" },
    });
  });
});

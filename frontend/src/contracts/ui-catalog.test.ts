import { describe, expect, it } from "vitest";
import { ContractError, parseUiCatalog } from "./ui-catalog";

const action = {
  operation_id: "demo.echo",
  title: "回显",
  description: "回显输入",
  method: "POST",
  path: "/api/demo/echo",
  params: [
    {
      name: "message",
      source: "body",
      required: true,
      title: "消息",
      description: "",
    },
  ],
  input_schema: { type: "object", properties: { message: { type: "string" } } },
  output_schema: { type: "object" },
  request_media_type: "json",
  response_kind: "json",
  requires_auth: false,
};

function envelope(overrides: Record<string, unknown> = {}) {
  return {
    code: 0,
    message: "成功",
    data: {
      schema_version: "2.2",
      revision: "a".repeat(64),
      actions: [action],
      table_views: [],
      ...overrides,
    },
  };
}

describe("parseUiCatalog", () => {
  it("接受当前版本并保留 Action 契约", () => {
    const catalog = parseUiCatalog(envelope());
    expect(catalog.actions[0]?.operation_id).toBe("demo.echo");
  });

  it("对未知响应枚举安全降级为 json", () => {
    const payload = envelope({
      actions: [{ ...action, response_kind: "future-stream" }],
    });
    expect(parseUiCatalog(payload).actions[0]?.response_kind).toBe("json");
  });

  it("拒绝未知 schema 版本并返回可诊断错误", () => {
    expect(() => parseUiCatalog(envelope({ schema_version: "99.0" }))).toThrow(
      ContractError,
    );
  });

  it("拒绝成功但缺少 data 的响应", () => {
    expect(() => parseUiCatalog({ code: 0, message: "成功" })).toThrow(
      "缺少 data",
    );
  });

  it("TableView 必须显式声明数据 Action", () => {
    const tableView = {
      view_id: "demo.items.main",
      title: "项目",
      table: "demo_items",
      columns: [],
      form: { fields: [] },
      query: {
        search_fields: [],
        filter_fields: [],
        default_sort: [],
        default_page_size: 20,
        max_page_size: 100,
      },
      actions: [],
      action_presentations: [],
    };
    expect(() =>
      parseUiCatalog(envelope({ table_views: [tableView] })),
    ).toThrow(ContractError);
    const catalog = parseUiCatalog(
      envelope({
        table_views: [{ ...tableView, data_action: "demo.items.select" }],
      }),
    );
    expect(catalog.table_views[0]?.data_action).toBe("demo.items.select");
  });
});

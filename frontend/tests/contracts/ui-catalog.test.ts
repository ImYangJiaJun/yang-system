import { describe, expect, it } from "vitest";
import { ContractError, parseUiCatalog } from "@/contracts/ui-catalog";

const action = {
  operation_id: "demo.echo",
  title: "回显",
  description: "回显输入",
  method: "POST",
  path: "/api/v1/demo/echo",
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
      modules: [],
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

  it("拒绝未知 Action interaction，禁止降级为 invoke 执行", () => {
    const tableView = {
      view_id: "demo.items.main",
      title: "项目",
      table: "demo_items",
      data_action: "demo.items.select",
      columns: [],
      form: { fields: [] },
      query: {
        search_fields: [],
        filter_fields: [],
        default_sort: [],
        default_page_size: 20,
        max_page_size: 100,
      },
      actions: ["demo.echo"],
      action_presentations: [
        {
          operation_id: "demo.echo",
          title: "未来交互",
          placement: "toolbar",
          interaction: "future-interaction",
        },
      ],
    };

    expect(() =>
      parseUiCatalog(envelope({ table_views: [tableView] })),
    ).toThrow(ContractError);
  });

  it("接受 2.3 表格展示、过滤和操作语义且不接收 CSS", () => {
    const catalog = parseUiCatalog(
      envelope({
        schema_version: "2.3",
        table_views: [
          {
            view_id: "demo.items.main",
            title: "项目",
            table: "demo_items",
            data_action: "demo.echo",
            columns: [
              {
                field: "status",
                title: "状态",
                description: "项目状态",
                widget: "radio",
                required: true,
                searchable: false,
                filterable: true,
                sortable: true,
                display: {
                  kind: "status",
                  align: "center",
                  width: 120,
                  options: [
                    { value: "active", label: "启用", tone: "positive" },
                  ],
                  class: "unsafe-backend-css",
                },
                filter: {
                  operators: ["eq", "in"],
                  default_operator: "eq",
                  widget: "radio",
                },
              },
            ],
            form: { fields: [] },
            query: {
              search_fields: [],
              filter_fields: ["status"],
              default_sort: [],
              default_page_size: 20,
              max_page_size: 100,
            },
            actions: ["demo.echo"],
            action_presentations: [
              {
                operation_id: "demo.echo",
                title: "删除",
                placement: "row",
                interaction: "invoke",
                appearance: {
                  emphasis: "danger",
                  icon: "delete",
                  order: 20,
                  overflow: true,
                },
              },
            ],
          },
        ],
      }),
    );

    const column = catalog.table_views[0]?.columns[0];
    expect(column?.display).toEqual({
      kind: "status",
      align: "center",
      width: 120,
      options: [{ value: "active", label: "启用", tone: "positive" }],
    });
    expect(column?.filter?.operators).toEqual(["eq", "in"]);
    expect(
      catalog.table_views[0]?.action_presentations[0]?.appearance,
    ).toMatchObject({ emphasis: "danger", overflow: true });
  });

  it("拒绝未声明或矛盾的过滤操作符", () => {
    const baseColumn = {
      field: "name",
      title: "名称",
      description: "",
      widget: "text",
      required: false,
      searchable: true,
      filterable: true,
      sortable: true,
    };
    const baseView = {
      view_id: "demo.items.main",
      title: "项目",
      table: "demo_items",
      data_action: "demo.echo",
      form: { fields: [] },
      query: {
        search_fields: ["name"],
        filter_fields: ["name"],
        default_sort: [],
        default_page_size: 20,
        max_page_size: 100,
      },
      actions: [],
      action_presentations: [],
    };

    expect(() =>
      parseUiCatalog(
        envelope({
          schema_version: "2.3",
          table_views: [
            {
              ...baseView,
              columns: [
                {
                  ...baseColumn,
                  filter: {
                    operators: ["eq"],
                    default_operator: "contains",
                  },
                },
              ],
            },
          ],
        }),
      ),
    ).toThrow(ContractError);

    expect(() =>
      parseUiCatalog(
        envelope({
          schema_version: "2.3",
          table_views: [
            {
              ...baseView,
              columns: [
                {
                  ...baseColumn,
                  filter: {
                    operators: ["unsafe-sql"],
                    default_operator: "unsafe-sql",
                  },
                },
              ],
            },
          ],
        }),
      ),
    ).toThrow(ContractError);
  });
});

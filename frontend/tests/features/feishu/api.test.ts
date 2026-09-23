import { afterEach, describe, expect, it, vi } from "vitest";

import type { ActionDemoSchema, UiCatalog } from "@/engine";
import {
  buildDatasourceListBody,
  createDatasource,
  createDatasourceTable,
  deleteDatasource,
  feishuQueryKeys,
  listBitableFields,
  listBitableTables,
  listBitableViews,
  listDatasources,
  listOptions,
  precheckApprovalOptions,
  requireAction,
  updateDatasource,
  type FeishuInvokeDeps,
} from "@/features/feishu/api";
import { DEFAULT_OPTION_ORDER_BY } from "@/features/feishu/api";
import type { DatasourceListQuery } from "@/features/feishu/types";

/// 飞书数据源 API 契约：query key 形状、请求体逐字对齐后端、缺 operation_id 时抛错。

const DATASOURCE_LIST_ACTION: ActionDemoSchema = {
  operation_id: "feishu.datasource.list_datasources",
  title: "数据源列表",
  description: "",
  method: "POST",
  path: "/api/v1/feishu/datasources/query",
  params: [],
  input_schema: {},
  output_schema: {},
  request_media_type: "json",
  response_kind: "json",
  requires_auth: true,
};

const DEPLOYED_ACTIONS: ActionDemoSchema[] = [
  DATASOURCE_LIST_ACTION,
  {
    ...DATASOURCE_LIST_ACTION,
    operation_id: "feishu.datasource.create_datasource",
    path: "/api/v1/feishu/datasources",
  },
  {
    ...DATASOURCE_LIST_ACTION,
    operation_id: "feishu.datasource.update_datasource",
    method: "PUT",
  },
  {
    ...DATASOURCE_LIST_ACTION,
    operation_id: "feishu.datasource.delete_datasource",
    method: "DELETE",
  },
  {
    ...DATASOURCE_LIST_ACTION,
    operation_id: "feishu.option.list_options",
    path: "/api/v1/feishu/options/query",
  },
  {
    ...DATASOURCE_LIST_ACTION,
    operation_id: "feishu.option.approval_options",
    path: "/api/v1/feishu/approval/options/{source_key}",
    requires_auth: false,
  },
];

function catalogWith(
  actions: ActionDemoSchema[] = DEPLOYED_ACTIONS,
): UiCatalog {
  return {
    schema_version: "2.3",
    revision: "a".repeat(64),
    actions,
    table_views: [],
    modules: [],
  };
}

const deps: FeishuInvokeDeps = {
  catalog: catalogWith(),
  session: { token: "tok-1" },
};

function query(
  overrides: Partial<DatasourceListQuery> = {},
): DatasourceListQuery {
  return {
    page: 1,
    pageSize: 10,
    search: "",
    status: "all",
    orderBy: [{ field: "source_key", direction: "Asc" }],
    ...overrides,
  };
}

function jsonResponse(payload: unknown, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

/// stub fetch，并把每次请求的 url + body 记下来。
function stubFetch(payload: unknown, status = 200) {
  const calls: Array<{ url: string; method: string; body: unknown }> = [];
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.toString();
      calls.push({
        url,
        method: init?.method ?? "GET",
        body: init?.body ? JSON.parse(String(init.body)) : undefined,
      });
      return Promise.resolve(jsonResponse(payload, status));
    }),
  );
  return calls;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("query key 工厂", () => {
  it("首段是 feishu，列表键带查询形状", () => {
    const key = feishuQueryKeys.datasourceList(query({ page: 2 }));
    expect(key[0]).toBe("feishu");
    expect(key[1]).toBe("datasources");
    expect(key[2]).toEqual({
      page: 2,
      pageSize: 10,
      search: "",
      status: "all",
      orderBy: [{ field: "source_key", direction: "Asc" }],
    });
  });

  it("选项键带上数据源标识", () => {
    const key = feishuQueryKeys.optionList({
      sourceKey: "expense_category",
      page: 1,
      pageSize: 10,
      orderBy: [],
    });
    expect(key).toEqual([
      "feishu",
      "options",
      "expense_category",
      { page: 1, pageSize: 10, orderBy: DEFAULT_OPTION_ORDER_BY },
    ]);
  });

  it("搜索词在键里被 trim，且排序已补确定性收尾键", () => {
    const key = feishuQueryKeys.datasourceList(
      query({
        search: "  北京  ",
        orderBy: [{ field: "title", direction: "Asc" }],
      }),
    );
    expect(key[2]).toMatchObject({
      search: "北京",
      orderBy: [
        { field: "title", direction: "Asc" },
        { field: "source_key", direction: "Asc" },
      ],
    });
  });
});

describe("requireAction", () => {
  it("目录里没有该 operation_id 时抛明确错误", () => {
    expect(() =>
      requireAction(catalogWith([]), "feishu.datasource.list_datasources"),
    ).toThrow(/找不到 Action「feishu\.datasource\.list_datasources」/);
  });

  it("目录尚未加载（undefined）同样抛错，而不是静默发请求", () => {
    expect(() =>
      requireAction(undefined, "feishu.datasource.list_datasources"),
    ).toThrow(/找不到 Action/);
  });
});

describe("buildDatasourceListBody", () => {
  it("order_by 恒非空：调用方给空数组也要兜住 source_key Asc", () => {
    expect(buildDatasourceListBody(query({ orderBy: [] }))).toMatchObject({
      order_by: [{ field: "source_key", direction: "Asc" }],
    });
  });

  it("方向是 PascalCase，不是小写", () => {
    const body = buildDatasourceListBody(
      query({ orderBy: [{ field: "title", direction: "Desc" }] }),
    );
    expect(body.order_by).toEqual([
      { field: "title", direction: "Desc" },
      { field: "source_key", direction: "Asc" },
    ]);
  });

  it("空搜索与「全部」筛选被省略（deny_unknown_fields 下不能多传键）", () => {
    const body = buildDatasourceListBody(query());
    expect(body.search).toBeUndefined();
    expect(body.where).toBeUndefined();
    expect(body.count_total).toBe(true);
  });

  it("状态筛选发成 where 等值条件", () => {
    expect(
      buildDatasourceListBody(query({ status: "disabled" })).where,
    ).toEqual({
      type: "eq",
      field: "status",
      value: "disabled",
    });
  });
});

describe("listDatasources", () => {
  it("打到 /feishu/datasources/query，并把响应投影成前端形状", async () => {
    const calls = stubFetch({
      code: 0,
      message: "查询成功",
      data: {
        items: [
          {
            source_key: "expense_category",
            title: "费用类型",
            encrypt_enabled: true,
            default_locale: "zh_cn",
            status: "disabled",
            updated_at: 1758000000,
          },
        ],
        page: 1,
        page_size: 10,
        total: 1,
      },
    });

    const page = await listDatasources(query(), deps);
    expect(calls[0]?.url).toBe("/api/v1/feishu/datasources/query");
    expect(calls[0]?.method).toBe("POST");
    expect(page.total).toBe(1);
    expect(page.items).toEqual([
      {
        sourceKey: "expense_category",
        title: "费用类型",
        encryptEnabled: true,
        defaultLocale: "zh_cn",
        status: "disabled",
        updatedAt: 1758000000,
        // 取数配置与同步状态：fixture 里没有这些键（模拟一个只按 push 用的存量
        // 数据源），所以全部落到「空」而不是被猜成某个值。
        ingestMode: "",
        bitableBaseToken: null,
        bitableTableId: null,
        bitableViewId: null,
        bitableFieldName: null,
        linkageMapping: null,
        lastPullAt: null,
        lastSuccessAt: null,
        consecutiveFailures: 0,
        lastError: null,
        snapshotDigest: null,
      },
    ]);
  });

  it("每次请求都带非空 order_by（不发就是无序分页）", async () => {
    const calls = stubFetch({
      code: 0,
      data: { items: [], page: 1, page_size: 10, total: 0 },
    });
    await listDatasources(query({ orderBy: [] }), deps);
    const body = calls[0]?.body as { order_by?: unknown[] };
    expect(body.order_by?.length).toBeGreaterThan(0);
  });

  it("没请求 count_total 时 total 投影为 null（不编造 0）", async () => {
    stubFetch({ code: 0, data: { items: [], page: 1, page_size: 10 } });
    const page = await listDatasources(query(), deps);
    expect(page.total).toBeNull();
  });

  it("服务端拒绝类错误直接透传后端原文", async () => {
    stubFetch({ code: 400001, message: "page/page_size 越界" }, 400);
    await expect(listDatasources(query(), deps)).rejects.toThrow(
      "page/page_size 越界",
    );
  });
});

describe("listOptions", () => {
  it("source_key 发在顶层，且默认按「最近推送」倒序", async () => {
    const calls = stubFetch({
      code: 0,
      data: { items: [], page: 1, page_size: 10, total: 0 },
    });
    await listOptions(
      { sourceKey: "expense_category", page: 1, pageSize: 10, orderBy: [] },
      deps,
    );
    expect(calls[0]?.url).toBe("/api/v1/feishu/options/query");
    expect(calls[0]?.body).toMatchObject({
      source_key: "expense_category",
      count_total: true,
      order_by: [
        { field: "updated_at", direction: "Desc" },
        { field: "option_id", direction: "Asc" },
      ],
    });
  });

  it("i18n 是 JSON 文本、可能是 null，原样投影不解析", async () => {
    stubFetch({
      code: 0,
      data: {
        items: [
          {
            option_id: "travel",
            source_key: "expense_category",
            label: "差旅费",
            i18n: '{"en_us":"Travel"}',
            sort_order: 1,
            is_default: true,
            enabled: true,
            updated_at: 1758000000,
          },
          {
            option_id: "meal",
            source_key: "expense_category",
            label: "餐费",
            i18n: null,
            sort_order: 2,
            enabled: false,
            updated_at: 0,
          },
        ],
        page: 1,
        page_size: 10,
        total: 2,
      },
    });
    const page = await listOptions(
      { sourceKey: "expense_category", page: 1, pageSize: 10, orderBy: [] },
      deps,
    );
    expect(page.items[0]?.i18n).toBe('{"en_us":"Travel"}');
    expect(page.items[1]?.i18n).toBeNull();
    expect(page.items[1]?.isDefault).toBe(false);
    expect(page.items[1]?.enabled).toBe(false);
  });
});

describe("createDatasource", () => {
  it("提交 source_key/title/token/encrypt_enabled/default_locale", async () => {
    const calls = stubFetch({ code: 0, data: { source_key: "dept_sales" } });
    const result = await createDatasource(
      {
        sourceKey: "dept_sales",
        title: "部门",
        token: "t-1",
        encryptEnabled: true,
        defaultLocale: "en_us",
      },
      deps,
    );
    expect(calls[0]?.url).toBe("/api/v1/feishu/datasources");
    expect(calls[0]?.body).toEqual({
      source_key: "dept_sales",
      title: "部门",
      token: "t-1",
      encrypt_enabled: true,
      default_locale: "en_us",
    });
    expect(result).toEqual({ sourceKey: "dept_sales" });
  });

  it("响应缺 source_key 时拒绝，不返回一个空标识", async () => {
    stubFetch({ code: 0, data: {} });
    await expect(
      createDatasource(
        {
          sourceKey: "dept_sales",
          title: "部门",
          token: "t-1",
          encryptEnabled: false,
          defaultLocale: "zh_cn",
        },
        deps,
      ),
    ).rejects.toThrow(/没有 source_key/);
  });
});

describe("updateDatasource", () => {
  it("只改名称时，未给的字段一个都不出现（省略 = 保持原值）", async () => {
    const calls = stubFetch({ code: 0, data: { affected: 1 } });
    await updateDatasource({ sourceKey: "dept_sales", title: "新名称" }, deps);
    expect(calls[0]?.method).toBe("PUT");
    expect(calls[0]?.body).toEqual({
      source_key: "dept_sales",
      title: "新名称",
    });
  });

  it("空白 Token 按「不轮换」处理，不会发出空串", async () => {
    const calls = stubFetch({ code: 0, data: { affected: 1 } });
    await updateDatasource(
      { sourceKey: "dept_sales", title: "新名称", token: "   " },
      deps,
    );
    expect(calls[0]?.body).not.toHaveProperty("token");
  });

  it("给了 Token 时原样发出（轮换）", async () => {
    const calls = stubFetch({ code: 0, data: { affected: 1 } });
    await updateDatasource({ sourceKey: "dept_sales", token: "t-2" }, deps);
    expect(calls[0]?.body).toMatchObject({ token: "t-2" });
  });

  it("状态改成 disabled 时只发 status", async () => {
    const calls = stubFetch({ code: 0, data: { affected: 1 } });
    await updateDatasource(
      { sourceKey: "dept_sales", status: "disabled" },
      deps,
    );
    expect(calls[0]?.body).toEqual({
      source_key: "dept_sales",
      status: "disabled",
    });
  });
});

describe("deleteDatasource", () => {
  it("DELETE 且只带 source_key，回执含被连带停用的选项数", async () => {
    const calls = stubFetch({
      code: 0,
      data: { deleted: 1, disabled_options: 3 },
    });
    const result = await deleteDatasource("dept_sales", deps);
    expect(calls[0]?.method).toBe("DELETE");
    expect(calls[0]?.body).toEqual({ source_key: "dept_sales" });
    expect(result).toEqual({ deleted: 1, disabledOptions: 3 });
  });
});

/* ---------------------- 表级配置：元数据与创建端点 ---------------------- */

/// 这四个端点的契约由后端 T3/T4/T5 落地；这里钉的是**前端发出去的形状**
/// （路径、权限位对应的 operation_id、请求体的键名）。
const TABLE_CONFIG_ACTIONS: ActionDemoSchema[] = [
  {
    ...DATASOURCE_LIST_ACTION,
    operation_id: "feishu.datasource.list_bitable_tables",
    path: "/api/v1/feishu/datasources/bitable-tables",
  },
  {
    ...DATASOURCE_LIST_ACTION,
    operation_id: "feishu.datasource.list_bitable_views",
    path: "/api/v1/feishu/datasources/bitable-views",
  },
  {
    ...DATASOURCE_LIST_ACTION,
    operation_id: "feishu.datasource.list_bitable_fields",
    path: "/api/v1/feishu/datasources/bitable-fields",
  },
  {
    ...DATASOURCE_LIST_ACTION,
    operation_id: "feishu.datasource.create_datasource_table",
    path: "/api/v1/feishu/datasources/table",
  },
];

const tableDeps: FeishuInvokeDeps = {
  catalog: catalogWith(TABLE_CONFIG_ACTIONS),
  session: { token: "tok-1" },
};

describe("表级配置的元数据端点", () => {
  it("listBitableTables：POST + app_token 走请求体，响应投影成驼峰", async () => {
    const calls = stubFetch({
      code: 0,
      data: { tables: [{ table_id: "tblA", name: "目标台账" }] },
    });
    const tables = await listBitableTables("app1", tableDeps);

    expect(calls[0]?.url).toBe("/api/v1/feishu/datasources/bitable-tables");
    expect(calls[0]?.method).toBe("POST");
    expect(calls[0]?.body).toEqual({ app_token: "app1" });
    expect(tables).toEqual([{ tableId: "tblA", name: "目标台账" }]);
  });

  it("listBitableViews：两个坐标都发，且带出 view_type", async () => {
    const calls = stubFetch({
      code: 0,
      data: {
        views: [{ view_id: "vew1", view_name: "全部记录", view_type: "grid" }],
      },
    });
    const views = await listBitableViews("app1", "tblA", tableDeps);

    expect(calls[0]?.url).toBe("/api/v1/feishu/datasources/bitable-views");
    expect(calls[0]?.body).toEqual({ app_token: "app1", table_id: "tblA" });
    expect(views).toEqual([
      { viewId: "vew1", viewName: "全部记录", viewType: "grid" },
    ]);
  });

  it("listBitableFields：不发 view_id（实测那个参数对列出字段不生效）", async () => {
    const calls = stubFetch({
      code: 0,
      data: {
        fields: [{ field_id: "fldA", field_name: "币种", type: 3 }],
      },
    });
    const fields = await listBitableFields("app1", "tblA", tableDeps);

    expect(calls[0]?.url).toBe("/api/v1/feishu/datasources/bitable-fields");
    expect(calls[0]?.body).toEqual({ app_token: "app1", table_id: "tblA" });
    expect(calls[0]?.body).not.toHaveProperty("view_id");
    expect(fields).toEqual([{ fieldId: "fldA", fieldName: "币种", type: 3 }]);
  });

  it("createDatasourceTable：勾选集合原样发出，父指针是 field_id", async () => {
    const calls = stubFetch({
      code: 0,
      data: {
        datasource_id: 7,
        credentials: [
          { field_id: "fldA", source_key: "payment_currency", token: "t-1" },
        ],
      },
    });
    const result = await createDatasourceTable(
      {
        title: "公司往来付款",
        appToken: "app1",
        tableId: "tblA",
        viewId: "vew1",
        fields: [
          {
            fieldId: "fldA",
            fieldName: "币种/Currency",
            type: 3,
            sourceKey: "payment_currency",
            parentFieldId: null,
          },
          {
            fieldId: "fldB",
            fieldName: "汇率/Exchange Rate",
            type: 2,
            sourceKey: "payment_fx_rate",
            parentFieldId: "fldA",
          },
        ],
      },
      tableDeps,
    );

    expect(calls[0]?.url).toBe("/api/v1/feishu/datasources/table");
    expect(calls[0]?.body).toEqual({
      title: "公司往来付款",
      ingest_mode: "pull",
      bitable_base_token: "app1",
      bitable_table_id: "tblA",
      bitable_view_id: "vew1",
      fields: [
        {
          field_id: "fldA",
          source_key: "payment_currency",
          parent_field_id: null,
        },
        {
          field_id: "fldB",
          source_key: "payment_fx_rate",
          parent_field_id: "fldA",
        },
      ],
    });
    expect(result.datasourceId).toBe(7);
    expect(result.credentials).toEqual([
      { fieldId: "fldA", sourceKey: "payment_currency", token: "t-1" },
    ]);
  });

  it("视图留空表示取全表：按键整个不发，而不是发空串", async () => {
    const calls = stubFetch({ code: 0, data: { datasource_id: 7 } });
    await createDatasourceTable(
      {
        title: "公司往来付款",
        appToken: "app1",
        tableId: "tblA",
        viewId: "",
        fields: [
          {
            fieldId: "fldA",
            fieldName: "币种",
            type: 3,
            sourceKey: "currency",
            parentFieldId: null,
          },
        ],
      },
      tableDeps,
    );
    expect(calls[0]?.body).not.toHaveProperty("bitable_view_id");
  });

  it("目录里没有这条 Action 时抛错，且一个请求都不发", async () => {
    const calls = stubFetch({ code: 0, data: { tables: [] } });
    await expect(listBitableTables("app1", deps)).rejects.toThrow(
      /找不到 Action「feishu\.datasource\.list_bitable_tables」/,
    );
    expect(calls).toHaveLength(0);
  });
});

describe("precheckApprovalOptions", () => {
  it("路径里的 source_key 被填进 URL，明文 Token 走请求体", async () => {
    const calls = stubFetch({
      code: 0,
      msg: "success!",
      data: { result: { options: [{ id: "a" }, { id: "b" }] } },
    });
    const result = await precheckApprovalOptions("dept_sales", "t-1", deps);
    expect(calls[0]?.url).toBe("/api/v1/feishu/approval/options/dept_sales");
    expect(calls[0]?.body).toEqual({ token: "t-1" });
    expect(result).toEqual({
      status: "ok",
      optionCount: 2,
      hasMore: false,
      encrypted: false,
    });
  });

  it("nextPageToken 非空 = 还有下一页：本页条数不是总数", async () => {
    // 单页上限 100。100 条 + 有下一页时，界面的「100」只是这一页。
    stubFetch({
      code: 0,
      msg: "success!",
      data: {
        result: {
          options: Array.from({ length: 100 }, (_, index) => ({
            id: `opt_${index}`,
          })),
          hasMore: true,
          nextPageToken: "cursor-1",
        },
      },
    });
    const result = await precheckApprovalOptions("dept_sales", "t-1", deps);
    expect(result).toEqual({
      status: "ok",
      optionCount: 100,
      hasMore: true,
      encrypted: false,
    });
  });

  it("nextPageToken 为 null（没有下一页）时不去猜成「还有更多」", async () => {
    stubFetch({
      code: 0,
      msg: "success!",
      data: {
        result: {
          options: [{ id: "a" }],
          hasMore: false,
          nextPageToken: null,
        },
      },
    });
    const result = await precheckApprovalOptions("dept_sales", "t-1", deps);
    if (result.status !== "ok" || result.encrypted) {
      throw new Error("应为明文成功回执");
    }
    expect(result.hasMore).toBe(false);
  });

  it("开了「加密返回」时拿到的是密文：连通但读不出条数", async () => {
    stubFetch({ code: 0, msg: "success!", data: { result: "ZW5jcnlwdGVk" } });
    const result = await precheckApprovalOptions("dept_sales", "t-1", deps);
    expect(result).toEqual({
      status: "ok",
      optionCount: null,
      encrypted: true,
    });
  });

  it("Token 不匹配时给出可归因的失败回执（读信封的 msg 原文）", async () => {
    stubFetch({ code: 40102, msg: "token 校验失败", data: null });
    const result = await precheckApprovalOptions("dept_sales", "wrong", deps);
    expect(result.status).toBe("failed");
    if (result.status !== "failed") throw new Error("应为失败回执");
    expect(result.code).toBe(40102);
    expect(result.message).toBe("token 校验失败");
    expect(result.hint).toContain("重填一次 Token");
    expect(result.verdict).toBe(
      "Token 没对：服务端存的摘要与这次传进来的不一致。",
    );
  });

  it("数据源已停用（40301）也照实回显码与含义", async () => {
    stubFetch({ code: 40301, msg: "数据源已停用", data: null });
    const result = await precheckApprovalOptions("dept_sales", "t-1", deps);
    if (result.status !== "failed") throw new Error("应为失败回执");
    expect(result.code).toBe(40301);
    expect(result.message).toBe("数据源已停用");
    expect(result.hint).toContain("停用");
    expect(result.verdict).toContain("Token 已经通过了比对");
  });

  it("失败结论按码区分：Token 是对的几种失败不说成凭据没过", async () => {
    // 服务端的判定顺序是「查数据源 → 比对 Token → 看状态」：
    // 40401 / 40301 / 50002 都没有「Token 没通过」这一层含义。
    const cases: Array<[number, string, RegExp]> = [
      [40401, "数据源不存在", /Token 对不对还无从谈起/],
      [40301, "数据源已停用", /Token 已经通过了比对/],
      [50002, "服务端未配置加密密钥", /Token 已经通过了比对/],
      [40102, "token 校验失败", /Token 没对/],
    ];

    for (const [code, msg, expected] of cases) {
      stubFetch({ code, msg, data: null });
      const result = await precheckApprovalOptions("dept_sales", "t-1", deps);
      if (result.status !== "failed") throw new Error("应为失败回执");
      expect(result.code).toBe(code);
      expect(result.verdict).toMatch(expected);
      expect(result.verdict).not.toMatch(/没有通过验证/);
      // 每个码都有自己的结论，不是同一句话
      expect(result.verdict).not.toBe("");
    }
  });

  it("目录里没有这条 Action 也能预检（它是 public 端点，用兜底契约）", async () => {
    const calls = stubFetch({
      code: 0,
      msg: "success!",
      data: { result: { options: [] } },
    });
    const result = await precheckApprovalOptions("dept_sales", "t-1", {
      catalog: catalogWith([]),
      session: { token: "tok-1" },
    });
    expect(calls[0]?.url).toBe("/api/v1/feishu/approval/options/dept_sales");
    expect(result.status).toBe("ok");
  });
});

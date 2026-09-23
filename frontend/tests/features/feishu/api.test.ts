import { afterEach, describe, expect, it, vi } from "vitest";

import type { ActionDemoSchema, UiCatalog } from "@/engine";
import {
  DATASOURCE_OPERATION_IDS,
  canWriteDatasources,
  buildDatasourceListBody,
  DATASOURCE_ITEM_KEYS,
  FIELD_BINDING_KEYS,
  HEALTH_MISSING_FIELD_KEYS,
  HEALTH_REPORT_KEYS,
  OPTION_ITEM_KEYS,
  PULL_SCHEDULE_KEYS,
  checkDatasourceHealth,
  fetchPullSchedule,
  createDatasourceTable,
  deleteDatasourceTable,
  enabledBindingInputs,
  feishuQueryKeys,
  listBitableFields,
  listBitableTables,
  listBitableViews,
  listDatasources,
  listOptions,
  precheckApprovalOptions,
  requireAction,
  revealFieldToken,
  rotateFieldToken,
  updateDatasourceTable,
  pullNow,
  type FeishuInvokeDeps,
} from "@/features/feishu/api";
import { DEFAULT_OPTION_ORDER_BY } from "@/features/feishu/api";
import type { DatasourceListQuery } from "@/features/feishu/types";

import projections from "../../../contracts/feishu-projections.json";
import {
  DEFAULT_LOCALE_OPTIONS,
  INGEST_MODE_OPTIONS,
  hasCompleteCoordinates,
  syncHealth,
} from "@/features/feishu/types";
import { STATUS_OPTIONS } from "@/features/feishu/components/ListToolbar";

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

/// 部署里真实存在的那一套（字段级 `create_datasource` / `update_datasource` /
/// `delete_datasource` 已在 T13 退役，这里一个都不留——替身照着事实搭，
/// 否则「界面上那个入口发得出去没有」这件事就被测反了）。
const DEPLOYED_ACTIONS: ActionDemoSchema[] = [
  DATASOURCE_LIST_ACTION,
  {
    ...DATASOURCE_LIST_ACTION,
    operation_id: "feishu.datasource.create_datasource_table",
    path: "/api/v1/feishu/datasources/table",
  },
  {
    ...DATASOURCE_LIST_ACTION,
    operation_id: "feishu.datasource.update_datasource_table",
    path: "/api/v1/feishu/datasources/table",
    method: "PUT",
  },
  {
    ...DATASOURCE_LIST_ACTION,
    operation_id: "feishu.datasource.delete_datasource_table",
    path: "/api/v1/feishu/datasources/table",
    method: "DELETE",
  },
  {
    ...DATASOURCE_LIST_ACTION,
    operation_id: "feishu.datasource.pull_now",
    path: "/api/v1/feishu/datasources/pull-now",
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
    orderBy: [{ field: "title", direction: "Asc" }],
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
      // 收尾键是表级行的真唯一键 `id`（`source_key` 已不在表级行上）
      orderBy: [
        { field: "title", direction: "Asc" },
        { field: "id", direction: "Asc" },
      ],
      // 不按主键取单条时也带上这一位（值为 null），键形状恒定。
      id: null,
    });
  });

  it("详情页的单条取值进 key——否则两条数据源会共用同一个缓存条目", () => {
    // 详情页除 `id` 外所有入参都固定（page=1 / pageSize=1 / search="" / status="all"），
    // 所以键里漏掉 `id` 的后果不是「多打一次请求」，而是**渲染错的那一条**：
    // staleTime 内从 #7 跳到 #8，页面直接复用 #7 的缓存——标题、字段绑定、
    // 凭据清单的复制/轮换目标、体检目标、以及「立即拉取」用的 id 全是 #7。
    const seven = feishuQueryKeys.datasourceList(query({ id: 7 }));
    const eight = feishuQueryKeys.datasourceList(query({ id: 8 }));
    expect(seven).not.toEqual(eight);
    expect((seven[2] as { id: number | null }).id).toBe(7);
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
        { field: "id", direction: "Asc" },
      ],
    });
  });
});

describe("写侧权限门控（canWriteDatasources）", () => {
  it("目录里只有**已落地**的表级端点时它就是真的", () => {
    // 这是 H2 的那条静默失效：常量表若还指向 T13 删掉的
    // `feishu.datasource.create_datasource`，`hasOperation` 恒为 false，
    // 于是「添加数据源」与列表项上的「⋯」菜单**永远不渲染**——
    // 界面不会报错，什么都没有，看起来跟「权限不足」一模一样。
    expect(canWriteDatasources(catalogWith(DEPLOYED_ACTIONS))).toBe(true);
  });

  it("把那个建源端点从目录里拿掉就不再有写权限（门控真的读目录）", () => {
    expect(
      canWriteDatasources(
        catalogWith(
          DEPLOYED_ACTIONS.filter(
            (action) =>
              action.operation_id !==
              "feishu.datasource.create_datasource_table",
          ),
        ),
      ),
    ).toBe(false);
  });

  it("三个写操作都按表级主键定位，不再是退役的字段级那三个", () => {
    expect(DATASOURCE_OPERATION_IDS).toMatchObject({
      create: "feishu.datasource.create_datasource_table",
      update: "feishu.datasource.update_datasource_table",
      remove: "feishu.datasource.delete_datasource_table",
      pullNow: "feishu.datasource.pull_now",
    });
    for (const id of Object.values(DATASOURCE_OPERATION_IDS)) {
      // 字段级那三个 id 正是服务端已经删掉的：任何一处留着它们都是死门控。
      expect(id).not.toMatch(/(create|update|delete)_datasource$/);
    }
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
  it("order_by 恒非空：调用方给空数组也要兜住默认排序 + 唯一键收尾", () => {
    expect(buildDatasourceListBody(query({ orderBy: [] }))).toMatchObject({
      order_by: [
        { field: "title", direction: "Asc" },
        { field: "id", direction: "Asc" },
      ],
    });
  });

  it("方向是 PascalCase，不是小写", () => {
    const body = buildDatasourceListBody(
      query({ orderBy: [{ field: "title", direction: "Desc" }] }),
    );
    expect(body.order_by).toEqual([
      { field: "title", direction: "Desc" },
      { field: "id", direction: "Asc" },
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

  it("详情页按主键取单条：发 id 等值条件，且**不发** search", () => {
    // 详情页借列表端点取「就是这一条」。它曾经用 `search: <source_key>`，那是把
    // **一条绑定的标识**当数据源的身份用：检索只覆盖 `searchable` 的列，而表级行上
    // 只有 `title` 可搜，所以那一发必然命中零行——零行又被页面读成「这条数据源不存在」。
    const body = buildDatasourceListBody(query({ id: 7 }));
    expect(body.where).toEqual({ type: "eq", field: "id", value: 7 });
    expect(body.search).toBeUndefined();
  });

  it("主键与状态并存时折成 and 组——DSL 的 where 树没有隐式合取", () => {
    const body = buildDatasourceListBody(query({ id: 7, status: "active" }));
    expect(body.where).toEqual({
      type: "and",
      conditions: [
        { type: "eq", field: "id", value: 7 },
        { type: "eq", field: "status", value: "active" },
      ],
    });
  });
});

describe("listDatasources", () => {
  it("缺 id 的行被丢掉，而不是降级成一条没有主键的数据源", async () => {
    // 表级行的身份就是 `id`，而它在 `list_datasources` 的投影里必然出现，所以
    // 「没有 id」只可能是形状认不出来。取舍与 `parseFieldBinding` 一致：**丢掉，不猜**。
    // 曾经这里把它降级成 `id: null` 的行——那种行在界面上既不能打开、也不能更新、
    // 也不能拉取，点哪儿都失效，还会占着一个「数据源」的位置。
    const calls = stubFetch({
      code: 0,
      message: "查询成功",
      data: {
        items: [
          {
            source_key: "expense_category",
            title: "费用类型",
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
    expect(page.items).toEqual([]);
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

/// **投影契约（emit ↔ read）** —— 这一整类 bug 的结构防线。
///
/// 契约文件 `frontend/contracts/feishu-projections.json` 同时被这份测试与后端
/// `list_datasources.rs` 的 `the_committed_contract_*` 读，所以「后端 emit 的键集」与
/// 「前端 read 的键集」不可能各自漂移：
///   · 前端读一个后端不发的键 ⇒ 恒得 `null`，界面把它当成「服务端说没有」；
///   · 后端发一个前端不读的键 ⇒ 死投影（补读，或去 `backend_only` 记账）。
///
/// 为什么「扫已删字段」那类检查不够：这一类的病根是**字段没被删，只是搬到了另一层**
/// （`encrypt_enabled` 在绑定表上依然是合法字段名，`linkage_mapping` 连注释都写对了、
/// 代码却还在读）。只有把两张键集摆在一起比才看得见。
/// 体检与排程平时不在 `DEPLOYED_ACTIONS` 里（控制台只在有权限 / 有 worker 时才用它们），
/// 所以这两条探针测试各自带一份目录——不动共享夹具，避免改到既有断言的前提。
const TABLE_DEPS: FeishuInvokeDeps = {
  catalog: catalogWith([
    ...DEPLOYED_ACTIONS,
    {
      ...DATASOURCE_LIST_ACTION,
      operation_id: "feishu.datasource.health_check",
      path: "/api/v1/feishu/datasources/table/health",
    },
    {
      ...DATASOURCE_LIST_ACTION,
      operation_id: "feishu.datasource.pull_schedule",
      path: "/api/v1/feishu/datasources/pull-schedule",
    },
  ]),
  session: { token: "tok-1" },
};

describe("投影契约（emit ↔ read）", () => {
  type Spec = {
    emitted: string[];
    backend_only: Record<string, string>;
    query_only?: Record<string, string>;
  };
  /// 另外两条轴也在同一个契约文件里（`client_fields` / `enums`）。
  type ClientFields = { order_by?: string[]; where?: string[] };
  type EnumDomain = { values?: string[]; enforced_by?: string };
  type Contract = Record<string, Record<string, Spec>> & {
    client_fields?: Record<string, ClientFields>;
    enums?: Record<string, Record<string, EnumDomain>>;
  };
  const CONTRACT = projections as unknown as Contract;
  /// 契约里每个「有形状的响应」都要在这里有一行：`[端点, 层, 键映射]`。
  /// 加端点时**同时**在契约文件与这里补一行——三处（Rust struct / 契约 / 前端映射）
  /// 少任何一处，下面那条集合断言就会红。
  const LEVELS = [
    ["list_datasources", "item", DATASOURCE_ITEM_KEYS],
    ["list_datasources", "binding", FIELD_BINDING_KEYS],
    ["list_options", "item", OPTION_ITEM_KEYS],
    ["health_check", "report", HEALTH_REPORT_KEYS],
    ["health_check", "missing_field", HEALTH_MISSING_FIELD_KEYS],
    ["pull_schedule", "result", PULL_SCHEDULE_KEYS],
  ] as const;

  /// 探针值：**每一个都必须与「缺键时的兜底值」不同**，否则「解析器到底读没读这个键」
  /// 根本测不出来——缺了它，这条测试会对着一个恒为默认值的字段满意地通过。
  const BINDING_PROBE: Record<string, unknown> = {
    field_id: "fldPROBE",
    field_name: "探针字段",
    source_key: "probe_key",
    parent_field_id: "fldPARENT",
    // 兜底是 true ⇒ 用 false 才测得出「读到了」
    enabled: false,
    // 兜底是 false ⇒ 用 true
    encrypt_enabled: true,
    // 兜底是 zh_cn ⇒ 换一个
    default_locale: "ja_jp",
    token_rotated_at: 1758000003,
  };
  const ITEM_PROBE: Record<string, unknown> = {
    id: 4242,
    title: "探针",
    status: "disabled",
    updated_at: 1758000000,
    ingest_mode: "pull",
    bitable_base_token: "probe_base",
    bitable_table_id: "probe_table",
    bitable_view_id: "probe_view",
    last_pull_at: 1758000001,
    last_success_at: 1758000002,
    consecutive_failures: 7,
    last_error: "探针错误",
    fields: [BINDING_PROBE],
  };

  /// 由契约的 emitted 键集**生成**线格式载荷：契约里加了键而这里忘了改，会当场报缺。
  function wireFrom(
    keys: string[],
    probe: Record<string, unknown>,
  ): Record<string, unknown> {
    const out: Record<string, unknown> = {};
    for (const key of keys) {
      expect(Object.hasOwn(probe, key), `探针表缺 ${key}`).toBe(true);
      out[key] = probe[key];
    }
    return out;
  }

  /// 由键映射**生成**期望的解析结果（键名逐条对应，不是手写一遍）。
  function expectedFrom(
    map: Record<string, string>,
    probe: Record<string, unknown>,
  ): Record<string, unknown> {
    const out: Record<string, unknown> = {};
    for (const [wire, field] of Object.entries(map)) out[field] = probe[wire];
    return out;
  }

  it("read 键集 ⊆ emitted，且 emitted 里没被读的键都在 backend_only 里记着", () => {
    for (const [endpoint, level, map] of LEVELS) {
      const spec = CONTRACT[endpoint]?.[level];
      if (spec === undefined) throw new Error(`契约里缺 ${endpoint}.${level}`);
      const read = Object.keys(map);
      for (const key of read) {
        expect(
          spec.emitted,
          `${endpoint}.${level}：前端读了 ${key}，后端并不发`,
        ).toContain(key);
      }
      const unread = spec.emitted.filter((key) => !read.includes(key)).sort();
      // 差值必须是**记录下来的决定**，不是没人注意到的死投影。
      expect(unread).toEqual(Object.keys(spec.backend_only).sort());
    }
  });

  it("表级行：契约里每个键都被真的读到，且解析结果里没有契约之外的字段", async () => {
    const wire = wireFrom(
      CONTRACT.list_datasources.item?.emitted ?? [],
      ITEM_PROBE,
    );
    stubFetch({
      code: 0,
      data: { items: [wire], page: 1, page_size: 1, total: 1 },
    });

    const page = await listDatasources(query(), deps);
    // 一条 `toEqual` 同时盖住两个方向：某个键没被读 ⇒ 值停在兜底值上；
    // 解析结果多出一个契约之外的字段 ⇒ 期望对象里没有它。
    //
    // `fields` 要单独给：它在**线格式**与**解析后**是两个形状（snake → camel），
    // 而 `ITEM_PROBE.fields` 是喂给接口的那一份（线格式）。
    expect(page.items[0]).toEqual({
      ...expectedFrom(DATASOURCE_ITEM_KEYS, ITEM_PROBE),
      fields: [expectedFrom(FIELD_BINDING_KEYS, BINDING_PROBE)],
    });
  });

  /// 另外三个端点的探针（同上：值必须与兜底值不同）。
  const OPTION_PROBE: Record<string, unknown> = {
    option_id: "optPROBE",
    source_key: "probe_key",
    label: "探针选项",
    i18n: '{"zh_cn":"探针"}',
    sort_order: 99,
    // 兜底 false / true / null 都换掉
    is_default: true,
    enabled: false,
    parent_key: "optPARENT",
    last_push_at: 1758000004,
    updated_at: 1758000005,
  };
  const HEALTH_PROBE: Record<string, unknown> = {
    // `ok` 的兜底是 false（读不出结论不能算通过）
    ok: true,
    // 占位：嵌套数组在下面单独填（它自己是一个「形状」，有自己的一行契约）。
    missing_fields: [],
    view_missing: true,
    table_missing: true,
    unchecked: ["这一轮没查成"],
  };
  const MISSING_PROBE: Record<string, unknown> = {
    field_id: "fldPROBE",
    source_key: "probe_key",
  };
  const SCHEDULE_PROBE: Record<string, unknown> = {
    interval_seconds: 900,
    next_run_at: 1758000009,
  };

  it("字段绑定：同上", async () => {
    const wire = wireFrom(
      CONTRACT.list_datasources.binding?.emitted ?? [],
      BINDING_PROBE,
    );
    stubFetch({
      code: 0,
      data: {
        items: [
          {
            ...wireFrom(
              CONTRACT.list_datasources.item?.emitted ?? [],
              ITEM_PROBE,
            ),
            fields: [wire],
          },
        ],
        page: 1,
        page_size: 1,
        total: 1,
      },
    });

    const page = await listDatasources(query(), deps);
    expect(page.items[0]?.fields).toEqual([
      expectedFrom(FIELD_BINDING_KEYS, BINDING_PROBE),
    ]);
  });

  it("选项行：同上", async () => {
    const wire = wireFrom(
      CONTRACT.list_options?.item?.emitted ?? [],
      OPTION_PROBE,
    );
    stubFetch({
      code: 0,
      data: { items: [wire], page: 1, page_size: 1, total: 1 },
    });

    const page = await listOptions(
      { sourceKey: "probe_key", page: 1, pageSize: 1, orderBy: [] },
      deps,
    );
    expect(page.items[0]).toEqual(expectedFrom(OPTION_ITEM_KEYS, OPTION_PROBE));
  });

  it("体检报告（含嵌套的缺失字段项）：同上", async () => {
    const wire = {
      ...wireFrom(CONTRACT.health_check?.report?.emitted ?? [], HEALTH_PROBE),
      missing_fields: [
        wireFrom(
          CONTRACT.health_check?.missing_field?.emitted ?? [],
          MISSING_PROBE,
        ),
      ],
    };
    stubFetch({ code: 0, data: wire });

    const report = await checkDatasourceHealth(7, TABLE_DEPS);
    expect(report).toEqual({
      ...expectedFrom(HEALTH_REPORT_KEYS, HEALTH_PROBE),
      missingFields: [expectedFrom(HEALTH_MISSING_FIELD_KEYS, MISSING_PROBE)],
    });
  });

  /// **轴一：客户端字段名。** 前端发出的名字必须都在契约的 `client_fields` 里。
  ///
  /// 后端那一半（这些名字在那张表上确实可排/可筛）锚在真实表声明上，见
  /// `projection_contract::assert_client_fields_are_usable`。两半合起来才是一条契约。
  it("轴一：前端发出的排序/筛选字段名，契约里都记着", async () => {
    const collector = (node: unknown): string[] => {
      const record =
        node !== null && typeof node === "object"
          ? (node as Record<string, unknown>)
          : undefined;
      if (record === undefined) return [];
      if (record.type === "and" || record.type === "or") {
        return Array.isArray(record.conditions)
          ? record.conditions.flatMap(collector)
          : [];
      }
      return typeof record.field === "string" ? [record.field] : [];
    };

    const listContract = CONTRACT.client_fields?.list_datasources;
    expect(listContract).toBeDefined();
    // 状态筛选（列表页工具栏）+ 主键收窄（详情页）——两种 where 都要覆盖到
    for (const body of [
      buildDatasourceListBody(query({ status: "active" })),
      buildDatasourceListBody(query({ id: 7 })),
    ]) {
      for (const field of collector(body.where)) {
        expect(
          listContract?.where,
          `列表页在 where 里发了 ${field}，契约里没记`,
        ).toContain(field);
      }
      for (const clause of body.order_by as Array<{ field: string }>) {
        expect(
          listContract?.order_by,
          `列表页在 order_by 里发了 ${clause.field}，契约里没记`,
        ).toContain(clause.field);
      }
    }

    // 选项列表的排序键走另一条构造路径，用桩记录下来的真实请求体对账
    const calls = stubFetch({
      code: 0,
      data: { items: [], page: 1, page_size: 1, total: 0 },
    });
    await listOptions(
      { sourceKey: "k", page: 1, pageSize: 1, orderBy: [] },
      deps,
    );
    const optionBody = calls.at(-1)?.body as {
      order_by?: Array<{ field: string }>;
    };
    for (const clause of optionBody.order_by ?? []) {
      expect(
        CONTRACT.client_fields?.list_options?.order_by,
        `选项列表在 order_by 里发了 ${clause.field}，契约里没记`,
      ).toContain(clause.field);
    }
  });

  /// **轴二：枚举取值域。** 界面上的可选值必须与契约一致。
  ///
  /// `status` / `ingest_mode` 在契约里标 `enforced_by: backend`（后端表声明里有
  /// `.options(..)`，那边逐字对账）；`default_locale` 标 `enforced_by: frontend`——
  /// **后端对它零校验**，界面是唯一的守卫，所以这边这条断言就是它全部的防线。
  it("轴二：枚举取值域与契约一致（含只有界面在守的那个）", () => {
    const ingest = CONTRACT.enums?.feishu_datasource?.ingest_mode;
    expect(INGEST_MODE_OPTIONS.map((option) => option.value)).toEqual(
      ingest?.values,
    );

    const status = CONTRACT.enums?.feishu_datasource?.status;
    expect(
      STATUS_OPTIONS.map((option) => option.value).filter(
        (value) => value !== "all",
      ),
      "`all` 是纯界面值（不过滤），不属于后端取值域",
    ).toEqual(status?.values);

    const locale = CONTRACT.enums?.feishu_datasource_field?.default_locale;
    expect(
      locale?.enforced_by,
      "`default_locale` 的取值域只有界面在守——这条契约的意义就在这里",
    ).toBe("frontend");
    expect(DEFAULT_LOCALE_OPTIONS.map((option) => option.value)).toEqual(
      locale?.values,
    );
  });

  it("自动拉取排程：同上", async () => {
    stubFetch({
      code: 0,
      data: wireFrom(
        CONTRACT.pull_schedule?.result?.emitted ?? [],
        SCHEDULE_PROBE,
      ),
    });

    const schedule = await fetchPullSchedule(TABLE_DEPS);
    expect(schedule).toEqual(expectedFrom(PULL_SCHEDULE_KEYS, SCHEDULE_PROBE));
  });
});

describe("表级数据源行的投影", () => {
  /// 表级化之后 `list_datasources` 的形状（`list_datasources.rs` 的 `DatasourceItem`）：
  /// **表级行上没有 `source_key`**，它在 `fields[]` 的每条绑定上。
  const TABLE_ROW = {
    id: 7,
    title: "公司往来付款",
    status: "active",
    updated_at: 1758000000,
    ingest_mode: "pull",
    bitable_base_token: "app1",
    bitable_table_id: "tblA",
    bitable_view_id: "vew1",
    last_pull_at: null,
    last_success_at: null,
    consecutive_failures: 2,
    last_error: "1254024 InvalidFieldNames",
    fields: [
      {
        field_id: "fldA",
        field_name: "币种/Currency",
        source_key: "payment_currency",
        parent_field_id: null,
        enabled: true,
        // 这三个键属于绑定层。取值刻意各不相同，好让下面那条断言真的在验映射，
        // 而不是「全都落回默认值也算过」。
        encrypt_enabled: true,
        default_locale: "en_us",
      },
      {
        field_id: "fldB",
        field_name: null,
        source_key: "payment_fx_rate",
        parent_field_id: "fldA",
        enabled: false,
        encrypt_enabled: false,
        default_locale: "zh_cn",
      },
    ],
  };

  it("真实投影的行满足坐标判据——不会被误报成「坐标不完整」", async () => {
    // **回归**：`hasCompleteCoordinates` 曾经还要求「取数列字段名」，而
    // `list_datasources` 从表级化起就不发这个键（取数列属于字段绑定）。后果是
    // **每一条**真实数据源在详情页都显示「坐标不完整，不会被拉取」——而它的
    // `last_success_at` 就在几分钟前，服务端正在正常拉取。
    //
    // 这条刻意**不走手写对象**，而是真实解析器 + 服务端投影形状的 payload：
    // 手写 fixture 可以比现实「宽」（`datasource-pull.test.tsx` 的 `pullSource()`
    // 曾长期喂 `bitable_field_name`），解析器不行。这是那一类 bug 的通用防线。
    stubFetch({
      code: 0,
      data: { items: [TABLE_ROW], page: 1, page_size: 10, total: 1 },
    });
    const page = await listDatasources(query(), deps);
    const item = page.items[0];
    expect(item).toBeDefined();
    if (item === undefined) return;
    expect(hasCompleteCoordinates(item)).toBe(true);
    expect(syncHealth(item).title).not.toContain("坐标不完整");
  });

  it("id 与字段绑定被投影出来，且不因为表级行没有 source_key 就丢掉整行", async () => {
    stubFetch({
      code: 0,
      data: { items: [TABLE_ROW], page: 1, page_size: 10, total: 1 },
    });
    const page = await listDatasources(query(), deps);

    expect(page.items).toHaveLength(1);
    const item = page.items[0];
    expect(item?.id).toBe(7);
    expect(item?.consecutiveFailures).toBe(2);
    expect(item?.lastError).toBe("1254024 InvalidFieldNames");
    // 表级行上没有表级标识：`DatasourceItem` 里**根本不存在** `sourceKey` 这个字段。
    // 它曾经是一个恒为空串的字段（「为了不把存量调用点一次全改掉」），而详情页正是
    // 拿它去查一张没有这一列的表。这条断言钉住的是「那扇门已经拆了」——
    // 类型系统会拦住下次想用它的人，这里钉住类型本身没被改回去。
    expect(item).toBeDefined();
    expect(Object.hasOwn(item as object, "sourceKey")).toBe(false);
    expect(item?.fields).toEqual([
      {
        fieldId: "fldA",
        fieldName: "币种/Currency",
        sourceKey: "payment_currency",
        parentFieldId: null,
        enabled: true,
        encryptEnabled: true,
        defaultLocale: "en_us",
      },
      {
        fieldId: "fldB",
        // 还没解析过字段名（首次拉取前）：null，不是空串
        fieldName: null,
        sourceKey: "payment_fx_rate",
        parentFieldId: "fldA",
        enabled: false,
        encryptEnabled: false,
        defaultLocale: "zh_cn",
      },
    ]);
  });

  it("绑定行缺 source_key 或 field_id 时被丢掉（两者都定位不了）", async () => {
    stubFetch({
      code: 0,
      data: {
        items: [
          {
            ...TABLE_ROW,
            fields: [
              { field_id: "fldA", source_key: "" },
              { field_id: "", source_key: "orphan" },
            ],
          },
        ],
        page: 1,
        page_size: 10,
        total: 1,
      },
    });
    const page = await listDatasources(query(), deps);
    expect(page.items[0]?.fields).toEqual([]);
  });
});

describe("体检与凭据端点", () => {
  const CREDENTIAL_ACTIONS: ActionDemoSchema[] = [
    {
      ...DATASOURCE_LIST_ACTION,
      operation_id: "feishu.datasource.health_check",
      path: "/api/v1/feishu/datasources/table/health",
    },
    {
      ...DATASOURCE_LIST_ACTION,
      operation_id: "feishu.datasource.reveal_token",
      path: "/api/v1/feishu/datasources/reveal-token",
    },
    {
      ...DATASOURCE_LIST_ACTION,
      operation_id: "feishu.datasource.rotate_token",
      path: "/api/v1/feishu/datasources/rotate-token",
    },
  ];

  const credentialDeps: FeishuInvokeDeps = {
    catalog: catalogWith(CREDENTIAL_ACTIONS),
    session: { token: "tok-1" },
  };

  it("checkDatasourceHealth：按表级主键定位，路径里不放 id", async () => {
    const calls = stubFetch({
      code: 0,
      data: {
        ok: false,
        missing_fields: [{ field_id: "fldGONE", source_key: "old_rate" }],
        view_missing: false,
        table_missing: false,
        unchecked: [],
      },
    });
    const report = await checkDatasourceHealth(7, credentialDeps);

    expect(calls[0]?.url).toBe("/api/v1/feishu/datasources/table/health");
    expect(calls[0]?.method).toBe("POST");
    expect(calls[0]?.body).toEqual({ datasource_id: 7 });
    expect(report).toEqual({
      ok: false,
      missingFields: [{ fieldId: "fldGONE", sourceKey: "old_rate" }],
      viewMissing: false,
      tableMissing: false,
      unchecked: [],
    });
  });

  it("体检报告缺 `ok` 键时按不通过处理（读不出结论 ≠ 通过）", async () => {
    stubFetch({ code: 0, data: { missing_fields: [] } });
    const report = await checkDatasourceHealth(7, credentialDeps);
    expect(report.ok).toBe(false);
  });

  it("reveal：打到 reveal-token，source_key 走请求体（T12 落地的口径）", async () => {
    const calls = stubFetch({ code: 0, data: { token: "plaintext" } });
    const token = await revealFieldToken("payment_currency", credentialDeps);

    expect(calls[0]?.url).toBe("/api/v1/feishu/datasources/reveal-token");
    expect(calls[0]?.method).toBe("POST");
    // 路由里没有路径段：标识只能走 body
    expect(calls[0]?.body).toEqual({ source_key: "payment_currency" });
    expect(token).toBe("plaintext");
  });

  it("rotate：打到 rotate-token，返回的是新值（旧值当场失效）", async () => {
    const calls = stubFetch({ code: 0, data: { token: "rotated" } });
    const token = await rotateFieldToken("payment_currency", credentialDeps);

    expect(calls[0]?.url).toBe("/api/v1/feishu/datasources/rotate-token");
    expect(calls[0]?.body).toEqual({ source_key: "payment_currency" });
    expect(token).toBe("rotated");
  });

  it("回显响应缺 token 时拒绝，不返回一个空值让人去粘", async () => {
    stubFetch({ code: 0, data: {} });
    await expect(
      revealFieldToken("payment_currency", credentialDeps),
    ).rejects.toThrow(/没有 token/);
  });

  it("目录里没有回显端点时抛错（它是独立权限位）", async () => {
    const calls = stubFetch({ code: 0, data: { token: "x" } });
    await expect(revealFieldToken("payment_currency", deps)).rejects.toThrow(
      /找不到 Action「feishu\.datasource\.reveal_token」/,
    );
    expect(calls).toHaveLength(0);
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

describe("updateDatasourceTable", () => {
  it("PUT 打到表级端点，带主键、名称与绑定集合（整份替换）", async () => {
    const calls = stubFetch({
      code: 0,
      data: { inserted: 0, updated: 2, disabled: 0 },
    });
    const result = await updateDatasourceTable(
      {
        datasourceId: 7,
        title: "新名称",
        fields: [
          { fieldId: "fldA", sourceKey: "dept_sales", parentFieldId: null },
          { fieldId: "fldB", sourceKey: "dept_sub", parentFieldId: "fldA" },
        ],
      },
      deps,
    );

    expect(calls[0]?.url).toBe("/api/v1/feishu/datasources/table");
    expect(calls[0]?.method).toBe("PUT");
    expect(calls[0]?.body).toEqual({
      datasource_id: 7,
      title: "新名称",
      fields: [
        { field_id: "fldA", source_key: "dept_sales", parent_field_id: null },
        { field_id: "fldB", source_key: "dept_sub", parent_field_id: "fldA" },
      ],
    });
    expect(result).toEqual({ inserted: 0, updated: 2, disabled: 0 });
  });

  it("没给名称时整个键不出现（省略 = 不改）", async () => {
    const calls = stubFetch({
      code: 0,
      data: { inserted: 0, updated: 1, disabled: 0 },
    });
    await updateDatasourceTable(
      {
        datasourceId: 7,
        fields: [
          { fieldId: "fldA", sourceKey: "dept_sales", parentFieldId: null },
        ],
      },
      deps,
    );
    expect(calls[0]?.body).not.toHaveProperty("title");
  });
});

describe("enabledBindingInputs", () => {
  it("只送启用中的绑定：集合里出现的已有绑定会被服务端写上 enabled = true", () => {
    // 把一条已停用的绑定塞回去，等于在「只是改个名字」的时候悄悄把它重新启用。
    expect(
      enabledBindingInputs({
        fields: [
          {
            fieldId: "fldA",
            fieldName: "费用类型",
            sourceKey: "dept_sales",
            parentFieldId: null,
            enabled: true,
            encryptEnabled: false,
            defaultLocale: "zh_cn",
          },
          {
            fieldId: "fldB",
            fieldName: "已停用",
            sourceKey: "dept_old",
            parentFieldId: "fldA",
            enabled: false,
            encryptEnabled: false,
            defaultLocale: "zh_cn",
          },
        ],
      }),
    ).toEqual([
      { fieldId: "fldA", sourceKey: "dept_sales", parentFieldId: null },
    ]);
  });
});

describe("deleteDatasourceTable", () => {
  it("DELETE 只带表级主键，回执含被连带停用的选项数", async () => {
    const calls = stubFetch({
      code: 0,
      data: { deleted_fields: 2, disabled_options: 3 },
    });
    const result = await deleteDatasourceTable(7, deps);
    expect(calls[0]?.url).toBe("/api/v1/feishu/datasources/table");
    expect(calls[0]?.method).toBe("DELETE");
    expect(calls[0]?.body).toEqual({ datasource_id: 7 });
    expect(result).toEqual({ deletedFields: 2, disabledOptions: 3 });
  });
});

describe("pullNow", () => {
  it("发的是表级 `datasource_id`，一个多余的键都不带", async () => {
    // 后端 `PullNowInput` 是 `deny_unknown_fields` + 必填 `datasource_id`：
    // 多发一个键（例如退役前的 `source_key`）会被直接拒掉。
    const calls = stubFetch({ code: 0, data: { accepted: true } });
    await pullNow(7, deps);
    expect(calls[0]?.url).toBe("/api/v1/feishu/datasources/pull-now");
    expect(calls[0]?.method).toBe("POST");
    expect(calls[0]?.body).toEqual({ datasource_id: 7 });
  });
});

/* ---------------------- 表级配置：元数据与创建端点 ---------------------- */

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

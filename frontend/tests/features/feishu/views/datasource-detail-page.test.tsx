import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { renderTestApp } from "@test/helpers/render-app";

import {
  bodiesOf,
  datasourceWire,
  jsonResponse,
  listPage,
  optionWire,
  stubFeishuApi,
} from "./harness";

const OPTIONS_PATH = "/api/v1/feishu/options/query";
const SOURCE_KEY = "expense_category";

/// 详情页（走真实路由 `/feishu/datasources/:sourceKey`）：只读选项表、默认按「最近推送」
/// 倒序、0 选项空态、缺 `option.read` 的 403 态，以及返回列表入口。

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

function renderDetail() {
  return renderTestApp({
    path: `/feishu/datasources/${SOURCE_KEY}`,
    authenticated: true,
  });
}

const TWO_OPTIONS = [
  optionWire({
    option_id: "travel",
    label: "差旅费",
    i18n: '{"en_us":"Travel"}',
    sort_order: 1,
    is_default: true,
    updated_at: 1758000000,
  }),
  optionWire({
    option_id: "meal",
    label: "餐费",
    i18n: null,
    sort_order: 2,
    enabled: false,
    updated_at: 1758003600,
  }),
];

describe("飞书数据源详情页 · 只读选项表", () => {
  it("渲染选项，且请求默认按「最近推送」倒序（option_id 收尾）", async () => {
    const calls = stubFeishuApi({ optionList: () => listPage(TWO_OPTIONS) });
    renderDetail();

    expect(await screen.findByText("差旅费")).toBeInTheDocument();
    expect(screen.getByText("餐费")).toBeInTheDocument();
    // 选项级「停用」在状态列如实呈现
    expect(screen.getByText("已停用")).toBeInTheDocument();
    expect(
      screen.getByText("默认", { selector: "[data-slot='status-badge']" }),
    ).toBeInTheDocument();

    const bodies = bodiesOf(calls, OPTIONS_PATH);
    expect(bodies.length).toBeGreaterThan(0);
    expect(bodies[0]).toMatchObject({
      source_key: SOURCE_KEY,
      count_total: true,
      // 决策 6：默认按「选项最后一次被推送的时间」倒序；唯一键收尾保证翻页全序
      order_by: [
        { field: "updated_at", direction: "Desc" },
        { field: "option_id", direction: "Asc" },
      ],
    });
  });

  it("i18n 是 JSON 文本：解析成语言名展示，不把原文铺在单元格里", async () => {
    stubFeishuApi({ optionList: () => listPage(TWO_OPTIONS) });
    renderDetail();

    expect(await screen.findByText("English：Travel")).toBeInTheDocument();
    expect(screen.queryByText('{"en_us":"Travel"}')).toBeNull();
  });

  it("点「最近写入」列头切换排序方向", async () => {
    const user = userEvent.setup();
    const calls = stubFeishuApi({ optionList: () => listPage(TWO_OPTIONS) });
    renderDetail();

    await user.click(
      await screen.findByRole("button", { name: "按最近写入排序" }),
    );

    await waitFor(() => {
      const bodies = bodiesOf(calls, OPTIONS_PATH);
      expect(bodies[bodies.length - 1]?.order_by).toEqual([
        { field: "updated_at", direction: "Asc" },
        { field: "option_id", direction: "Asc" },
      ]);
    });
  });

  it("只读：页面里没有任何选项的增删改入口", async () => {
    stubFeishuApi({ optionList: () => listPage(TWO_OPTIONS) });
    renderDetail();

    await screen.findByText("差旅费");
    for (const name of [/添加选项/, /新建选项/, /编辑选项/, /删除选项/]) {
      expect(screen.queryByRole("button", { name })).toBeNull();
    }
    // 选项行上没有操作菜单；详情页也不放数据源自身的重命名/停用/删除入口
    expect(screen.queryByRole("button", { name: /的操作/ })).toBeNull();
  });
});

describe("飞书数据源详情页 · 说明与返回", () => {
  it("顶部说明讲清「只读」与两级「停用」的区别", async () => {
    stubFeishuApi({ optionList: () => listPage(TWO_OPTIONS) });
    renderDetail();

    expect(
      await screen.findByText(/选项由多维表格自动推送，控制台只读/),
    ).toBeInTheDocument();
    expect(screen.getByText("数据源级停用")).toBeInTheDocument();
    expect(screen.getByText("选项级停用")).toBeInTheDocument();
    expect(
      screen.getByText(/后端是把它禁用而不是删除，改回来就恢复/),
    ).toBeInTheDocument();
  });

  it("「返回数据源列表」把用户带回列表页", async () => {
    const user = userEvent.setup();
    stubFeishuApi({
      optionList: () => listPage(TWO_OPTIONS),
      datasourceList: () => listPage([]),
    });
    renderDetail();

    await user.click(
      await screen.findByRole("link", { name: "返回数据源列表" }),
    );
    expect(
      await screen.findByRole("heading", { name: "飞书数据源" }),
    ).toBeInTheDocument();
  });
});

describe("飞书数据源详情页 · 两个异常分支", () => {
  it("0 选项：指向多维表格的自动化，不写「暂无数据」", async () => {
    stubFeishuApi({ optionList: () => listPage([]) });
    renderDetail();

    expect(
      await screen.findByRole("heading", { name: "还没有选项推过来" }),
    ).toBeInTheDocument();
    expect(screen.getByText(/多维表格那边的自动化/)).toBeInTheDocument();
    expect(screen.getByText(/至少成功跑过一次/)).toBeInTheDocument();
    expect(screen.queryByText("暂无数据")).toBeNull();
    expect(screen.queryByRole("table")).toBeNull();
  });

  it("空结果集不替服务端背书：「没有选项」与「数据源不存在」分不出来", async () => {
    // 服务端的 list_options 对不存在的 source_key 也只回一个空结果集，而本页没有
    // 取单条数据源的 Action——两种原因在这一页长得一模一样，所以：
    // 既不能说「这个数据源本身是好的」，也不能反过来断言它不存在。
    stubFeishuApi({ optionList: () => listPage([]) });
    renderDetail();

    await screen.findByRole("heading", { name: "还没有选项推过来" });
    expect(screen.queryByText(/这个数据源本身是好的/)).toBeNull();
    expect(
      screen.getByText(/也可能是这个数据源已经不在了/),
    ).toBeInTheDocument();
    // 也给出分辩的动作：回列表页看它还在不在
    expect(screen.getByText(/回列表页看一眼就知道/)).toBeInTheDocument();
  });

  it("选项查询报错时原样回显后端原文（例如「数据源不存在」），不给 0 选项空态", async () => {
    stubFeishuApi({
      optionList: () =>
        jsonResponse({ code: 400001, message: "数据源不存在" }, 400),
    });
    renderDetail();

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("数据源不存在");
    expect(
      screen.queryByRole("heading", { name: "还没有选项推过来" }),
    ).toBeNull();
  });

  it("缺 option.read：给 403 说明与重试，既不是白屏也不是空列表", async () => {
    const calls = stubFeishuApi({
      optionRead: false,
      optionList: () => listPage(TWO_OPTIONS),
    });
    renderDetail();

    expect(await screen.findByText("你没有查看选项的权限")).toBeInTheDocument();
    expect(screen.getByText(/不代表它真的没有选项/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
    // 空态不能顶替 403（那是两条不同的结论）
    expect(
      screen.queryByRole("heading", { name: "还没有选项推过来" }),
    ).toBeNull();
    expect(screen.queryByRole("table")).toBeNull();
    // 没有权限就不发那次注定 403 的请求
    expect(bodiesOf(calls, OPTIONS_PATH)).toHaveLength(0);
  });

  it("连 datasource.read 也没有时，不说「当前身份可以看数据源本身」", async () => {
    // 三个权限位彼此独立：两粒都没有的身份照样能点到这个 URL，
    // 那就不能替它说一句它不成立的话。
    const calls = stubFeishuApi({
      datasourceRead: false,
      optionRead: false,
      optionList: () => listPage(TWO_OPTIONS),
    });
    renderDetail();

    expect(await screen.findByText("你没有查看选项的权限")).toBeInTheDocument();
    expect(screen.queryByText(/现在身份可以看数据源本身/)).toBeNull();
    expect(screen.getByText(/看不到数据源本身/)).toBeInTheDocument();
    expect(screen.getByText(/不代表它真的没有选项/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
    expect(bodiesOf(calls, OPTIONS_PATH)).toHaveLength(0);
  });

  it("选项拉不到时给后端原文与「重试」", async () => {
    stubFeishuApi({
      optionList: () =>
        jsonResponse({ code: 400001, message: "数据源不存在" }, 400),
    });
    renderDetail();

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("数据源不存在");
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
  });
});

/// 表级扩展（T15）：体检面板与凭据清单，按**表级 id 与字段绑定**定位。
describe("飞书数据源详情页 · 体检与凭据清单", () => {
  /// 表级行：没有表级 `source_key`，凭据在 `fields[]` 的每条绑定上。
  /// 路由参数仍是那个**字段的** source_key（列表页就是这么链过来的）。
  const TABLE_ROW = datasourceWire({
    id: 7,
    source_key: undefined,
    title: "公司往来付款",
    ingest_mode: "pull",
    bitable_base_token: "app1",
    bitable_table_id: "tblA",
    bitable_view_id: "vew1",
    fields: [
      {
        field_id: "fldEblAr7X",
        field_name: "费用类型/Fee Type*",
        source_key: SOURCE_KEY,
        parent_field_id: null,
        enabled: true,
      },
      {
        field_id: "fldGONE",
        field_name: null,
        source_key: "old_rate",
        parent_field_id: null,
        enabled: false,
      },
    ],
  });

  function renderTableDetail(
    options: Parameters<typeof stubFeishuApi>[0] = {},
  ) {
    const calls = stubFeishuApi({
      tableConfig: true,
      datasourceList: () => listPage([TABLE_ROW]),
      optionList: () => listPage(TWO_OPTIONS),
      ...options,
    });
    renderDetail();
    return calls;
  }

  it("凭据清单按字段绑定列出：字段名 + 该字段的 URL，且只列启用中的", async () => {
    renderTableDetail();

    expect(await screen.findByText("费用类型/Fee Type*")).toBeInTheDocument();
    expect(
      screen.getByText(
        `${window.location.origin}/api/v1/feishu/approval/options/${SOURCE_KEY}`,
      ),
    ).toBeInTheDocument();
    // 停用的绑定不进清单（它出站会吃 SOURCE_DISABLED），但要说明它为什么不在
    expect(screen.queryByText(/old_rate/)).toBeNull();
    expect(screen.getByText(/1 条已停用的绑定没列在这里/)).toBeInTheDocument();
  });

  it("体检按表级 id 打 health 端点，并把缺失字段的 id 与 source_key 一起列出", async () => {
    const calls = renderTableDetail({
      health: () => ({
        ok: false,
        missing_fields: [{ field_id: "fldGONE", source_key: "old_rate" }],
        view_missing: false,
        table_missing: false,
        unchecked: [],
      }),
    });

    expect(await screen.findByText("fldGONE")).toBeInTheDocument();
    expect(screen.getByText("old_rate")).toBeInTheDocument();
    expect(bodiesOf(calls, "/api/v1/feishu/datasources/table/health")).toEqual([
      { datasource_id: 7 },
    ]);
  });

  it("复制 Token 走回显端点（读），绝不打轮换端点（写）", async () => {
    // 决策 D10 的硬要求：误点复制不能有任何后果。
    const user = userEvent.setup();
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    const calls = renderTableDetail();

    await screen.findByText("费用类型/Fee Type*");
    await user.click(screen.getByRole("button", { name: "复制 Token" }));
    await waitFor(() => {
      expect(writeText).toHaveBeenCalledWith("revealed-token");
    });

    // 回显（读）打了，且 source_key 走请求体；轮换（写）一个都没打
    const reveals = calls.filter((call) => call.url.endsWith("/reveal-token"));
    expect(reveals).toHaveLength(1);
    expect(reveals[0]?.body).toEqual({ source_key: SOURCE_KEY });
    expect(
      calls.filter((call) => call.url.endsWith("/rotate-token")),
    ).toHaveLength(0);
    vi.unstubAllGlobals();
  });
});

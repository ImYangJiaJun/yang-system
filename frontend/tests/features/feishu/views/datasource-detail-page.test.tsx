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
const LIST_PATH = "/api/v1/feishu/datasources/query";
const SOURCE_KEY = "expense_category";
/// 详情页的身份是**表级主键**（一条数据源 = 一张表）。
const DATASOURCE_ID = 7;

/// 详情页（走真实路由 `/feishu/datasources/:id`）：只读选项表、默认按「最近推送」
/// 倒序、0 选项空态、缺 `option.read` 的 403 态，以及返回列表入口。

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

function renderDetail() {
  return renderTestApp({
    path: `/feishu/datasources/${DATASOURCE_ID}`,
    authenticated: true,
  });
}

/// 详情页的默认数据源行：一条表级行 + 一条启用中的绑定（`source_key` = `SOURCE_KEY`）。
///
/// 详情页现在**必须**先取到这一行才能开始工作：选项是按**某条绑定**的 `source_key`
/// 索引的，取不到就无从知道该查哪个字段——页面会明说「上面「同步」区还没拿到这条
/// 数据源」。以前可以省掉这一步，因为选项区直接读路由参数，那是字段级时代的口径。
const DETAIL_ROW = datasourceWire({
  id: DATASOURCE_ID,
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
  ],
});

/// 凭据清单的每一行。**必须按行取，不能按文本取**：详情页现在上面还有一张
/// 字段绑定表，两处都会出现字段名与标识——`getByText` 会同时命中，测试红在
/// 「找到多个」上，而那不是被测行为出了问题。
function credentialRows(): HTMLElement[] {
  return Array.from(
    document.querySelectorAll<HTMLElement>('[data-slot="credential-row"]'),
  );
}

/// 装上桩，并把那条数据源喂上（每个用例都要，见 `DETAIL_ROW`）。
function stubDetail(
  options: Parameters<typeof stubFeishuApi>[0] = {},
): ReturnType<typeof stubFeishuApi> {
  return stubFeishuApi({
    datasourceList: () => listPage([DETAIL_ROW]),
    ...options,
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
    const calls = stubDetail({ optionList: () => listPage(TWO_OPTIONS) });
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
    stubDetail({ optionList: () => listPage(TWO_OPTIONS) });
    renderDetail();

    expect(await screen.findByText("English：Travel")).toBeInTheDocument();
    expect(screen.queryByText('{"en_us":"Travel"}')).toBeNull();
  });

  it("点「最近写入」列头切换排序方向", async () => {
    const user = userEvent.setup();
    const calls = stubDetail({ optionList: () => listPage(TWO_OPTIONS) });
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
    stubDetail({ optionList: () => listPage(TWO_OPTIONS) });
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
    stubDetail({ optionList: () => listPage(TWO_OPTIONS) });
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
    stubDetail({
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
    stubDetail({ optionList: () => listPage([]) });
    renderDetail();

    expect(
      await screen.findByRole("heading", { name: "还没有选项推过来" }),
    ).toBeInTheDocument();
    expect(screen.getByText(/多维表格那边的自动化/)).toBeInTheDocument();
    expect(screen.getByText(/至少成功跑过一次/)).toBeInTheDocument();
    expect(screen.queryByText("暂无数据")).toBeNull();
    // 「不渲染空表」要**只针对选项区**：`queryByRole("table")` 会把下面
    // 凭据清单那张表也算进来——而它现在会正常渲染（这条数据源取到了），
    // 于是这条断言会在一个**修好了**的页面上失败。改用「没有选项行」来钉。
    expect(screen.queryByText("差旅费")).toBeNull();
  });

  it("空结果集不替服务端背书：「没有选项」与「数据源不存在」分不出来", async () => {
    // 服务端的 list_options 对不存在的 source_key 也只回一个空结果集，而本页没有
    // 取单条数据源的 Action——两种原因在这一页长得一模一样，所以：
    // 既不能说「这个数据源本身是好的」，也不能反过来断言它不存在。
    stubDetail({ optionList: () => listPage([]) });
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
    stubDetail({
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
    const calls = stubDetail({
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
    // 同上：只断言「没有选项行」，不断言「页面里没有 table」——凭据清单那张表
    // 与权限无关，它在修好的页面上本来就会出现。
    expect(screen.queryByText("差旅费")).toBeNull();
    // 没有权限就不发那次注定 403 的请求
    expect(bodiesOf(calls, OPTIONS_PATH)).toHaveLength(0);
  });

  it("连 datasource.read 也没有时，不说「当前身份可以看数据源本身」", async () => {
    // 三个权限位彼此独立：两粒都没有的身份照样能点到这个 URL，
    // 那就不能替它说一句它不成立的话。
    const calls = stubDetail({
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
    stubDetail({
      optionList: () =>
        jsonResponse({ code: 400001, message: "数据源不存在" }, 400),
    });
    renderDetail();

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("数据源不存在");
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
  });
});

/// 取这条数据源的**五态**：加载中 / 地址不合法 / 无权限 / 请求被拒 / 确认不存在。
///
/// 这一组全是回归：曾经「请求被拒」与「没有这一行」渲染成同一句话，于是三块面板
/// 一起说「查不到这条数据源」——而那条源就在库里。区分它们不是措辞问题，
/// 是**下一步动作**问题（重试并看错误码 vs 回列表确认）。
describe("飞书数据源详情页 · 取数据源的五态", () => {
  it("按表级主键取单条：请求体是 where id，且没有任何 source_key / order_by source_key", async () => {
    const calls = stubDetail();
    renderDetail();

    await screen.findByRole("heading", { name: "同步" });
    const body = bodiesOf(calls, LIST_PATH)[0];
    expect(body).toMatchObject({
      where: { type: "eq", field: "id", value: DATASOURCE_ID },
    });
    // 曾经这里是 `orderBy: [{field:"source_key"}]` + `search: <source_key>`：
    // 表级行上没有这一列 → 排序校验 400、检索命中零行，两处都失败。
    expect(body?.order_by).toEqual([{ field: "id", direction: "Asc" }]);
    expect(JSON.stringify(body)).not.toContain("source_key");
  });

  it("请求被拒：说清「是请求被拒」，并给出 status / code / 后端原文", async () => {
    stubDetail({
      datasourceList: () =>
        jsonResponse(
          { code: 900001, message: "Unknown column 'source_key'" },
          400,
        ),
    });
    renderDetail();

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("是请求被拒，不是「没有数据」");
    expect(alert).toHaveTextContent("HTTP 400");
    expect(alert).toHaveTextContent("code 900001");
    expect(alert).toHaveTextContent("Unknown column 'source_key'");
    // 三个区块不再各自说一遍同一句话：取不到就只在一处说明原因。
    expect(screen.queryByRole("heading", { name: "体检" })).toBeNull();
    expect(screen.queryByRole("heading", { name: "同步" })).toBeNull();
  });

  it("查询成功但没有这一行：另一句话，且**不给**重试按钮（重试也还是没有）", async () => {
    stubDetail({ datasourceList: () => listPage([]) });
    renderDetail();

    expect(await screen.findByText(/查询成功但没有这一行/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "重试读取" })).toBeNull();
  });

  it("地址不是数字主键（旧深链）：一个请求都不发，也不说「不存在」", async () => {
    const calls = stubDetail();
    renderTestApp({
      path: "/feishu/datasources/expense_category",
      authenticated: true,
    });

    expect(await screen.findByText(/没有有效的数据源主键/)).toBeInTheDocument();
    // 查询是 `enabled: false`：`parseDatasourceId` 已经把这一态挡在前面了。
    expect(bodiesOf(calls, LIST_PATH)).toHaveLength(0);
  });

  it("缺 datasource.read：说权限，不是「加载中」——不给永远转下去的骨架屏", async () => {
    // 权限不足时那一发查询是 `enabled: false`，`isPending` 会**永远**为真。
    // 不把它单独判出来，这一页就是一块永远转下去的空白。
    stubDetail({ datasourceRead: false });
    renderDetail();

    expect(
      await screen.findByText("当前身份没有查看数据源的权限"),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "重新加载权限目录" }),
    ).toBeInTheDocument();
  });
});

/// 「一张表 = N 个字段」在界面上的落点：字段绑定表既是全貌，也是切换器。
describe("飞书数据源详情页 · 字段表即切换器", () => {
  /// 两级链：费用大类（父）→ 费用类型（子）。子先给、父后给，排序得自己理出来。
  const TWO_FIELDS = datasourceWire({
    id: DATASOURCE_ID,
    title: "公司往来付款",
    ingest_mode: "pull",
    bitable_base_token: "app1",
    bitable_table_id: "tblA",
    fields: [
      {
        field_id: "fldB",
        field_name: "费用类型/Fee Type*",
        source_key: SOURCE_KEY,
        parent_field_id: "fldA",
        enabled: true,
      },
      {
        field_id: "fldA",
        field_name: "费用大类/Main Exp Cat*",
        source_key: "main_exp_cat",
        parent_field_id: null,
        enabled: true,
      },
    ],
  });

  it("默认看第一条启用中的绑定，点另一行就把选项切过去（父子相邻）", async () => {
    const user = userEvent.setup();
    const calls = stubDetail({
      datasourceList: () => listPage([TWO_FIELDS]),
      optionList: () => listPage(TWO_OPTIONS),
    });
    renderDetail();

    // 起点是**父**那一行（`orderBindingsForDisplay` 把它排在前面）
    await waitFor(() => {
      expect(bodiesOf(calls, OPTIONS_PATH)[0]?.source_key).toBe("main_exp_cat");
    });
    // 父子相邻：父的行先出现，子紧随其后
    const bindingRows = Array.from(
      document.querySelectorAll<HTMLElement>('[data-slot="binding-row"]'),
    );
    expect(bindingRows.map((row) => row.dataset.depth)).toEqual(["0", "1"]);

    await user.click(
      screen.getByRole("button", { name: "费用类型/Fee Type*" }),
    );

    // 切过去之后发的是**子**的标识，而且说明了下面看的是哪个字段
    await waitFor(() => {
      expect(bodiesOf(calls, OPTIONS_PATH).at(-1)?.source_key).toBe(SOURCE_KEY);
    });
    expect(screen.getByText(/「费用类型\/Fee Type\*」/)).toBeInTheDocument();
  });

  it("切字段那一帧不拿上一个字段的行冒充：占位帧画骨架，不画旧行、也不说「还没有选项」", async () => {
    // **回归**：`useOptionList` 带 `keepPreviousData`，切字段会换查询键，于是那一帧里
    // `isPending` / `isError` 都是 false、`isSuccess` 还是 true，而 data 是**上一个字段**的。
    // 详情页原先只判 isPending/isError，于是画出来的是「新字段的名字 + 旧字段的行」；
    // 旧字段恰好为空时，还会对新字段说「还没有选项推过来」——一句当场可证伪的假话。
    const user = userEvent.setup();
    let call = 0;
    stubDetail({
      datasourceList: () => listPage([TWO_FIELDS]),
      optionList: () => {
        call += 1;
        // 第一次（父）正常落定；第二次（子）**永不作答**，把占位那一帧定住。
        return call === 1
          ? listPage(TWO_OPTIONS)
          : new Promise<never>(() => {});
      },
    });
    renderDetail();

    expect(await screen.findByText("差旅费")).toBeInTheDocument();

    await user.click(
      screen.getByRole("button", { name: "费用类型/Fee Type*" }),
    );

    // 新字段的数据还没来：旧字段的行必须消失，且不能替新字段下「没有选项」的结论。
    await waitFor(() => expect(screen.queryByText("差旅费")).toBeNull());
    expect(
      screen.queryByRole("heading", { name: "还没有选项推过来" }),
    ).toBeNull();
  });

  it("切字段回到第 1 页——新字段很可能没有当前那一页", async () => {
    // **回归**：切字段是一次**结果集变更**。不回到第 1 页，新字段的第 2 页往往不存在：
    // 服务端回空 items 而 `count_total` 仍给真值，于是「共 N 条」与空态同时出现，
    // 而空态分支**不渲染分页控件**——人被卡在那一页，点别的字段也还是同一页码。
    const user = userEvent.setup();
    const calls = stubDetail({
      datasourceList: () => listPage([TWO_FIELDS]),
      optionList: () => listPage(TWO_OPTIONS, { total: 25 }),
    });
    renderDetail();

    // 父字段有 25 条 ⇒ 有第 2 页
    await screen.findByText("差旅费");
    await user.click(screen.getByRole("button", { name: "下一页" }));
    await waitFor(() => {
      expect(bodiesOf(calls, OPTIONS_PATH).at(-1)?.page).toBe(2);
    });

    await user.click(
      screen.getByRole("button", { name: "费用类型/Fee Type*" }),
    );

    await waitFor(() => {
      const last = bodiesOf(calls, OPTIONS_PATH).at(-1);
      expect(last?.source_key).toBe(SOURCE_KEY);
      expect(last?.page).toBe(1);
    });
  });

  it("一条绑定都没有时不渲染字段表，也不拿空标识去查选项", async () => {
    const calls = stubDetail({
      datasourceList: () =>
        listPage([datasourceWire({ id: DATASOURCE_ID, fields: [] })]),
    });
    renderDetail();

    expect(
      await screen.findByText(/这条数据源还没有字段绑定/),
    ).toBeInTheDocument();
    expect(document.querySelectorAll('[data-slot="binding-row"]')).toHaveLength(
      0,
    );
    expect(bodiesOf(calls, OPTIONS_PATH)).toHaveLength(0);
  });
});

/// 表级扩展（T15）：体检面板与凭据清单，按**表级 id 与字段绑定**定位。
describe("飞书数据源详情页 · 体检与凭据清单", () => {
  /// 表级行：没有表级 `source_key`，凭据在 `fields[]` 的每条绑定上。
  /// 路由参数是 `id`——列表页就是按它链过来的。
  const TABLE_ROW = datasourceWire({
    id: DATASOURCE_ID,
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
    const calls = stubDetail({
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

    await waitFor(() => expect(credentialRows()).toHaveLength(1));
    const [row] = credentialRows();
    expect(row).toHaveTextContent("费用类型/Fee Type*");
    expect(row).toHaveTextContent(
      `${window.location.origin}/api/v1/feishu/approval/options/${SOURCE_KEY}`,
    );
    // 停用的绑定不进**凭据清单**（它出站会吃 SOURCE_DISABLED），但要说明它为什么不在。
    // 注意它仍会出现在上面的字段绑定表里——那张表列的是这张表有哪些列，与能不能出站无关。
    expect(row).not.toHaveTextContent("old_rate");
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

    // 同样按 `data-slot` 取：`old_rate` 在字段绑定表里也有一行，按文本会命中两个。
    await waitFor(() =>
      expect(
        document.querySelectorAll('[data-slot="missing-field"]').length,
      ).toBe(1),
    );
    const [missing] = Array.from(
      document.querySelectorAll<HTMLElement>('[data-slot="missing-field"]'),
    );
    expect(missing).toHaveTextContent("fldGONE");
    expect(missing).toHaveTextContent("old_rate");
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

    await waitFor(() => expect(credentialRows()).toHaveLength(1));
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

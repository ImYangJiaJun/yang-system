import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { renderTestApp } from "@test/helpers/render-app";

import {
  bodiesOf,
  countCalls,
  datasourceWire,
  jsonResponse,
  listPage,
  stubFeishuApi,
} from "./harness";

const LIST_PATH = "/api/v1/feishu/datasources/query";

/// 造 count 行数据源：标识 `ds_01`..、名称「数据源 01」..——默认排序（`source_key Asc`）
/// 下顺序与数组一致，按页断言才不用再排一遍。
function makeRows(count: number): Array<Record<string, unknown>> {
  return Array.from({ length: count }, (_, index) => {
    const n = String(index + 1).padStart(2, "0");
    return datasourceWire({ source_key: `ds_${n}`, title: `数据源 ${n}` });
  });
}

/// `where` 是页面自己构造的布尔树（`{ type: "eq", field: "status", value }`）。
function asWhereValue(where: unknown): unknown {
  return where !== null && typeof where === "object" && "value" in where
    ? (where as { value: unknown }).value
    : null;
}

function orderedByTitle(orderBy: unknown): boolean {
  return (
    Array.isArray(orderBy) &&
    (orderBy[0] as { field?: string } | undefined)?.field === "title"
  );
}

/**
 * 一个够真实的列表桩：按 `where.status` 与 `search` 过滤、按 page/page_size 切页，
 * `total` 回**过滤之后**的条数——「共 N 个」说的就是它。
 */
function pageRows(
  rows: Array<Record<string, unknown>>,
  body: Record<string, unknown>,
) {
  const status = asWhereValue(body.where);
  const search = typeof body.search === "string" ? body.search : "";
  const matched = rows.filter(
    (row) =>
      (status === null || row.status === status) &&
      (search === "" || String(row.title).includes(search)),
  );
  const page = Number(body.page ?? 1);
  const pageSize = Number(body.page_size ?? 10);
  return listPage(matched.slice((page - 1) * pageSize, page * pageSize), {
    page,
    pageSize,
    total: matched.length,
  });
}

/// 第 from 次调用之后的列表请求体：用来看「这次变更之后到底取了第几页」。
function listBodiesAfter(
  calls: ReturnType<typeof stubFeishuApi>,
  from: number,
): Array<Record<string, unknown> | undefined> {
  return calls
    .slice(from)
    .filter((call) => call.url.endsWith(LIST_PATH))
    .map((call) => call.body);
}

/// 列表页（走真实路由 `/feishu/datasources`，含 lazy 的 `.default` 约定）：
/// 双视图、权限门控、四个情境化指引里属于列表页的两个落点，以及必须断言的查询行为。

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

function renderList() {
  return renderTestApp({ path: "/feishu/datasources", authenticated: true });
}

describe("飞书数据源列表页 · 四种状态", () => {
  it("空态：四步指引就是页面正文，工具栏与空栅格都不渲染", async () => {
    stubFeishuApi({ datasourceList: () => listPage([]) });
    renderList();

    expect(
      await screen.findByRole("heading", { name: "还没有数据源" }),
    ).toBeInTheDocument();
    expect(screen.getByText("在飞书审批后台配置控件")).toBeInTheDocument();
    expect(screen.getByText("在这里建一个数据源")).toBeInTheDocument();
    expect(
      screen.getByText("把接口地址与 Token 填回审批后台"),
    ).toBeInTheDocument();
    expect(screen.getByText("在多维表格配自动化推送选项")).toBeInTheDocument();
    // 工具栏整条不渲染（此刻唯一该做的事就是建档）
    expect(screen.queryByLabelText("搜索数据源")).toBeNull();
    // 不渲染空栅格 / 空表
    expect(document.querySelector('[data-slot="datasource-card"]')).toBeNull();
    expect(screen.queryByRole("table")).toBeNull();
  });

  it("加载中：按当前视图出骨架，而不是先闪一下空态", async () => {
    stubFeishuApi({ datasourceList: () => new Promise<never>(() => {}) });
    renderList();

    // 等页面挂上（AppLayout 在目录就绪前不渲染 Outlet，那之前它自己的骨架不算数）
    await screen.findByRole("heading", { name: "飞书数据源" });
    // 默认视图是台账：若干行骨架
    await waitFor(() =>
      expect(
        document.querySelectorAll('[data-slot="skeleton"]').length,
      ).toBeGreaterThan(0),
    );
    expect(screen.queryByRole("heading", { name: "还没有数据源" })).toBeNull();
    expect(screen.getByLabelText("搜索数据源")).toBeInTheDocument();
  });

  it("错误：错误条给后端原文与「重试」，重试真的再打一次", async () => {
    const user = userEvent.setup();
    const calls = stubFeishuApi({
      datasourceList: () =>
        jsonResponse({ code: 500001, message: "数据源不存在" }, 500),
    });
    renderList();

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("数据源不存在");
    // 错误态不渲染四步指引
    expect(screen.queryByRole("heading", { name: "还没有数据源" })).toBeNull();

    const before = countCalls(calls, LIST_PATH);
    await user.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() =>
      expect(countCalls(calls, LIST_PATH)).toBeGreaterThan(before),
    );
  });

  it("搜索无结果与「一个数据源都没有」区分开，且搜索后回到第 1 页", async () => {
    const user = userEvent.setup();
    const calls = stubFeishuApi({
      datasourceList: (body) =>
        body.search === "zzz"
          ? listPage([])
          : listPage([datasourceWire()], { total: 25 }),
    });
    renderList();

    // 先翻到第 2 页
    await user.click(await screen.findByRole("button", { name: "下一页" }));
    await waitFor(() =>
      expect(bodiesOf(calls, LIST_PATH).some((body) => body?.page === 2)).toBe(
        true,
      ),
    );

    await user.type(screen.getByLabelText("搜索数据源"), "zzz");

    // 是「没有匹配」，不是「还没有数据源」
    expect(
      await screen.findByRole("heading", { name: "没有匹配的数据源" }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "还没有数据源" })).toBeNull();
    // 结果集变了 → 必须回到第 1 页，否则会停在一个不存在的页码上
    const searched = bodiesOf(calls, LIST_PATH).filter(
      (body) => body?.search === "zzz",
    );
    expect(searched.length).toBeGreaterThan(0);
    for (const body of searched) expect(body?.page).toBe(1);

    // 工具栏保留（要能就地改搜索词），并给一个能脱困的动作
    await user.click(screen.getByRole("button", { name: "清除筛选" }));
    expect(await screen.findByText("部门")).toBeInTheDocument();
    expect(screen.getByLabelText("搜索数据源")).toHaveValue("");
  });
});

describe("飞书数据源列表页 · 权限门控", () => {
  it("无写权限：「添加数据源」与列表项操作菜单都不渲染（不是禁用）", async () => {
    stubFeishuApi({
      datasourceWrite: false,
      datasourceList: () =>
        listPage([
          datasourceWire(),
          datasourceWire({ source_key: "expense_category", title: "费用类型" }),
        ]),
    });
    renderList();

    expect(await screen.findByText("部门")).toBeInTheDocument();
    expect(screen.getByText("费用类型")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "添加数据源" })).toBeNull();
    expect(screen.queryByRole("button", { name: /的操作/ })).toBeNull();
  });

  it("有写权限：「添加数据源」与每行的操作菜单都在，且请求恒带非空 order_by", async () => {
    const calls = stubFeishuApi({
      datasourceList: () =>
        listPage([
          datasourceWire(),
          datasourceWire({ source_key: "expense_category", title: "费用类型" }),
        ]),
    });
    renderList();

    expect(
      await screen.findByRole("button", { name: "添加数据源" }),
    ).toBeInTheDocument();
    // 等数据落地再断言行内菜单（那之前是骨架行）
    expect(await screen.findByText("费用类型")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "部门 的操作" }),
    ).toBeInTheDocument();

    const bodies = bodiesOf(calls, LIST_PATH);
    expect(bodies.length).toBeGreaterThan(0);
    for (const body of bodies) {
      // 不发排序就是无序分页：翻页会重复或漏行
      expect(body?.order_by).toEqual([
        { field: "source_key", direction: "Asc" },
      ]);
    }
  });

  it("无读权限：整页只给一句说明，不发列表请求", async () => {
    const calls = stubFeishuApi({ datasourceRead: false });
    renderList();

    expect(
      await screen.findByText(/当前身份没有查看飞书数据源的权限/),
    ).toBeInTheDocument();
    // 没有读权限就不发那次注定 403 的请求
    expect(countCalls(calls, LIST_PATH)).toBe(0);
    // 整片列表 UI 都不渲染（不是渲染一个空列表）
    expect(screen.queryByLabelText("搜索数据源")).toBeNull();
    expect(screen.queryByRole("heading", { name: "还没有数据源" })).toBeNull();
  });
});

describe("飞书数据源列表页 · 双视图", () => {
  it("切视图只换渲染方式：不重新发请求、不丢搜索词", async () => {
    const user = userEvent.setup();
    const calls = stubFeishuApi({
      datasourceList: () => listPage([datasourceWire()]),
    });
    renderList();

    await screen.findByText("部门");
    await user.type(screen.getByLabelText("搜索数据源"), "部");
    await waitFor(() =>
      expect(
        bodiesOf(calls, LIST_PATH).some((body) => body?.search === "部"),
      ).toBe(true),
    );
    const before = countCalls(calls, LIST_PATH);

    await user.click(screen.getByRole("button", { name: "卡片" }));

    await waitFor(() =>
      expect(
        document.querySelector('[data-slot="datasource-card"]'),
      ).not.toBeNull(),
    );
    // 同一份查询状态、同一份数据：切视图不产生新请求
    expect(countCalls(calls, LIST_PATH)).toBe(before);
    expect(screen.getByLabelText("搜索数据源")).toHaveValue("部");
    // 视图选择被持久化（下次进来还是卡片）
    expect(localStorage.getItem("yang.feishu.datasource.view")).toBe("cards");
  });
});

describe("飞书数据源列表页 · 创建后的预检回执", () => {
  async function createFirstDatasource() {
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "添加数据源" }));
    const dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByLabelText("数据源标识"), "dept_sales");
    await user.type(within(dialog).getByLabelText("名称"), "部门");
    await user.type(within(dialog).getByLabelText("接口 Token"), "t-1");
    await user.click(
      within(dialog).getByRole("button", { name: "创建数据源" }),
    );
    return user;
  }

  it("预检通过：回执带真实标识与「粘回飞书审批后台」+ 复制入口", async () => {
    stubFeishuApi({
      // 回执的有效性以「这一行还在列表里」为准，所以桩里也要有它。
      datasourceList: () =>
        listPage([datasourceWire({ source_key: "dept_sales", title: "部门" })]),
      approvalOptions: () => ({ result: { options: [{ id: "a" }] } }),
    });
    renderList();
    await createFirstDatasource();

    expect(
      await screen.findByText(/把这一段粘回飞书审批后台/),
    ).toBeInTheDocument();
    expect(screen.getByText(/拉到了 1 个选项/)).toBeInTheDocument();
    // 回执里那一段可复制的标识（列表行里也有同一个标识，所以按元素定位）
    expect(
      screen.getByText("dept_sales", { selector: "code" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "复制" })).toBeInTheDocument();
    // 创建成功后对话框关闭
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("预检失败：说清数据源已创建、给出可归因码，并能就地重填 Token", async () => {
    const user = userEvent.setup();
    stubFeishuApi({
      datasourceList: () =>
        listPage([datasourceWire({ source_key: "dept_sales", title: "部门" })]),
      approvalOptions: () =>
        jsonResponse({ code: 40102, msg: "token 校验失败", data: null }),
    });
    renderList();
    await createFirstDatasource();

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("数据源已创建");
    expect(alert).toHaveTextContent("token 校验失败");
    expect(alert).toHaveTextContent("40102");
    expect(screen.getByText(/重填一次 Token/)).toBeInTheDocument();
    // 预检失败不阻塞创建结果：对话框已关，数据源已经就位
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    await user.click(screen.getByRole("button", { name: "重新填写 Token" }));
    const renameDialog = await screen.findByRole("dialog");
    expect(within(renameDialog).getByText("dept_sales")).toBeInTheDocument();
    expect(
      within(renameDialog).getByLabelText("轮换 Token（可留空）"),
    ).toBeInTheDocument();
  });

  it("删掉那个数据源后回执当场清掉：它的「重新填写 Token」会送进一个不存在的标识", async () => {
    const user = userEvent.setup();
    stubFeishuApi({
      datasourceList: () =>
        listPage([datasourceWire({ source_key: "dept_sales", title: "部门" })]),
      approvalOptions: () => ({ result: { options: [{ id: "a" }] } }),
    });
    renderList();
    await createFirstDatasource();

    expect(
      await screen.findByText(/把这一段粘回飞书审批后台/),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "部门 的操作" }));
    await user.click(await screen.findByRole("menuitem", { name: "删除" }));
    const confirm = await screen.findByRole("dialog");
    // 删除不可逆：确认框里必须看得见删的是哪一条
    expect(within(confirm).getByText("dept_sales")).toBeInTheDocument();
    await user.click(within(confirm).getByRole("button", { name: "删除" }));

    // 不能再挂着一句「数据源已创建」
    await waitFor(() =>
      expect(screen.queryByText(/把这一段粘回飞书审批后台/)).toBeNull(),
    );
  });

  it("回执所指的数据源已不在结果集里（别处删的）：回执同样不再渲染", async () => {
    const user = userEvent.setup();
    // 创建那一刻它还在（挂载 + 创建后回读各一次），随后别处删掉了它。
    let listCalls = 0;
    stubFeishuApi({
      // 只数**列表页自己**的读取。父级候选那一份带 page_size=100（与页面的分页无关），
      // 它不影响这里要断言的「回读之后结果集里还有没有那条」。
      datasourceList: (body) => {
        if (body.page_size === 100) return listPage([]);
        listCalls += 1;
        return listPage(
          listCalls <= 2
            ? [datasourceWire({ source_key: "dept_sales", title: "部门" })]
            : [],
        );
      },
      approvalOptions: () => ({ result: { options: [{ id: "a" }] } }),
    });
    renderList();
    await createFirstDatasource();

    expect(
      await screen.findByText(/把这一段粘回飞书审批后台/),
    ).toBeInTheDocument();

    // 换一次排序让列表再次回读（不是筛选，所以上面那份结果集就是全部）：
    // 整份结果集里已经没有它了，回执就失去了指向。
    await user.click(screen.getByRole("button", { name: "按名称排序" }));
    await waitFor(() =>
      expect(screen.queryByText(/把这一段粘回飞书审批后台/)).toBeNull(),
    );
  });

  it("服务端拒绝创建时，后端原文回显在对话框里且不出现预检回执", async () => {
    stubFeishuApi({
      datasourceList: () => listPage([]),
      createDatasource: () =>
        jsonResponse({ code: 400001, message: "数据源标识已存在" }, 400),
    });
    renderList();
    await createFirstDatasource();

    const dialog = await screen.findByRole("dialog");
    await waitFor(() =>
      expect(within(dialog).getByRole("alert")).toHaveTextContent(
        "数据源标识已存在",
      ),
    );
    expect(screen.queryByText(/把这一段粘回飞书审批后台/)).toBeNull();
  });
});

describe("飞书数据源列表页 · 编辑入口能改服务端开关", () => {
  it("默认语言选错后必须有一条修复路径：现值来自那一行，改动随 PUT 发出", async () => {
    const user = userEvent.setup();
    const calls = stubFeishuApi({
      datasourceList: () =>
        listPage([
          datasourceWire({
            source_key: "dept_sales",
            title: "部门",
            encrypt_enabled: true,
            default_locale: "zh_cn",
          }),
        ]),
    });
    renderList();

    await user.click(
      await screen.findByRole("button", { name: "部门 的操作" }),
    );
    await user.click(await screen.findByRole("menuitem", { name: "重命名" }));

    const dialog = await screen.findByRole("dialog");
    // 初值来自那一行，而不是控件的缺省值
    expect(
      within(dialog).getByRole("checkbox", { name: /加密返回/ }),
    ).toBeChecked();
    expect(
      within(dialog).getByRole("combobox", { name: "默认语言" }),
    ).toHaveTextContent("简体中文");

    await user.click(
      within(dialog).getByRole("combobox", { name: "默认语言" }),
    );
    await user.click(await screen.findByRole("option", { name: "日本語" }));
    await user.click(within(dialog).getByRole("button", { name: "保存" }));

    await waitFor(() => {
      const put = calls.find((call) => call.method === "PUT");
      expect(put?.body).toMatchObject({
        source_key: "dept_sales",
        title: "部门",
        encrypt_enabled: true,
        default_locale: "ja_jp",
      });
    });
  });

  it("行里的 default_locale 是取值域外的值（zh-CN）：留空让用户显式改，不静默替换", async () => {
    // 后端对它零校验。把认不出的值静默换成 zh_cn 再写回去，等于把
    // 「所有语言下控件都取不到文案」的元凶悄悄改掉——所以这一项留空，由用户显式选。
    const user = userEvent.setup();
    const calls = stubFeishuApi({
      datasourceList: () =>
        listPage([
          datasourceWire({
            source_key: "dept_sales",
            title: "部门",
            default_locale: "zh-CN",
          }),
        ]),
    });
    renderList();

    await user.click(
      await screen.findByRole("button", { name: "部门 的操作" }),
    );
    await user.click(await screen.findByRole("menuitem", { name: "重命名" }));

    const dialog = await screen.findByRole("dialog");
    // 修复入口要在，而且要说清当前值为什么不合法
    expect(
      within(dialog).getByRole("combobox", { name: "默认语言" }),
    ).toBeInTheDocument();
    expect(within(dialog).getByText(/「zh-CN」/)).toBeInTheDocument();
    expect(within(dialog).getByText(/不在取值域内/)).toBeInTheDocument();

    // 不动它直接保存：不发 default_locale（省略 = 保持原值）
    await user.click(within(dialog).getByRole("button", { name: "保存" }));
    await waitFor(() => {
      const put = calls.find((call) => call.method === "PUT");
      expect(put?.body).not.toHaveProperty("default_locale");
    });
  });

  it("显式选一项：取值域外的值就被改成合法值（这就是修复入口）", async () => {
    const user = userEvent.setup();
    const calls = stubFeishuApi({
      datasourceList: () =>
        listPage([
          datasourceWire({
            source_key: "dept_sales",
            title: "部门",
            default_locale: "zh-CN",
          }),
        ]),
    });
    renderList();

    await user.click(
      await screen.findByRole("button", { name: "部门 的操作" }),
    );
    await user.click(await screen.findByRole("menuitem", { name: "重命名" }));
    const dialog = await screen.findByRole("dialog");

    await user.click(
      within(dialog).getByRole("combobox", { name: "默认语言" }),
    );
    await user.click(await screen.findByRole("option", { name: "简体中文" }));
    await user.click(within(dialog).getByRole("button", { name: "保存" }));

    await waitFor(() => {
      const put = calls.find((call) => call.method === "PUT");
      expect(put?.body).toMatchObject({ default_locale: "zh_cn" });
    });
  });
});

/// 结果集收缩之后，「当前页空了」与「真的没有匹配」是两件事。这一组用例盯的就是
/// 页面有没有把前者说成后者，以及页码有没有归位。
describe("飞书数据源列表页 · 结果集收缩后的页码与结论", () => {
  it("第 2 页唯一一行被停用后：不出现「没有匹配的数据源」，页码归位到有效页", async () => {
    const user = userEvent.setup();
    const rows = makeRows(11);
    const calls = stubFeishuApi({
      datasourceList: (body) => pageRows(rows, body),
      updateDatasource: (body) => {
        const row = rows.find(
          (candidate) => candidate.source_key === body.source_key,
        );
        if (row) row.status = body.status;
        return { affected: 1 };
      },
    });
    renderList();

    // 只看「启用」：11 条 → 第 1 页 10 行、第 2 页 1 行
    await screen.findByText("数据源 01");
    await user.click(screen.getByRole("button", { name: "启用" }));
    expect(await screen.findByText("数据源 01")).toBeInTheDocument();
    expect(screen.getByText(/共 11 个 · 第 1 \/ 2 页/)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "下一页" }));
    expect(await screen.findByText("数据源 11")).toBeInTheDocument();

    const before = calls.length;
    // 停用它：这一行离开「启用」这个结果集，第 2 页随之不复存在
    await user.click(screen.getByRole("button", { name: "数据源 11 的操作" }));
    await user.click(await screen.findByRole("menuitem", { name: "停用" }));
    await user.click(await screen.findByRole("button", { name: "停用" }));

    // 结果集缩到 10：页码必须归位；「这一页空了」不等于「没有匹配」
    expect(
      await screen.findByText(/共 10 个 · 第 1 \/ 1 页/),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "没有匹配的数据源" }),
    ).toBeNull();
    expect(screen.queryByRole("heading", { name: "还没有数据源" })).toBeNull();
    expect(screen.queryByText("数据源 11")).toBeNull();
    // 归位是真的回读了第 1 页，而不是停在那个已经不存在的页码上
    expect(
      listBodiesAfter(calls, before).some((body) => body?.page === 1),
    ).toBe(true);
  });

  it("重命名后不再命中搜索词：同样回第 1 页，不把空页说成「没有匹配」", async () => {
    const user = userEvent.setup();
    const rows = makeRows(11);
    stubFeishuApi({
      datasourceList: (body) => pageRows(rows, body),
      updateDatasource: (body) => {
        const row = rows.find(
          (candidate) => candidate.source_key === body.source_key,
        );
        if (row && typeof body.title === "string") row.title = body.title;
        return { affected: 1 };
      },
    });
    renderList();

    // 搜索「数据源」命中 11 条 → 第 2 页只有 1 行
    await user.type(await screen.findByLabelText("搜索数据源"), "数据源");
    expect(await screen.findByText("数据源 01")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "下一页" }));
    expect(await screen.findByText("数据源 11")).toBeInTheDocument();

    // 把它改成不再命中搜索词的名字
    await user.click(screen.getByRole("button", { name: "数据源 11 的操作" }));
    await user.click(await screen.findByRole("menuitem", { name: "重命名" }));
    const dialog = await screen.findByRole("dialog");
    const titleInput = within(dialog).getByLabelText("名称");
    await user.clear(titleInput);
    await user.type(titleInput, "部门");
    await user.click(within(dialog).getByRole("button", { name: "保存" }));

    // 命中数缩到 10：页码归位，页面不冒充「没有匹配」
    expect(
      await screen.findByText(/共 10 个 · 第 1 \/ 1 页/),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "没有匹配的数据源" }),
    ).toBeNull();
    expect(screen.queryByText("数据源 11")).toBeNull();
  });

  it("当前页越界（别处改过数据后回读）：夹回有效页，不给任何空态结论", async () => {
    const user = userEvent.setup();
    const rows = makeRows(11);
    const calls = stubFeishuApi({
      datasourceList: (body) => pageRows(rows, body),
    });
    renderList();

    expect(await screen.findByText("数据源 01")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "下一页" }));
    expect(await screen.findByText("数据源 11")).toBeInTheDocument();

    // 别处删掉了一行：结果集缩到 10，而我们仍停在已经越界的第 2 页。
    rows.pop();
    // 点列头只换排序、不动页码，正好让这一页重新回读一次（排序不改变结果集大小）。
    const before = calls.length;
    await user.click(screen.getByRole("button", { name: "按名称排序" }));

    // 拿到 total 与每页条数就能算出最后一页（10 条 → 第 1 页），夹回去即可。
    expect(
      await screen.findByText(/共 10 个 · 第 1 \/ 1 页/),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "没有匹配的数据源" }),
    ).toBeNull();
    expect(screen.queryByRole("heading", { name: "还没有数据源" })).toBeNull();
    const afterSort = listBodiesAfter(calls, before);
    // 先回读了越界的那一页（这是回读本身），再夹回第 1 页
    expect(afterSort.some((body) => body?.page === 2)).toBe(true);
    expect(afterSort.some((body) => body?.page === 1)).toBe(true);
  });

  it("有筛选、当前页为空但 total>0：给的是翻页入口，不是「没有匹配」", async () => {
    const user = userEvent.setup();
    stubFeishuApi({
      // 「已停用」这一支：后端说这个筛选下有 7 条，却一行都没给这一页。
      // 页面只能证明「这一页是空的」，证明不了「没有匹配」——所以那个结论不能出现。
      datasourceList: (body) =>
        asWhereValue(body.where) === null
          ? listPage(makeRows(3), { total: 3 })
          : listPage([], { total: 7 }),
    });
    renderList();

    expect(await screen.findByText("数据源 01")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "已停用" }));

    // 分页控件由 total 决定是否渲染，不看当前页有几行——没有它，用户连页码都看不到
    expect(
      await screen.findByText(/共 7 个 · 第 1 \/ 1 页/),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "没有匹配的数据源" }),
    ).toBeNull();
    expect(screen.queryByRole("heading", { name: "还没有数据源" })).toBeNull();
  });

  it("total>0 时不冒充「一个数据源都没有」：只有 total 为 0 才给四步建档", async () => {
    // 当前页一行都没有，但后端说总共还有 3 条——「这一页为空」证明不了「一个都没有」。
    stubFeishuApi({ datasourceList: () => listPage([], { total: 3 }) });
    renderList();

    expect(await screen.findByText(/共 3 个/)).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "还没有数据源" })).toBeNull();
    // 工具栏要留着：四步建档是「别的分支」才有的事，那之前不该把整屏交出去
    expect(screen.getByLabelText("搜索数据源")).toBeInTheDocument();
  });

  it("切回「全部」的那一帧拿的是 placeholderData：不判空、不闪四步建档", async () => {
    const user = userEvent.setup();
    const rows = makeRows(11);
    const calls = stubFeishuApi({
      datasourceList: (body) => {
        // 「已停用」这一支：后端确实一条都没有
        if (asWhereValue(body.where) !== null) return listPage([]);
        // 「全部 + 名称排序 + 第 1 页」这一跳故意不返回：停在 placeholderData 那一帧上观察
        if (body.page === 1 && orderedByTitle(body.order_by)) {
          return new Promise<never>(() => {});
        }
        return pageRows(rows, body);
      },
    });
    renderList();

    // 先在第 2 页点一次列头排序：「全部 + 名称排序 + 第 1 页」从此是个没取过的键
    expect(await screen.findByText("数据源 01")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "下一页" }));
    expect(await screen.findByText("数据源 11")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "按名称排序" }));
    await waitFor(() =>
      expect(
        bodiesOf(calls, LIST_PATH).some(
          (body) => body?.page === 2 && orderedByTitle(body?.order_by),
        ),
      ).toBe(true),
    );

    // 切到「已停用」：这次是真的没有匹配，页面就该这么说
    await user.click(screen.getByRole("button", { name: "已停用" }));
    expect(
      await screen.findByRole("heading", { name: "没有匹配的数据源" }),
    ).toBeInTheDocument();

    // 切回「全部」：新键的请求发出去了但还没有结果，此刻 items 仍是上一把的空数组
    await user.click(screen.getByRole("button", { name: "全部" }));
    await waitFor(() =>
      expect(
        bodiesOf(calls, LIST_PATH).some(
          (body) =>
            asWhereValue(body?.where) === null &&
            body?.page === 1 &&
            orderedByTitle(body?.order_by),
        ),
      ).toBe(true),
    );
    // placeholderData 不属于当前查询键，它证明不了任何空态
    expect(screen.queryByRole("heading", { name: "还没有数据源" })).toBeNull();
    expect(
      screen.queryByRole("heading", { name: "没有匹配的数据源" }),
    ).toBeNull();
    // 工具栏不能被藏掉（旧实现会在这一帧把整屏换成四步建档）
    expect(screen.getByLabelText("搜索数据源")).toBeInTheDocument();
  });

  it("新键在 placeholder 之后落定为 total=0：空态照样给四步建档，不会停在上一把的行上", async () => {
    // 上面那条钉的是「placeholder 那一帧不许判空」；这条钉的是它的另一面——
    // 别把 settled 做成「一律不判空」：新键真的落定为 0 条时，四步建档必须出现。
    // 排序不改变结果集大小，也就不会置起 filtered，所以走的是「一个数据源都没有」那一支。
    const user = userEvent.setup();
    stubFeishuApi({
      datasourceList: (body) =>
        orderedByTitle(body.order_by)
          ? listPage([]) // 新键：服务端确实一条都没有
          : listPage([datasourceWire()]), // 旧键：有一行，正好当 placeholder
    });
    renderList();

    expect(await screen.findByText("部门")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "按名称排序" }));

    expect(
      await screen.findByRole("heading", { name: "还没有数据源" }),
    ).toBeInTheDocument();
    // 上一把的行必须让位：它不属于这个查询键
    expect(screen.queryByText("部门")).toBeNull();
    expect(screen.getByText("在这里建一个数据源")).toBeInTheDocument();
    expect(screen.queryByLabelText("搜索数据源")).toBeNull();
  });
});

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
      datasourceList: () => listPage([]),
      approvalOptions: () => ({ result: { options: [{ id: "a" }] } }),
    });
    renderList();
    await createFirstDatasource();

    expect(
      await screen.findByText(/把这一段粘回飞书审批后台/),
    ).toBeInTheDocument();
    expect(screen.getByText(/拉到了 1 个选项/)).toBeInTheDocument();
    expect(screen.getByText("dept_sales")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "复制" })).toBeInTheDocument();
    // 创建成功后对话框关闭
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("预检失败：说清数据源已创建、给出可归因码，并能就地重填 Token", async () => {
    const user = userEvent.setup();
    stubFeishuApi({
      datasourceList: () => listPage([]),
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

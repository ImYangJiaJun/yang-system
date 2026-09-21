import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { renderTestApp } from "@test/helpers/render-app";

import {
  bodiesOf,
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

  it("点「最近推送」列头切换排序方向", async () => {
    const user = userEvent.setup();
    const calls = stubFeishuApi({ optionList: () => listPage(TWO_OPTIONS) });
    renderDetail();

    await user.click(
      await screen.findByRole("button", { name: "按最近推送排序" }),
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

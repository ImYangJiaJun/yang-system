import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/api/auth-session";
import { renderTestApp } from "@/test/render-app";

import addFixture from "@/test/fixtures/demo-items-add.json";
import listFixture from "@/test/fixtures/demo-items-list.json";
import optionsFixture from "@/test/fixtures/demo-category-options.json";
import catalogFixture from "@/test/fixtures/ui-catalog.json";

/**
 * 垂直切片集成测试：Catalog 拉取（实录 fixture）→ 导航投影 →
 * 通用模块页表格渲染 → 打开「新增项目」对话框 → 填表 → 提交 →
 * 断言请求方法/路径/body 与演示后端契约一致。
 */

function jsonResponse(payload: unknown, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

type CapturedRequest = { url: string; method: string; body: unknown };

function installFetchMock() {
  const captured: CapturedRequest[] = [];
  const fetchMock = vi.fn(
    async (input: RequestInfo | URL, init?: RequestInit) => {
      const url =
        typeof input === "string"
          ? input
          : input instanceof URL
            ? input.href
            : input.url;
      if (url.includes("/.well-known/yang/ui-catalog")) {
        return jsonResponse(catalogFixture);
      }
      if (url.includes("/api/v1/demo/items/query")) {
        captured.push({
          url,
          method: init?.method ?? "GET",
          body: init?.body ? JSON.parse(String(init.body)) : undefined,
        });
        return jsonResponse(listFixture);
      }
      if (url.includes("/api/v1/demo/categories/options")) {
        return jsonResponse(optionsFixture);
      }
      if (url.endsWith("/api/v1/demo/items") && init?.method === "POST") {
        captured.push({
          url,
          method: "POST",
          body: JSON.parse(String(init.body)),
        });
        return jsonResponse(addFixture);
      }
      throw new Error(`测试未覆盖的请求：${init?.method ?? "GET"} ${url}`);
    },
  );
  vi.stubGlobal("fetch", fetchMock);
  return { fetchMock, captured };
}

function renderApp(
  initialPath: string,
  options: { authenticated?: boolean } = {},
) {
  // 门控路由要求认证会话；默认直接注入内存 Token（不走登录流程）。
  return renderTestApp({
    path: initialPath,
    authenticated: options.authenticated ?? true,
  });
}

beforeEach(() => {
  // 屏蔽 jsdom 中 Radix 组件依赖但与本测试无关的 API。
  Element.prototype.scrollIntoView ??= () => undefined;
});

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  // 清空 auth-session 模块级内存 Token，避免用例间串扰。
  clearStoredSession();
});

describe("模块页垂直切片", () => {
  it("Catalog → 应用中心 → 业务页 → 表格行渲染（含关系标签与树缩进）", async () => {
    installFetchMock();
    renderApp("/");

    // “/” 是应用中心（Dashboard）；未分配视图以卡片入口出现。
    expect(
      await screen.findByRole("heading", { name: "应用中心", level: 1 }),
    ).toBeInTheDocument();
    const card = await screen.findByTestId("view-card-demo.items.main");

    // 点击进入业务页，通用表格渲染树形行。
    const user = userEvent.setup();
    await user.click(card);
    await waitFor(() =>
      expect(screen.getByText("平台能力")).toBeInTheDocument(),
    );
    expect(screen.getByText("通用渲染器")).toBeInTheDocument();

    // 关系列把 category_id 翻译成选项标签。
    await waitFor(() => {
      expect(screen.getByText("平台")).toBeInTheDocument();
      expect(screen.getByText("业务")).toBeInTheDocument();
    });
  });

  it("新增 Action：对话框表单 → Ajv 校验 → 提交方法与路径与 body 正确", async () => {
    const { captured } = installFetchMock();
    renderApp("/m/demo.items.main");

    await screen.findByRole("heading", { name: "项目目录", level: 1 });
    await screen.findByText("平台能力");

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "新增项目" }));

    const dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByLabelText(/名称/), "集成测试项目");
    // 分类是 relation_select（Radix Select）：等待远程选项加载后打开并选「业务」。
    const categoryTrigger = within(dialog).getByRole("combobox", {
      name: "分类",
    });
    await user.click(categoryTrigger);
    await user.click(await screen.findByRole("option", { name: "业务" }));
    await user.type(within(dialog).getByLabelText(/状态/), "active");

    await user.click(within(dialog).getByRole("button", { name: "提交" }));

    // 断言提交请求的方法、路径与 body。
    // parent_id 默认值为 null，按旧 action-request 语义 null 视为缺省不下发。
    await waitFor(() => {
      const add = captured.find(
        (request) => request.url === "/api/v1/demo/items",
      );
      expect(add).toBeDefined();
      expect(add?.method).toBe("POST");
      expect(add?.body).toEqual({
        name: "集成测试项目",
        category_id: 2,
        status: "active",
      });
    });

    // 成功后关闭对话框、提示成功并刷新表格。
    await screen.findByRole("status");
    expect(screen.getByRole("status")).toHaveTextContent("成功");
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(
      captured.filter((request) =>
        request.url.includes("/api/v1/demo/items/query"),
      ).length,
    ).toBeGreaterThanOrEqual(2);
  });

  it("Ajv 校验失败时阻止提交并把错误映射到字段", async () => {
    const { captured } = installFetchMock();
    renderApp("/m/demo.items.main");

    await screen.findByText("平台能力");
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "新增项目" }));
    const dialog = await screen.findByRole("dialog");

    // 只填名称，缺 category_id / status（required），提交必须被拦截。
    await user.type(within(dialog).getByLabelText(/名称/), "缺字段项目");
    await user.click(within(dialog).getByRole("button", { name: "提交" }));

    await waitFor(() => {
      expect(
        within(dialog).getAllByRole("alert").length,
      ).toBeGreaterThanOrEqual(1);
    });
    expect(
      captured.find((request) => request.url === "/api/v1/demo/items"),
    ).toBeUndefined();
  });
});

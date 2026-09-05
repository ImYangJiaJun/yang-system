import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { renderTestApp } from "@test/helpers/render-app";

import listFixture from "@test/fixtures/demo-items-list.json";
import optionsFixture from "@test/fixtures/demo-category-options.json";
import catalogFixture from "@test/fixtures/ui-catalog.json";

/// custom view 注册表链路：工具栏「项目洞察」（interaction=custom）→ 静态注册表解析 → 自定义页渲染/返回。

function jsonResponse(payload: unknown, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

beforeEach(() => {
  Element.prototype.scrollIntoView ??= () => undefined;
});

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

describe("custom view 注册表", () => {
  it("项目洞察经静态注册表加载自定义页，可返回通用表格", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = typeof input === "string" ? input : input.toString();
        if (url.includes("/.well-known/yang/ui-catalog")) {
          return jsonResponse(catalogFixture);
        }
        if (url.includes("/api/v1/demo/items/insight")) {
          return jsonResponse({
            code: 0,
            message: "成功",
            data: { total: 2, active: 1, draft: 1 },
          });
        }
        if (url.includes("/api/v1/demo/items/query")) {
          return jsonResponse(listFixture);
        }
        if (url.includes("/api/v1/demo/categories/options")) {
          return jsonResponse(optionsFixture);
        }
        throw new Error(`测试未覆盖的请求：${url}`);
      }),
    );
    renderTestApp({ path: "/m/demo.items.main", authenticated: true });

    await waitFor(
      () => expect(screen.getByText("平台能力")).toBeInTheDocument(),
      { timeout: 5000 },
    );
    const user = userEvent.setup();
    // insight 是 toolbar 次按钮（direct secondary）。
    await user.click(screen.getByRole("button", { name: "项目洞察" }));

    // 自定义页替换通用表格，展示洞察指标（lazy chunk + Action 调用，放宽超时）。
    expect(
      await screen.findByText("项目运行洞察", undefined, { timeout: 5000 }),
    ).toBeInTheDocument();
    expect(
      await screen.findByText("项目总数", undefined, { timeout: 5000 }),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "返回通用表格" }));
    await waitFor(
      () => expect(screen.getByText("平台能力")).toBeInTheDocument(),
      { timeout: 5000 },
    );
  });

  it("未注册的 custom view_id 回退通用表格并提示", async () => {
    const catalog = JSON.parse(JSON.stringify(catalogFixture)) as unknown;
    // 把 insight 的 view_id 改成未注册值；revision 必须换成新的内容地址，
    // 否则命中进程内 CatalogCache 返回上一用例的目录。
    const doc = catalog as {
      data: {
        revision: string;
        table_views: Array<{
          action_presentations: Array<Record<string, unknown>>;
        }>;
      };
    };
    doc.data.revision = "9".repeat(64);
    for (const view of doc.data.table_views) {
      for (const presentation of view.action_presentations) {
        if (presentation.operation_id === "demo.items.insight") {
          presentation.view_id = "demo.items.unregistered";
        }
      }
    }
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = typeof input === "string" ? input : input.toString();
        if (url.includes("/.well-known/yang/ui-catalog")) {
          return jsonResponse(doc);
        }
        if (url.includes("/api/v1/demo/items/query")) {
          return jsonResponse(listFixture);
        }
        if (url.includes("/api/v1/demo/categories/options")) {
          return jsonResponse(optionsFixture);
        }
        throw new Error(`测试未覆盖的请求：${url}`);
      }),
    );
    renderTestApp({ path: "/m/demo.items.main", authenticated: true });

    await waitFor(
      () => expect(screen.getByText("平台能力")).toBeInTheDocument(),
      { timeout: 5000 },
    );
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "项目洞察" }));

    expect(
      await screen.findByText(/未注册，已保留通用模块页/, undefined, {
        timeout: 5000,
      }),
    ).toBeInTheDocument();
    // 通用表格仍在。
    expect(screen.getByText("平台能力")).toBeInTheDocument();
  });
});

describe("开发工作台", () => {
  it("DEV 构建下 /workbench 可达并列出目录内容", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = typeof input === "string" ? input : input.toString();
        if (url.includes("/.well-known/yang/ui-catalog")) {
          return jsonResponse(catalogFixture);
        }
        if (url.includes("/api/v1/demo/items/query")) {
          return jsonResponse(listFixture);
        }
        if (url.includes("/api/v1/demo/categories/options")) {
          return jsonResponse(optionsFixture);
        }
        throw new Error(`测试未覆盖的请求：${url}`);
      }),
    );
    renderTestApp({ path: "/workbench", authenticated: true });

    // 业务页面模式：默认选中第一个视图并渲染通用表格。
    await waitFor(
      () => expect(screen.getByText("平台能力")).toBeInTheDocument(),
      { timeout: 5000 },
    );
    expect(screen.getByRole("tab", { name: "业务页面" })).toBeInTheDocument();

    // 接口演示模式：列出全部 Action，选中「回显输入」进入调试面板。
    const user = userEvent.setup();
    await user.click(screen.getByRole("tab", { name: "接口演示" }));
    await user.click(
      await screen.findByRole(
        "button",
        { name: "回显输入" },
        { timeout: 5000 },
      ),
    );
    expect(
      await screen.findByRole(
        "heading",
        { name: "回显输入" },
        { timeout: 5000 },
      ),
    ).toBeInTheDocument();
  });
});

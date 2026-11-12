import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { renderTestApp } from "@test/helpers/render-app";

// 全量测试并行加载 antd 时首屏可能超过 30s，单独跑只要数秒。
vi.setConfig({ testTimeout: 90_000, hookTimeout: 90_000 });

function jsonResponse(payload: unknown, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function catalogResponse() {
  return jsonResponse({
    code: 0,
    message: "成功",
    data: {
      schema_version: "2.4",
      revision: "e".repeat(64),
      actions: [],
      table_views: [],
      modules: [
        {
          module_id: "feishu.batch_approval",
          identity: {
            id: "user",
            title: "个人账户",
            icon: "person",
            order: 10,
          },
          title: "批量审批",
          description: "",
          icon: "batch_approval",
          order: 20,
          app_route: "/batch-approval",
          actions: [],
          action_presentations: [],
          views: [],
        },
      ],
    },
  });
}

const CONFIGS = [
  {
    id: 7,
    approval_code: "D1",
    name: "参考人民币金额",
    left_field_id: "amount",
    left_field_name: "原币金额",
    operator: "multiply",
    right_field_id: "rate",
    right_field_name: "汇率",
    precision: 2,
    enabled: true,
    sort_order: 0,
  },
];

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("批量审批计算字段配置", () => {
  it("直接列出已配置字段，并支持打开新增表单", async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("ui-catalog")) return catalogResponse();
      if (url.includes("/computed-fields")) {
        return jsonResponse({
          code: 0,
          message: "ok",
          data: { fields: CONFIGS },
        });
      }
      return jsonResponse({ code: 0, message: "ok", data: {} });
    });
    vi.stubGlobal("fetch", fetchMock);

    const user = userEvent.setup();
    renderTestApp({
      path: "/feishu/approval/computed-fields",
      authenticated: true,
      identity: "admin",
    });

    // 全量并发时路由 chunk 首次加载可能超过 find 默认的 1 秒。
    await screen.findByRole(
      "heading",
      { name: "批量审批计算字段" },
      { timeout: 15_000 },
    );
    expect(
      await screen.findByText("参考人民币金额", {}, { timeout: 15_000 }),
    ).toBeInTheDocument();
    expect(screen.getByText("原币金额")).toBeInTheDocument();
    expect(screen.getByText("汇率")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /新增字段/ }));
    expect(await screen.findByText("新增计算字段")).toBeInTheDocument();
    expect(screen.getByText(/汇率即使来自外部数据源/)).toBeInTheDocument();
  });

  it("加载失败时显示错误而不假装为空", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes("ui-catalog")) return catalogResponse();
        return jsonResponse({ code: 50001, message: "读取配置失败" }, 500);
      }),
    );

    renderTestApp({
      path: "/feishu/approval/computed-fields",
      authenticated: true,
      identity: "admin",
    });

    await waitFor(() => {
      expect(screen.getByText("加载计算字段失败")).toBeInTheDocument();
    });
    expect(screen.getByText(/读取配置失败/)).toBeInTheDocument();
  });
});

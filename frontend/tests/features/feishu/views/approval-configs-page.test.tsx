/**
 * 审批派发配置页（`/feishu/approval/configs`，走真实路由含 lazy 的 `.default` 约定）。
 *
 * 钉住四件事：
 * - 列表投影（名称/坐标/Code/时区/启用/快照/更新）；
 * - 权限门控：无 list_configs 时侧边栏入口不渲染、页面给权限说明；
 * - 无写权限：行操作整组不渲染（只读行），「新建配置」不出现；
 * - 行操作：启停翻转 enabled、删除确认（成功 toast + 回读）、映射明细展开。
 */

import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { renderTestApp } from "@test/helpers/render-app";

import {
  approvalConfigWire,
  bodiesOf,
  countCalls,
  listPage,
  stubApprovalApi,
} from "./approval-harness";

const CONFIG_LIST_PATH = "/api/v1/feishu/approval/configs/query";
const CONFIG_UPDATE_PATH = "/api/v1/feishu/approval/configs/update";
const CONFIG_DELETE_PATH = "/api/v1/feishu/approval/configs/delete";

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

function renderConfigs() {
  return renderTestApp({
    path: "/feishu/approval/configs",
    authenticated: true,
  });
}

function configRows() {
  return [
    approvalConfigWire(),
    approvalConfigWire({
      id: 2,
      title: "费用报销",
      approval_code: "CODE-EXPENSE",
      enabled: false,
      maps: [
        {
          widget_id: "widget-1",
          widget_type: "input",
          bitable_field: "fldA",
          bitable_field_name: "金额",
          required: true,
          converter: "direct",
        },
      ],
    }),
  ];
}

describe("审批派发配置页 · 列表", () => {
  it("空列表也能展开工作流指引，提供真实接口、参数与单行/批量示例", async () => {
    const calls = stubApprovalApi({ approvalWrite: false });
    renderConfigs();
    await screen.findByRole("heading", { name: "审批派发" });

    const summary = screen.getByText("多维表格工作流配置指引");
    const guide = summary.closest("details")!;
    expect(guide).not.toHaveAttribute("open");
    await userEvent.click(summary);
    expect(guide).toHaveAttribute("open");

    const content = within(guide);
    expect(
      content.getByText(
        `${window.location.origin}/api/v1/feishu/approval/dispatch`,
      ),
    ).toBeInTheDocument();
    expect(
      content.getByText(/Authorization: Bearer <管理 Token>/),
    ).toBeInTheDocument();
    expect(
      content.getByText(/Content-Type: application\/json/),
    ).toBeInTheDocument();
    for (const name of [
      "base_token",
      "table_id",
      "record_id",
      "requested_by",
      "approval_code",
      "applicant_field",
      "backfill_field",
    ]) {
      expect(
        content.getByText(name, { selector: "dt code" }),
      ).toBeInTheDocument();
    }
    const single = JSON.parse(
      content.getByLabelText("单行派发请求体").textContent!,
    );
    const batch = JSON.parse(
      content.getByLabelText("批量派发请求体").textContent!,
    );
    expect(single).toEqual({
      base_token: "<多维表格 Token>",
      table_id: "<数据表 ID>",
      record_id: "<触发记录 ID>",
      requested_by: "<触发人标识>",
    });
    expect(batch).toEqual({
      base_token: single.base_token,
      table_id: single.table_id,
      requested_by: single.requested_by,
    });
    expect(content.getByText(/所有启用配置的待处理队列/)).toBeInTheDocument();
    expect(
      content.getByText(/feishu.management_api_token/),
    ).toBeInTheDocument();
    expect(content.getByText(/data.accepted/)).toBeInTheDocument();
    expect(countCalls(calls, "/api/v1/feishu/approval/dispatch")).toBe(0);

    await userEvent.click(summary);
    expect(guide).not.toHaveAttribute("open");
  });

  it("渲染列表投影：名称/坐标/Code/时区/启用/快照时间/更新时间", async () => {
    stubApprovalApi({
      configList: () => listPage(configRows()),
    });
    renderConfigs();

    expect(await screen.findByText("差旅报销")).toBeInTheDocument();
    expect(await screen.findByText("费用报销")).toBeInTheDocument();
    // 坐标：base_token / table_id 同一格
    expect(screen.getAllByText(/appbcbWCzen6 \/ tblsRc9GRRX/).length).toBe(2);
    // Code 与时区
    expect(screen.getByText("CODE-TRAVEL")).toBeInTheDocument();
    expect(screen.getByText("CODE-EXPENSE")).toBeInTheDocument();
    expect(screen.getAllByText("Asia/Shanghai").length).toBe(2);
    // 启用徽标（「启用」同时是停用行的行操作按钮名，用 getAll 取徽标）
    expect(screen.getAllByText("启用").length).toBeGreaterThan(0);
    expect(screen.getByText("已停用")).toBeInTheDocument();
    // 快照/更新时间（unix 秒 → 本地文本）
    expect(screen.getAllByText(/2025/).length).toBeGreaterThan(0);
  });

  it("列表请求恒带确定性排序（updated_at Desc + id Asc 收尾）", async () => {
    const calls = stubApprovalApi({
      configList: () => listPage(configRows()),
    });
    renderConfigs();
    await screen.findByText("差旅报销");

    for (const body of bodiesOf(calls, CONFIG_LIST_PATH)) {
      expect(body?.order_by).toEqual([
        { field: "updated_at", direction: "Desc" },
        { field: "id", direction: "Asc" },
      ]);
      expect(body?.count_total).toBe(true);
    }
  });
});

describe("审批派发配置页 · 权限门控", () => {
  it("无读权限：侧边栏「审批派发」入口不渲染，页面给权限说明", async () => {
    const calls = stubApprovalApi({ approvalRead: false });
    renderConfigs();

    expect(
      await screen.findByText(/当前身份没有查看审批派发配置的权限/),
    ).toBeInTheDocument();
    // 侧边栏入口不渲染（门控照 canReadFeishuDatasources 模式）
    expect(screen.queryByRole("link", { name: "审批派发" })).toBeNull();
    // 不发那次注定 403 的请求
    expect(countCalls(calls, CONFIG_LIST_PATH)).toBe(0);
  });

  it("有读无写：「新建配置」与行操作整组不渲染（不是禁用）", async () => {
    stubApprovalApi({
      approvalWrite: false,
      configList: () => listPage(configRows()),
    });
    renderConfigs();

    expect(await screen.findByText("差旅报销")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "新建配置" })).toBeNull();
    expect(screen.queryByRole("button", { name: "删除" })).toBeNull();
    expect(screen.queryByRole("button", { name: "启用" })).toBeNull();
    // 行尾给「只读」标注
    expect(screen.getAllByText("只读").length).toBe(2);
  });

  it("有读有写：入口与行操作都在", async () => {
    stubApprovalApi({
      configList: () => listPage(configRows()),
    });
    renderConfigs();

    expect(
      await screen.findByRole("button", { name: "新建配置" }),
    ).toBeInTheDocument();
    expect(await screen.findByText("费用报销")).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "启用" }).length).toBe(1);
    expect(screen.getAllByRole("button", { name: "停用" }).length).toBe(1);
    expect(screen.getAllByRole("button", { name: "删除" }).length).toBe(2);
  });
});

describe("审批派发配置页 · 行操作", () => {
  it("启停：调用 update_config 翻转 enabled，成功后回读列表", async () => {
    const calls = stubApprovalApi({
      configList: () => listPage(configRows()),
    });
    renderConfigs();
    await screen.findByText("差旅报销");

    // 差旅报销当前启用 → 停用
    const stopButtons = screen.getAllByRole("button", { name: "停用" });
    await userEvent.click(stopButtons[0]!);

    await waitFor(() => {
      const bodies = bodiesOf(calls, CONFIG_UPDATE_PATH);
      expect(bodies.length).toBe(1);
      expect(bodies[0]).toEqual({ config_id: 1, enabled: false });
    });
    // 成功后回读（第二次列表请求）
    await waitFor(() => {
      expect(countCalls(calls, CONFIG_LIST_PATH)).toBeGreaterThan(1);
    });
  });

  it("删除：ConfirmDialog 确认后调用 delete_config，成功后 toast 并回读", async () => {
    const calls = stubApprovalApi({
      configList: () => listPage(configRows()),
    });
    renderConfigs();
    await screen.findByText("费用报销");

    await userEvent.click(screen.getAllByRole("button", { name: "删除" })[1]!);
    // 确认对话框指认对象
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText(/费用报销（#2）/)).toBeInTheDocument();
    // 行内还有「删除」按钮，确认动作必须限定在对话框里
    await userEvent.click(within(dialog).getByRole("button", { name: "删除" }));

    await waitFor(() => {
      expect(bodiesOf(calls, CONFIG_DELETE_PATH)).toEqual([{ config_id: 2 }]);
    });
    // 成功 toast（含被清掉的 pending 任务数）
    expect(await screen.findByText(/已删除「费用报销」/)).toBeInTheDocument();
    await waitFor(() => {
      expect(countCalls(calls, CONFIG_LIST_PATH)).toBeGreaterThan(1);
    });
  });

  it("删除失败：toast 报后端原文，对话框不关", async () => {
    stubApprovalApi({
      configList: () => listPage(configRows()),
      deleteConfig: () =>
        new Response(
          JSON.stringify({
            code: 404,
            message: "审批派发配置不存在",
            data: null,
          }),
          { status: 404, headers: { "content-type": "application/json" } },
        ),
    });
    renderConfigs();
    await screen.findByText("费用报销");

    await userEvent.click(screen.getAllByRole("button", { name: "删除" })[1]!);
    const dialog = await screen.findByRole("dialog");
    await screen.findByText(/费用报销（#2）/);
    await userEvent.click(within(dialog).getByRole("button", { name: "删除" }));

    expect(await screen.findByText("审批派发配置不存在")).toBeInTheDocument();
  });

  it("映射明细：展开显示 widget_id/类型/列名/必填/转换器", async () => {
    stubApprovalApi({
      configList: () => listPage(configRows()),
    });
    renderConfigs();
    await screen.findByText("费用报销");

    await userEvent.click(
      screen.getAllByRole("button", { name: "映射明细" })[1]!,
    );
    const detail = await screen.findByTestId("maps-of-2");
    expect(within(detail).getByText("widget-1")).toBeInTheDocument();
    expect(within(detail).getByText("input")).toBeInTheDocument();
    expect(within(detail).getByText("fldA")).toBeInTheDocument();
    // 明细表头「必填」+ 该映射行值各一处
    expect(within(detail).getAllByText("必填").length).toBe(2);
    expect(within(detail).getByText("direct")).toBeInTheDocument();
  });
});

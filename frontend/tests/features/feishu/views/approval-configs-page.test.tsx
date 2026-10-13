/**
 * 审批派发配置页（`/feishu/approval/configs`，走真实路由含 lazy 的 `.default` 约定）。
 *
 * 钉住四件事：
 * - 列表业务名称与详情排查信息；
 * - 权限门控：无 list_configs 时侧边栏入口不渲染、页面给权限说明；
 * - 无写权限：详情写操作不渲染，「新建配置」不出现；
 * - 详情操作：启停确认后翻转 enabled、删除确认（成功 toast + 回读）、独立详情字段对应。
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
          widget_name: "付款明细_金额",
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
    await screen.findByRole("heading", { name: "审批配置" });

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
      "applicant",
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
      base_token:
        "https://tenant.example/base/<Base Token>?table=<数据表 ID>&view=<视图 ID>",
      record_id: "<触发记录 ID>",
      applicant: "<发起人姓名>",
      requested_by: "<触发人标识>",
    });
    expect(batch).toEqual({
      base_token: "https://tenant.example/base/<Base Token>?table=<数据表 ID>",
      applicant: single.applicant,
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

  it("列表展示配置名称和状态，隐藏技术标识", async () => {
    stubApprovalApi({
      configList: () => listPage(configRows()),
    });
    renderConfigs();

    expect(await screen.findByText("差旅报销")).toBeInTheDocument();
    expect(await screen.findByText("费用报销")).toBeInTheDocument();
    expect(screen.queryByText("CODE-TRAVEL")).toBeNull();
    expect(screen.queryByText("Asia/Shanghai")).toBeNull();
    const table = within(screen.getByRole("table"));
    expect(table.getByText("已启用")).toBeInTheDocument();
    expect(table.getByText("已停用")).toBeInTheDocument();
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
      await screen.findByText(/当前功能域没有审批控制台的读取权限/),
    ).toBeInTheDocument();
    // 无 Catalog Module 就不会生成导航入口。
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
    const user = userEvent.setup();
    await user.click(screen.getAllByRole("button", { name: "查看配置" })[1]!);
    const detail = within(
      screen.getByRole("tabpanel", { name: "字段对应（1）" }),
    );
    expect(detail.getByText("付款明细_金额")).toBeInTheDocument();
    expect(detail.getByText("金额")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "编辑配置" })).toBeNull();
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
    await userEvent.click(
      screen.getAllByRole("button", { name: "查看配置" })[1]!,
    );
    expect(
      await screen.findByRole("button", { name: "编辑配置" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "启用" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "删除" })).toBeInTheDocument();
  });
});

describe("审批派发配置页 · 行操作", () => {
  it("编辑保存逐项映射：预览保留原选择，请求使用控件和字段 ID", async () => {
    const user = userEvent.setup();
    const calls = stubApprovalApi({
      configList: () => listPage(configRows()),
      widgets: () => ({
        widgets: [
          {
            id: "widget-1",
            name: "金额",
            qualified_name: "付款明细_金额",
            type: "input",
            required: true,
          },
        ],
      }),
      bitableFields: () => ({
        fields: [
          { field_id: "fldA", field_name: "金额", type: 1 },
          { field_id: "fldB", field_name: "实际付款", type: 1 },
        ],
      }),
    });
    renderConfigs();
    await screen.findByText("费用报销");
    await user.click(screen.getAllByRole("button", { name: "查看配置" })[1]!);
    await user.click(await screen.findByRole("button", { name: "编辑配置" }));
    await user.click(screen.getByRole("button", { name: "预览控件" }));
    const pick = await screen.findByRole("combobox", {
      name: "付款明细_金额对应的多维表格字段",
    });
    expect(pick).toHaveTextContent("金额");
    await user.click(pick);
    await user.click(await screen.findByRole("option", { name: /实际付款/ }));
    await user.click(screen.getByRole("button", { name: "下一步" }));
    await user.click(screen.getByRole("button", { name: "下一步" }));
    expect(screen.getByText("付款明细_金额 → 实际付款")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() =>
      expect(bodiesOf(calls, CONFIG_UPDATE_PATH)).toEqual([
        {
          config_id: 2,
          base_timezone: "Asia/Shanghai",
          maps: [{ widget_id: "widget-1", bitable_field: "fldB" }],
        },
      ]),
    );
    await waitFor(() =>
      expect(countCalls(calls, CONFIG_LIST_PATH)).toBeGreaterThan(1),
    );
  });
  it("启停：调用 update_config 翻转 enabled，成功后回读列表", async () => {
    const calls = stubApprovalApi({
      configList: () => listPage(configRows()),
    });
    renderConfigs();
    await screen.findByText("差旅报销");

    // 差旅报销当前启用 → 停用
    await userEvent.click(
      screen.getAllByRole("button", { name: "查看配置" })[0]!,
    );
    await userEvent.click(await screen.findByRole("button", { name: "停用" }));
    await userEvent.click(
      within(screen.getByRole("dialog")).getByRole("button", {
        name: "确认停用",
      }),
    );

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

    await userEvent.click(
      screen.getAllByRole("button", { name: "查看配置" })[1]!,
    );
    await userEvent.click(await screen.findByRole("button", { name: "删除" }));
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

    await userEvent.click(
      screen.getAllByRole("button", { name: "查看配置" })[1]!,
    );
    await userEvent.click(await screen.findByRole("button", { name: "删除" }));
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
      screen.getAllByRole("button", { name: "查看配置" })[1]!,
    );
    await userEvent.click(await screen.findByRole("tab", { name: "排查信息" }));
    await userEvent.click(screen.getByText("原始字段对应"));
    expect(screen.getByText("widget-1")).toBeInTheDocument();
    expect(screen.getByText("input")).toBeInTheDocument();
    expect(screen.getByText("fldA")).toBeInTheDocument();
    expect(screen.getByText("direct")).toBeInTheDocument();
  });
});

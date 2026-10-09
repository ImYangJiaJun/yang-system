/**
 * 派发记录页（`/feishu/approval/requests`，走真实路由）。
 *
 * 钉住四件事：
 * - 列表投影（时间/请求人/坐标/record_id/outcome 色标/message）；
 * - 行展开看请求体与返回体 JSON 原文（含 serial_number）；
 * - 批量行（accepted）「查看任务」下钻 list_tasks（按 config_id 过滤）；
 * - 筛选：base_token / table_id / outcome 折成 where 树 + 分页；
 * - 权限门控：无读权限时入口不渲染、不发请求。
 */

import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { renderTestApp } from "@test/helpers/render-app";

import {
  bodiesOf,
  countCalls,
  listPage,
  requestWire,
  stubApprovalApi,
  taskWire,
} from "./approval-harness";

const REQUEST_LIST_PATH = "/api/v1/feishu/approval/requests/query";
const TASK_LIST_PATH = "/api/v1/feishu/approval/tasks/query";

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

function renderRequests() {
  return renderTestApp({
    path: "/feishu/approval/requests",
    authenticated: true,
  });
}

function fourOutcomeRows() {
  return [
    requestWire(),
    requestWire({
      id: 2,
      requested_by: null,
      record_id: null,
      outcome: "waiting",
      message: "必填字段缺失，本轮未处理",
      serial_number: null,
      response_body: null,
    }),
    requestWire({
      id: 3,
      outcome: "accepted",
      record_id: null,
      message: "已受理 3 条",
      serial_number: null,
    }),
    requestWire({
      id: 4,
      outcome: "failed",
      message: "飞书出站凭证未配置",
      serial_number: null,
      response_body: null,
    }),
  ];
}

describe("派发记录页 · 列表", () => {
  it("渲染列表投影：时间/请求人/坐标/record_id/outcome 色标/message", async () => {
    stubApprovalApi({
      requestList: () => listPage(fourOutcomeRows()),
    });
    renderRequests();

    // 默认 requested_by 是张三：四条里三条有值，一条是「—」
    expect((await screen.findAllByText("张三")).length).toBe(3);
    expect(screen.getByText("成功")).toBeInTheDocument();
    expect(screen.getByText("等待")).toBeInTheDocument();
    expect(screen.getByText("批量受理")).toBeInTheDocument();
    expect(screen.getByText("失败")).toBeInTheDocument();
    // 色标 tone 与结果对应
    expect(screen.getByText("成功").closest("[data-tone]")).toHaveAttribute(
      "data-tone",
      "positive",
    );
    expect(screen.getByText("等待").closest("[data-tone]")).toHaveAttribute(
      "data-tone",
      "warning",
    );
    expect(screen.getByText("批量受理").closest("[data-tone]")).toHaveAttribute(
      "data-tone",
      "info",
    );
    expect(screen.getByText("失败").closest("[data-tone]")).toHaveAttribute(
      "data-tone",
      "danger",
    );
    // 坐标与 record_id（默认值 recABC 出现两次：等待/批量受理两条是空态）
    expect(screen.getAllByText(/appbcbWCzen6 \/ tblsRc9GRRX/).length).toBe(4);
    expect(screen.getAllByText("recABC").length).toBe(2);
    // message（截断列）
    expect(screen.getByText("飞书出站凭证未配置")).toBeInTheDocument();
  });

  it("筛选：base_token/table_id/outcome 折成 where 树，变化回第 1 页", async () => {
    const calls = stubApprovalApi({
      requestList: () => listPage(fourOutcomeRows()),
    });
    renderRequests();
    await screen.findAllByText("张三");

    await userEvent.type(screen.getByLabelText("按 Base Token 筛选"), "appX");
    await userEvent.type(screen.getByLabelText("按 table_id 筛选"), "tblY");
    await userEvent.click(screen.getByRole("combobox", { name: "结果筛选" }));
    await userEvent.click(await screen.findByRole("option", { name: "失败" }));

    await waitFor(() => {
      const bodies = bodiesOf(calls, REQUEST_LIST_PATH);
      const last = bodies[bodies.length - 1];
      expect(last?.where).toEqual({
        type: "and",
        conditions: [
          { type: "eq", field: "base_token", value: "appX" },
          { type: "eq", field: "table_id", value: "tblY" },
          { type: "eq", field: "outcome", value: "failed" },
        ],
      });
      expect(last?.page).toBe(1);
    });
  });

  it("无筛选时请求体不带 where，且恒带确定性排序", async () => {
    const calls = stubApprovalApi({
      requestList: () => listPage(fourOutcomeRows()),
    });
    renderRequests();
    await screen.findAllByText("张三");

    for (const body of bodiesOf(calls, REQUEST_LIST_PATH)) {
      expect(body?.where).toBeUndefined();
      expect(body?.order_by).toEqual([
        { field: "created_at", direction: "Desc" },
        { field: "id", direction: "Asc" },
      ]);
      expect(body?.count_total).toBe(true);
    }
  });
});

describe("派发记录页 · 行展开", () => {
  it("展开看请求体与返回体 JSON 原文与单号", async () => {
    stubApprovalApi({
      requestList: () => listPage(fourOutcomeRows()),
    });
    renderRequests();
    await screen.findAllByText("张三");

    await userEvent.click(
      screen.getAllByRole("button", { name: "展开详情" })[0]!,
    );
    const requestBody = await screen.findByText(/requested_by/);
    expect(requestBody).toBeInTheDocument();
    expect(screen.getByText(/instance_code/)).toBeInTheDocument();
    expect(screen.getByText("SN-2026-0001")).toBeInTheDocument();
  });

  it("批量行展开自动下钻 list_tasks（按 config_id 过滤），展示任务字段", async () => {
    const calls = stubApprovalApi({
      requestList: () => listPage(fourOutcomeRows()),
      taskList: () =>
        listPage([
          taskWire(),
          taskWire({
            id: 2,
            record_id: "recDEF",
            state: "terminal",
            last_error: "表不存在",
            instance_code: null,
            serial_number: null,
          }),
        ]),
    });
    renderRequests();
    await screen.findAllByText("张三");

    await userEvent.click(
      screen.getAllByRole("button", { name: "展开详情" })[2]!,
    );
    expect(await screen.findByText("任务（该配置）")).toBeInTheDocument();
    await waitFor(() => {
      const bodies = bodiesOf(calls, TASK_LIST_PATH);
      expect(bodies.length).toBe(1);
      expect(bodies[0]?.where).toEqual({
        type: "eq",
        field: "config_id",
        value: 1,
      });
    });
    // 任务行字段。`inst-1` 会同时出现在展开的返回体 JSON 原文里，用 getAll。
    expect(await screen.findByText("recDEF")).toBeInTheDocument();
    expect(screen.getByText("terminal")).toBeInTheDocument();
    expect(screen.getByText("表不存在")).toBeInTheDocument();
    expect(screen.getAllByText("inst-1").length).toBeGreaterThan(0);
  });

  it("非批量行展开不触发任务查询", async () => {
    const calls = stubApprovalApi({
      requestList: () => listPage(fourOutcomeRows()),
    });
    renderRequests();
    await screen.findAllByText("张三");

    // 第一行是 succeeded（不是 accepted）
    await userEvent.click(
      screen.getAllByRole("button", { name: "展开详情" })[0]!,
    );
    await screen.findByText(/requested_by/);
    expect(countCalls(calls, TASK_LIST_PATH)).toBe(0);
    expect(screen.queryByText("任务（该配置）")).toBeNull();
  });
});

describe("派发记录页 · 权限门控", () => {
  it("无读权限：侧边栏「派发记录」入口不渲染，页面给权限说明，不发请求", async () => {
    const calls = stubApprovalApi({ approvalRead: false });
    renderRequests();

    expect(
      await screen.findByText(/当前功能域没有审批控制台的读取权限/),
    ).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "派发记录" })).toBeNull();
    expect(countCalls(calls, REQUEST_LIST_PATH)).toBe(0);
  });
});

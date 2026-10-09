import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { renderTestApp } from "@test/helpers/render-app";
import {
  listPage,
  requestWire,
  stubApprovalApi,
  taskWire,
} from "./approval-harness";

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
});

describe("审批控制台", () => {
  it("仅记录 operation 可见时仍能展开批量任务", async () => {
    stubApprovalApi({
      onlyRead: "requests",
      requestList: () => listPage([requestWire({ outcome: "accepted" })]),
      taskList: () => listPage([taskWire({ record_id: "rec-task-only" })]),
    });
    renderTestApp({ path: "/feishu/approval?tab=requests" });
    await userEvent.click(
      await screen.findByRole("button", { name: "展开详情" }),
    );
    expect(await screen.findByText("rec-task-only")).toBeInTheDocument();
  });
  it.each([
    ["configs", "配置"],
    ["requests", "派发记录"],
  ] as const)("单 %s operation 仍能读取首个可用 tab", async (tab, name) => {
    const calls = stubApprovalApi({ onlyRead: tab });
    renderTestApp({ path: "/feishu/approval?tab=invalid" });
    expect(await screen.findByRole("tab", { name })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.getAllByRole("tab")).toHaveLength(1);
    await waitFor(() =>
      expect(calls.some((c) => c.url.endsWith(`/${tab}/query`))).toBe(true),
    );
  });
  it("默认配置、键盘切换记录并保留其他查询参数", async () => {
    stubApprovalApi();
    const { router } = renderTestApp({ path: "/feishu/approval?filter=keep" });
    const configs = await screen.findByRole("tab", { name: "配置" });
    expect(configs).toHaveAttribute("aria-selected", "true");
    const user = userEvent.setup();
    configs.focus();
    await user.keyboard("{ArrowRight}");
    const requests = screen.getByRole("tab", { name: "派发记录" });
    expect(requests).toHaveFocus();
    expect(requests).toHaveAttribute("aria-selected", "true");
    expect(configs).toHaveAttribute("tabindex", "-1");
    expect(router.state.location.search).toContain("filter=keep");
    expect(router.state.location.search).toContain("tab=requests");
    expect(screen.getByRole("tabpanel")).toHaveAttribute(
      "id",
      requests.getAttribute("aria-controls"),
    );
    await user.keyboard("{Home}");
    expect(configs).toHaveFocus();
    await user.keyboard("{End}");
    expect(requests).toHaveFocus();
    await user.keyboard("{ArrowLeft}");
    expect(configs).toHaveFocus();
    await user.keyboard("{ArrowLeft}");
    expect(requests).toHaveFocus();
  });
  it.each([
    ["requests", "派发记录"],
    ["invalid", "配置"],
  ])("query tab=%s 选择可用页面", async (tab, name) => {
    stubApprovalApi();
    renderTestApp({ path: `/feishu/approval?tab=${tab}` });
    expect(await screen.findByRole("tab", { name })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });
  it("无读权限时没有 tab 或列表请求", async () => {
    const calls = stubApprovalApi({ approvalRead: false });
    renderTestApp({ path: "/feishu/approval" });
    expect(
      await screen.findByText(
        "当前功能域没有审批控制台的读取权限。请联系管理员。",
      ),
    ).toHaveAttribute("aria-live", "polite");
    expect(screen.queryByRole("tab")).toBeNull();
    expect(
      calls.some(
        (c) =>
          c.url.endsWith("/configs/query") || c.url.endsWith("/requests/query"),
      ),
    ).toBe(false);
  });
  it.each([
    ["configs", "配置"],
    ["requests", "派发记录"],
  ])("旧 %s URL 实际重定向", async (tab, name) => {
    stubApprovalApi();
    const { router } = renderTestApp({ path: `/feishu/approval/${tab}` });
    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/feishu/approval"),
    );
    expect(router.state.location.search).toBe(`?tab=${tab}`);
    expect(await screen.findByRole("tab", { name })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(router.state.historyAction).toBe("REPLACE");
  });
});

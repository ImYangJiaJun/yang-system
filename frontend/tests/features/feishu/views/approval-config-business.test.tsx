import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { clearStoredSession } from "@/engine/session/auth-session";
import { renderTestApp } from "@test/helpers/render-app";
import {
  approvalConfigWire,
  bodiesOf,
  listPage,
  jsonResponse,
  stubApprovalApi,
} from "./approval-harness";

const queryPath = "/api/v1/feishu/approval/configs/query";
const businessRow = () =>
  approvalConfigWire({
    base_name: "财务协作",
    table_name: "差旅申请表",
    approval_name: "员工报销审批",
    applicant_field_name: "报销人",
    backfill_field_name: "审批单编号",
    maps: [
      {
        widget_id: "w1",
        widget_name: "出发日期",
        widget_type: "date",
        bitable_field: "fldDate",
        bitable_field_name: "出差日期",
        required: true,
        converter: "date",
      },
    ],
  });
const render = (suffix = "") =>
  renderTestApp({
    path: `/feishu/approval${suffix}`,
    authenticated: true,
  });
afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

describe("审批配置业务阅读", () => {
  it("缺失或异常坐标不能伪造可打开的外链", async () => {
    stubApprovalApi({
      configList: () =>
        listPage([
          approvalConfigWire({
            base_token: "",
            table_id: "bad/table",
            approval_code: "bad?code",
          }),
        ]),
    });
    render("?config=1");
    await screen.findByRole("heading", { name: "差旅报销" });
    expect(screen.queryByRole("link", { name: "打开多维表" })).toBeNull();
    expect(screen.queryByRole("link", { name: "打开审批定义" })).toBeNull();
    expect(screen.getByText("打开多维表（标识待核对）")).toBeInTheDocument();
  });
  it("有审批定义 id 时链接精确指向审批后台定义页", async () => {
    stubApprovalApi({
      configList: () =>
        listPage([approvalConfigWire({ approval_id: "7699783289053989908" })]),
    });
    render("?config=1");
    expect(
      await screen.findByRole("link", { name: "打开审批定义" }),
    ).toHaveAttribute(
      "href",
      "https://www.feishu.cn/approval/admin/createApproval?id=7699783289053989908&definitionCode=CODE-TRAVEL",
    );
  });
  it("缺审批定义 id 时提示补全链接，不给错误跳转", async () => {
    stubApprovalApi({ configList: () => listPage([approvalConfigWire()]) });
    render("?config=1");
    expect(
      await screen.findByText("打开审批定义（请编辑配置补全审批定义链接）"),
    ).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "打开审批定义" })).toBeNull();
  });
  it("列表优先展示业务名称，技术标识不占主阅读区", async () => {
    stubApprovalApi({ configList: () => listPage([businessRow()]) });
    render();
    expect(
      await screen.findByRole("heading", { name: "审批配置" }),
    ).toBeInTheDocument();
    for (const name of ["财务协作", "差旅申请表", "员工报销审批"])
      expect(await screen.findByText(name)).toBeInTheDocument();
    expect(
      within(screen.getByRole("table")).getByText("已启用"),
    ).toBeInTheDocument();
    expect(screen.queryByText("CODE-TRAVEL")).toBeNull();
    expect(screen.queryByText("Asia/Shanghai")).toBeNull();
    expect(
      screen.getByRole("button", { name: "查看配置" }),
    ).toBeInTheDocument();
  });
  it("缺失名称明确待补充，不能把标题或标识当名称", async () => {
    stubApprovalApi({ configList: () => listPage([approvalConfigWire()]) });
    render();
    for (const text of ["表格名称待补充", "数据表名称待补充", "审批名称待补充"])
      expect(await screen.findByText(text)).toBeInTheDocument();
  });
  it("状态和搜索请求作用于服务端并回到首页", async () => {
    const calls = stubApprovalApi({
      configList: () => listPage([businessRow()], { total: 25 }),
    });
    render("?page=2");
    await screen.findByText("差旅申请表");
    await userEvent.click(screen.getByRole("button", { name: "已停用" }));
    await userEvent.type(
      screen.getByRole("textbox", { name: "搜索配置" }),
      "员工",
    );
    await waitFor(() =>
      expect(bodiesOf(calls, queryPath).at(-1)).toMatchObject({
        page: 1,
        search: "员工",
        where: { type: "eq", field: "enabled", value: false },
      }),
    );
  });
  it("独立按编号读取详情，默认中文对应关系，技术信息集中排查", async () => {
    const calls = stubApprovalApi({
      approvalWrite: false,
      configList: () => listPage([businessRow()]),
    });
    render("?config=1");
    expect(
      await screen.findByRole("heading", { name: "差旅报销" }),
    ).toBeInTheDocument();
    expect(bodiesOf(calls, queryPath)[0]).toMatchObject({
      where: { type: "eq", field: "id", value: 1 },
    });
    expect(screen.getByText("出差日期")).toBeInTheDocument();
    expect(screen.getByText("按表格时区转换日期")).toBeInTheDocument();
    expect(screen.queryByText("w1")).toBeNull();
    expect(screen.queryByRole("button", { name: "编辑配置" })).toBeNull();
    await userEvent.click(screen.getByRole("tab", { name: "排查信息" }));
    expect(screen.getByText("CODE-TRAVEL")).toBeInTheDocument();
    expect(screen.getByText("表单结构缓存时间")).toBeInTheDocument();
    await userEvent.click(screen.getByText("原始字段对应"));
    expect(screen.getByText("w1")).toBeInTheDocument();
  });
  it("关联配置单独按审批查询，有真实总数和分页", async () => {
    const calls = stubApprovalApi({
      configList: (body) =>
        body.where &&
        (body.where as { field: string }).field === "approval_code"
          ? listPage([businessRow()], { total: 23 })
          : listPage([businessRow()]),
    });
    render("?config=1");
    await screen.findByRole("heading", { name: "差旅报销" });
    await userEvent.click(screen.getByRole("button", { name: "查看关联配置" }));
    const dialog = await screen.findByRole("dialog");
    expect(await within(dialog).findByText(/23/)).toBeInTheDocument();
    await userEvent.click(
      within(dialog).getByRole("button", { name: "下一页" }),
    );
    await waitFor(() =>
      expect(bodiesOf(calls, queryPath).at(-1)).toMatchObject({
        page: 2,
        where: { type: "eq", field: "approval_code", value: "CODE-TRAVEL" },
      }),
    );
  });
  it("启停需要确认，取消不提交", async () => {
    const calls = stubApprovalApi({
      configList: () => listPage([businessRow()]),
    });
    render("?config=1");
    await screen.findByRole("heading", { name: "差旅报销" });
    await userEvent.click(screen.getByRole("button", { name: "停用" }));
    const dialog = screen.getByRole("dialog");
    expect(
      within(dialog).getByText(/新的派发请求将不再通过此配置受理/),
    ).toBeInTheDocument();
    expect(
      bodiesOf(calls, "/api/v1/feishu/approval/configs/update"),
    ).toHaveLength(0);
    await userEvent.click(within(dialog).getByRole("button", { name: "取消" }));
    expect(
      bodiesOf(calls, "/api/v1/feishu/approval/configs/update"),
    ).toHaveLength(0);
    await userEvent.click(screen.getByRole("button", { name: "停用" }));
    await userEvent.click(
      within(screen.getByRole("dialog")).getByRole("button", {
        name: "确认停用",
      }),
    );
    await waitFor(() =>
      expect(bodiesOf(calls, "/api/v1/feishu/approval/configs/update")).toEqual(
        [{ config_id: 1, enabled: false }],
      ),
    );
  });
  it("详情不存在明确提示并可返回", async () => {
    stubApprovalApi();
    render("?config=99");
    expect(await screen.findByText("无法找到这条配置")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "返回配置列表" }),
    ).toBeInTheDocument();
  });
  it("返回列表恢复原分页和筛选", async () => {
    const calls = stubApprovalApi({
      configList: () => listPage([businessRow()], { total: 25 }),
    });
    render("?page=2&search=差旅&status=enabled");
    await screen.findByText("差旅申请表");
    await userEvent.click(screen.getByRole("button", { name: "查看配置" }));
    await screen.findByRole("heading", { name: "差旅报销" });
    await userEvent.click(screen.getByRole("button", { name: "返回配置列表" }));
    expect(
      await screen.findByRole("textbox", { name: "搜索配置" }),
    ).toHaveValue("差旅");
    await waitFor(() =>
      expect(
        bodiesOf(calls, queryPath)
          .filter(
            (body) =>
              !body?.where || (body.where as { field: string }).field !== "id",
          )
          .at(-1),
      ).toMatchObject({
        page: 2,
        search: "差旅",
        where: { type: "eq", field: "enabled", value: true },
      }),
    );
  });
  it("详情读取失败可重试，不能误判为已删除", async () => {
    let failed = true;
    stubApprovalApi({
      configList: () =>
        failed
          ? jsonResponse({ code: 503, message: "暂时无法连接" }, 503)
          : listPage([businessRow()]),
    });
    render("?config=1");
    expect(await screen.findByText(/暂时无法读取配置/)).toBeInTheDocument();
    expect(screen.queryByText("无法找到这条配置")).toBeNull();
    failed = false;
    await userEvent.click(screen.getByRole("button", { name: "重新加载" }));
    expect(
      await screen.findByRole("heading", { name: "差旅报销" }),
    ).toBeInTheDocument();
  });
  it("旧映射缺规则或未知类型时待确认，字段搜索有明确空态", async () => {
    stubApprovalApi({
      configList: () =>
        listPage([
          approvalConfigWire({
            maps: [
              {
                widget_id: "unknown",
                widget_type: "future",
                bitable_field: "fldX",
                bitable_field_name: "项目编号",
              },
            ],
          }),
        ]),
    });
    render("?config=1");
    expect(await screen.findByText("填写规则待确认")).toBeInTheDocument();
    expect(screen.getByText("类型待确认")).toBeInTheDocument();
    await userEvent.type(
      screen.getByRole("textbox", { name: "搜索字段对应" }),
      "不存在",
    );
    expect(screen.getByText(/没有符合条件的字段对应/)).toBeInTheDocument();
  });
  it("零条对应不宣称配置完成", async () => {
    stubApprovalApi({ configList: () => listPage([approvalConfigWire()]) });
    render("?config=1");
    expect(await screen.findByText(/暂无字段对应/)).toBeInTheDocument();
  });
  it("名称补充按十条分批，部分失败保留原有显示", async () => {
    const calls = stubApprovalApi({
      configList: () =>
        listPage(
          Array.from({ length: 20 }, (_, i) => ({
            ...businessRow(),
            id: i + 1,
          })),
          { total: 20 },
        ),
      refreshNames: (body) => ({ incomplete_ids: body.config_ids }),
    });
    render("?pageSize=20");
    await screen.findByRole("button", { name: "补充本页名称" });
    await userEvent.click(screen.getByRole("button", { name: "补充本页名称" }));
    await waitFor(() =>
      expect(
        bodiesOf(calls, "/api/v1/feishu/approval/configs/refresh-names"),
      ).toEqual([
        { config_ids: Array.from({ length: 10 }, (_, i) => i + 1) },
        { config_ids: Array.from({ length: 10 }, (_, i) => i + 11) },
      ]),
    );
    expect(
      await screen.findByText(/20 条配置的名称未补充完整/),
    ).toBeInTheDocument();
    expect(screen.getAllByText("差旅申请表")).toHaveLength(20);
  });
  it("标签页支持键盘，结构缓存不表示检查通过", async () => {
    stubApprovalApi({ configList: () => listPage([businessRow()]) });
    render("?config=1");
    const maps = await screen.findByRole("tab", { name: /字段对应/ });
    maps.focus();
    await userEvent.keyboard("{End}");
    expect(screen.getByRole("tab", { name: "排查信息" })).toHaveFocus();
    expect(
      screen.getByText(/不代表配置检查通过，也不代表审批已经运行成功/),
    ).toBeInTheDocument();
    await userEvent.keyboard("{Home}");
    expect(maps).toHaveFocus();
  });
});

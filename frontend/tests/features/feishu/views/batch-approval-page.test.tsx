import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { renderTestApp } from "@test/helpers/render-app";

// Ant Design 的表格在 jsdom 中首屏渲染较慢（rc-table + CSS-in-JS），单测给足时间。
vi.setConfig({ testTimeout: 15_000 });

/// 批量审批页：分组、可操作性、动作确认、job 轮询、详情与重新授权引导。

async function filterColumn(
  user: ReturnType<typeof userEvent.setup>,
  groupName: string,
  columnName: string,
  optionLabel: string,
) {
  const heading = screen.getByRole("heading", { name: groupName });
  const section = heading.parentElement?.parentElement;
  const header = Array.from(section?.querySelectorAll("th") ?? []).find(
    (cell) => cell.textContent?.includes(columnName),
  );
  const trigger = header?.querySelector(".ant-table-filter-trigger");
  expect(trigger).not.toBeNull();
  await user.click(trigger as HTMLElement);

  const dropdown = await waitFor(() => {
    const elements = document.querySelectorAll(
      ".ant-table-filter-dropdown:not(.ant-dropdown-hidden)",
    );
    const element = elements[elements.length - 1];
    if (!element) throw new Error("筛选菜单未打开");
    return element as HTMLElement;
  });
  const option = within(dropdown).getByText(optionLabel);
  const item = option.closest("li");
  await user.click(item ?? option);
  const confirms = document.querySelectorAll(
    ".ant-table-filter-dropdown:not(.ant-dropdown-hidden)",
  );
  const active = confirms[confirms.length - 1] ?? dropdown;
  await user.click(
    within(active as HTMLElement).getByRole("button", { name: /OK|确定/ }),
  );
}

function jsonResponse(payload: unknown, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

const TASKS = [
  {
    task_id: "t1",
    instance_code: "i1",
    definition_code: "D1",
    definition_name: "报销审批",
    title: "报销单 A",
    initiator_name: "李四",
    summaries: ["差旅费 ¥100"],
    support_api_operate: true,
    status: "PENDING",
    create_time: 1_700_000_000,
    form_fields: [
      { name: "金额", value: { text: "100" } },
      { name: "部门", value: "研发" },
    ],
    current_node_names: ["部门审批", "财务审批"],
    my_node_name: "部门审批",
  },
  {
    task_id: "t2",
    instance_code: "i2",
    definition_code: "D2",
    definition_name: "请假审批",
    title: "请假单 B",
    initiator_name: "王五",
    summaries: ["2 天"],
    support_api_operate: false,
    status: "PENDING",
    form_fields: [{ name: "天数", value: "2" }],
    current_node_names: ["主管审批"],
    my_node_name: "主管审批",
  },
  {
    task_id: "t3",
    instance_code: "i3",
    definition_code: "D2",
    definition_name: "请假审批",
    title: "外部单 C",
    initiator_name: "赵六",
    summaries: ["外部流程"],
    instance_external_id: "ext-1",
    status: "PENDING",
  },
];

type Call = { method: string; path: string; body: unknown };

type StubOptions = {
  tasksResponse?: Response;
  detail?: unknown;
  jobResponses?: Record<string, unknown>;
  onCreateJob?: (body: unknown) => { job_id: string };
  onSearch?: (query: string) => unknown;
};

function stubBatchApi(options: StubOptions = {}): Call[] {
  const calls: Call[] = [];
  let created = 0;
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const raw = typeof input === "string" ? input : input.toString();
      const url = new URL(raw, "http://localhost");
      const method = init?.method ?? "GET";
      const body = init?.body ? JSON.parse(String(init.body)) : undefined;
      calls.push({ method, path: url.pathname, body });

      if (url.pathname === "/api/v1/users/me") {
        return jsonResponse({
          code: 0,
          data: { username: "alice" },
        });
      }
      if (url.pathname === "/api/v1/feishu/batch-approval/tasks") {
        return (
          options.tasksResponse ??
          jsonResponse({
            code: 0,
            data: { tasks: TASKS, truncated: false },
          })
        );
      }
      if (url.pathname.startsWith("/api/v1/feishu/batch-approval/tasks/")) {
        return jsonResponse({
          code: 0,
          data: options.detail ?? {},
        });
      }
      if (url.pathname === "/api/v1/feishu/batch-approval/users/search") {
        return jsonResponse({
          code: 0,
          data: {
            users: options.onSearch
              ? options.onSearch(url.searchParams.get("q") ?? "")
              : [],
          },
        });
      }
      if (
        url.pathname === "/api/v1/feishu/batch-approval/jobs" &&
        method === "POST"
      ) {
        created += 1;
        const result = options.onCreateJob
          ? options.onCreateJob(body)
          : { job_id: `job-${created}` };
        return jsonResponse({ code: 0, data: result });
      }
      const jobMatch = url.pathname.match(
        /^\/api\/v1\/feishu\/batch-approval\/jobs\/([^/]+)$/,
      );
      if (jobMatch && method === "GET") {
        const payload = options.jobResponses?.[jobMatch[1]];
        if (!payload) {
          throw new Error(`测试未覆盖的 job：${jobMatch[1]}`);
        }
        return jsonResponse({ code: 0, data: payload });
      }
      throw new Error(`测试未覆盖的请求：${method} ${url.pathname}`);
    }),
  );
  return calls;
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

describe("BatchApprovalPage", () => {
  it("按定义分组展示，不可操作任务禁用并说明原因", async () => {
    stubBatchApi();
    renderTestApp({ path: "/batch-approval", authenticated: true });

    expect(
      await screen.findByRole(
        "heading",
        { name: "批量审批" },
        { timeout: 15_000 },
      ),
    ).toBeInTheDocument();
    expect(await screen.findByText("报销单 A")).toBeInTheDocument();
    // 左侧分组带数量。
    expect(screen.getByRole("button", { name: /报销审批/ })).toHaveTextContent(
      "1",
    );
    expect(screen.getByRole("button", { name: /请假审批/ })).toHaveTextContent(
      "2",
    );

    // support_api_operate=false 与外部审批都不可勾选，并给出原因。
    expect(screen.getByLabelText("选择 请假单 B")).toBeDisabled();
    expect(screen.getByLabelText("选择 外部单 C")).toBeDisabled();
    expect(screen.getByText("该审批不支持 API 操作")).toBeInTheDocument();
    expect(screen.getByText("外部审批，需在飞书侧处理")).toBeInTheDocument();
    expect(screen.getByLabelText("选择 报销单 A")).not.toBeDisabled();
  });

  it("勾选任务 → 同意确认 → 创建 job 并轮询逐条结果", async () => {
    const calls = stubBatchApi({
      jobResponses: {
        "job-1": {
          job_id: "job-1",
          status: "succeeded",
          total: 1,
          succeeded: 1,
          failed: 0,
          unknown: 0,
          items: [
            {
              task_id: "t1",
              instance_code: "i1",
              title: "报销单 A",
              status: "succeeded",
            },
          ],
        },
      },
    });
    renderTestApp({ path: "/batch-approval", authenticated: true });

    const user = userEvent.setup();
    await screen.findByText("报销单 A");
    await user.click(screen.getByLabelText("选择 报销单 A"));
    await user.click(screen.getByRole("button", { name: "同意" }));

    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText(/即将对/)).toHaveTextContent("1");
    await user.click(within(dialog).getByRole("button", { name: "确认同意" }));

    const createCall = calls.find(
      (call) =>
        call.method === "POST" &&
        call.path === "/api/v1/feishu/batch-approval/jobs",
    );
    expect(createCall?.body).toMatchObject({
      action: "pass",
      items: [{ task_id: "t1", instance_code: "i1" }],
    });
    expect(
      (createCall?.body as { idempotency_key?: string } | undefined)
        ?.idempotency_key,
    ).toEqual(expect.any(String));

    expect(await screen.findByText("全部成功")).toBeInTheDocument();
    expect(screen.getByText("成功")).toBeInTheDocument();
  });

  it("拒绝必须填写理由；填写后 job 携带 comment", async () => {
    const calls = stubBatchApi({
      jobResponses: {
        "job-1": {
          job_id: "job-1",
          status: "succeeded",
          total: 1,
          succeeded: 1,
          failed: 0,
          unknown: 0,
          items: [],
        },
      },
    });
    renderTestApp({ path: "/batch-approval", authenticated: true });

    const user = userEvent.setup();
    await screen.findByText("报销单 A");
    await user.click(screen.getByLabelText("选择 报销单 A"));
    await user.click(screen.getByRole("button", { name: "拒绝" }));

    const dialog = await screen.findByRole("dialog");
    const confirm = within(dialog).getByRole("button", { name: "确认拒绝" });
    expect(confirm).toBeDisabled();

    await user.type(
      within(dialog).getByLabelText("审批意见（必填）"),
      "金额不符",
    );
    expect(confirm).not.toBeDisabled();
    await user.click(confirm);

    const createCall = calls.find(
      (call) =>
        call.method === "POST" &&
        call.path === "/api/v1/feishu/batch-approval/jobs",
    );
    expect(createCall?.body).toMatchObject({
      action: "refuse",
      comment: "金额不符",
      items: [{ task_id: "t1", instance_code: "i1" }],
    });
  });

  it("表格直接展示全部表单字段，并支持按字段与当前节点筛选", async () => {
    const extraTask = {
      ...TASKS[0],
      task_id: "t1b",
      instance_code: "i1b",
      title: "报销单 A2",
      form_fields: [
        { name: "金额", value: { text: "200" } },
        { name: "部门", value: "研发" },
      ],
      current_node_names: ["财务审批"],
      my_node_name: "财务审批",
    };
    stubBatchApi({
      tasksResponse: jsonResponse({
        code: 0,
        data: {
          tasks: [TASKS[0], extraTask, ...TASKS.slice(1)],
          truncated: false,
        },
      }),
    });
    renderTestApp({ path: "/batch-approval", authenticated: true });

    await screen.findByText("报销单 A");
    // 表头与值都在同一张表内，不需要点开详情。
    expect(screen.getAllByText("金额").length).toBeGreaterThan(0);
    expect(screen.getAllByText("部门").length).toBeGreaterThan(0);
    expect(screen.getByText("100")).toBeInTheDocument();
    expect(screen.getByText("部门审批（我的节点）")).toBeInTheDocument();
    expect(screen.getByText("财务审批")).toBeInTheDocument();

    const user = userEvent.setup();
    await filterColumn(user, "报销审批", "金额", "100");
    expect(screen.getByText("报销单 A")).toBeInTheDocument();
    expect(screen.queryByText("报销单 A2")).not.toBeInTheDocument();

    await filterColumn(user, "请假审批", "当前节点", "主管审批");
    expect(screen.getByText("报销单 A")).toBeInTheDocument();
    expect(screen.getByText("请假单 B")).toBeInTheDocument();
    expect(screen.queryByText("外部单 C")).not.toBeInTheDocument();
  });

  it("附件字段展示文件名，图片和 PDF 可预览，Excel 可下载", async () => {
    const attachmentTask = {
      ...TASKS[0],
      form_fields: [
        {
          name: "发票",
          value: ["https://files.example.com/发票.pdf"],
          field_type: "attachmentV2",
          ext: "'发票.pdf'",
        },
        {
          name: "凭证",
          value: ["https://files.example.com/凭证.png"],
          field_type: "imageV2",
          ext: "'凭证.png'",
        },
        {
          name: "明细",
          value: ["https://files.example.com/明细.xlsx"],
          field_type: "attachmentV2",
          ext: "'明细.xlsx'",
        },
      ],
    };
    stubBatchApi({
      tasksResponse: jsonResponse({
        code: 0,
        data: { tasks: [attachmentTask], truncated: false },
      }),
    });
    renderTestApp({ path: "/batch-approval", authenticated: true });

    expect(await screen.findByText("发票.pdf")).toBeInTheDocument();
    expect(screen.getByText("凭证.png")).toBeInTheDocument();
    expect(screen.getByText("明细.xlsx")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "预览 发票.pdf" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "预览 凭证.png" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "预览 明细.xlsx" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "下载 明细.xlsx" }),
    ).toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "预览 发票.pdf" }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByTitle("发票.pdf")).toBeInTheDocument();
    expect(
      within(dialog).getByRole("link", { name: "在新窗口打开" }),
    ).toHaveAttribute("href", "https://files.example.com/发票.pdf");
  });

  it("凭证失效时显示重新授权入口", async () => {
    stubBatchApi({
      tasksResponse: jsonResponse(
        { code: 99991679, message: "user access token 缺少审批 scope" },
        400,
      ),
    });
    renderTestApp({ path: "/batch-approval", authenticated: true });

    expect(
      await screen.findByText(/飞书授权已过期或尚未绑定/),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "重新授权飞书" }),
    ).toBeInTheDocument();
  });

  it("失败项可重试：提交一个带新幂等键的新 job", async () => {
    const calls = stubBatchApi({
      jobResponses: {
        "job-1": {
          job_id: "job-1",
          status: "failed",
          total: 1,
          succeeded: 0,
          failed: 1,
          unknown: 0,
          items: [
            {
              task_id: "t1",
              instance_code: "i1",
              title: "报销单 A",
              status: "failed",
              message: "任务状态已变化",
            },
          ],
        },
        "job-2": {
          job_id: "job-2",
          status: "succeeded",
          total: 1,
          succeeded: 1,
          failed: 0,
          unknown: 0,
          items: [
            {
              task_id: "t1",
              instance_code: "i1",
              title: "报销单 A",
              status: "succeeded",
            },
          ],
        },
      },
    });
    renderTestApp({ path: "/batch-approval", authenticated: true });

    const user = userEvent.setup();
    await screen.findByText("报销单 A");
    await user.click(screen.getByLabelText("选择 报销单 A"));
    await user.click(screen.getByRole("button", { name: "同意" }));
    await user.click(
      within(await screen.findByRole("dialog")).getByRole("button", {
        name: "确认同意",
      }),
    );

    const retryButton = await screen.findByRole("button", {
      name: /重试失败项/,
    });
    await user.click(retryButton);

    await waitFor(() => {
      const createCalls = calls.filter(
        (call) =>
          call.method === "POST" &&
          call.path === "/api/v1/feishu/batch-approval/jobs",
      );
      expect(createCalls).toHaveLength(2);
      expect(createCalls[1]?.body).toMatchObject({
        action: "pass",
        items: [{ task_id: "t1", instance_code: "i1" }],
      });
    });
    expect(await screen.findByText("全部成功")).toBeInTheDocument();
  });

  it("实例状态与任务状态按文档映射成中文，并支持逐列筛选", async () => {
    stubBatchApi({
      tasksResponse: jsonResponse({
        code: 0,
        data: {
          tasks: [
            {
              ...TASKS[0],
              instance_status: "1",
              title: "流转中的报销单",
            },
            {
              ...TASKS[1],
              topic: "1",
              title: "待办请假单",
            },
            {
              ...TASKS[0],
              task_id: "t4",
              instance_code: "i4",
              title: "待办报销单",
            },
          ],
          truncated: false,
        },
      }),
    });
    renderTestApp({ path: "/batch-approval", authenticated: true });

    const statusBadges = () =>
      Array.from(document.querySelectorAll('[data-slot="status-badge"]'));
    await waitFor(() => {
      expect(
        statusBadges().some((badge) => badge.textContent === "审批中"),
      ).toBe(true);
      expect(statusBadges().some((badge) => badge.textContent === "待办")).toBe(
        true,
      );
    });
    expect(screen.queryByText("状态 1")).not.toBeInTheDocument();

    const user = userEvent.setup();
    await filterColumn(user, "报销审批", "标题", "流转中的报销单");
    await waitFor(() => {
      expect(
        screen.queryByRole("cell", { name: "待办报销单" }),
      ).not.toBeInTheDocument();
    });
    expect(
      screen.getByRole("cell", { name: "流转中的报销单" }),
    ).toBeInTheDocument();
    expect(
      (await screen.findAllByRole("cell", { name: /待办请假单/ })).length,
    ).toBeGreaterThan(0);
  });
});

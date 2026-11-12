import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import * as XLSX from "xlsx";

import { clearStoredSession } from "@/engine/session/auth-session";
import { renderTestApp } from "@test/helpers/render-app";

vi.mock("pdfjs-dist/build/pdf.worker.min.mjs?worker", () => ({
  default: class {
    postMessage() {}
    terminate() {}
  },
}));

vi.mock("pdfjs-dist", () => {
  const page = {
    getViewport: () => ({ width: 100, height: 100 }),
    render: () => ({ promise: Promise.resolve(), cancel: vi.fn() }),
  };
  const document = { numPages: 5, getPage: vi.fn(async () => page) };
  return {
    PDFWorker: {
      create: vi.fn(() => ({ destroy: vi.fn() })),
    },
    getDocument: vi.fn(() => ({
      promise: Promise.resolve(document),
      destroy: vi.fn(),
    })),
  };
});

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
    within(active as HTMLElement).getByRole("button", {
      name: /OK|确\s*定/,
    }),
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
      { id: "amount", name: "金额", value: { text: "100" } },
      { id: "dept", name: "部门", value: "研发" },
    ],
    computed_fields: [
      {
        key: "computed:c7",
        name: "参考人民币金额",
        value: 8794.79,
        display: "8794.79",
        precision: 2,
      },
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
    form_fields: [{ id: "days", name: "天数", value: "2" }],
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
  viewConfig?: unknown;
  catalog?: unknown[];
  onSaveView?: (body: unknown) => void;
  detailRows?: (taskId: string, params: URLSearchParams) => unknown;
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
      if (url.pathname === "/api/v1/feishu/batch-approval/attachments") {
        const name = url.searchParams.get("name") ?? "";
        if (name.endsWith(".xlsx")) {
          const workbook = XLSX.utils.book_new();
          XLSX.utils.book_append_sheet(
            workbook,
            XLSX.utils.aoa_to_sheet([
              ["项目", "金额"],
              ["差旅", "100"],
            ]),
            "明细",
          );
          return new Response(
            XLSX.write(workbook, { type: "array", bookType: "xlsx" }),
            {
              headers: {
                "content-type":
                  "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
              },
            },
          );
        }
        return new Response(new Uint8Array([1, 2, 3]), {
          headers: {
            "content-type": name.endsWith(".pdf")
              ? "application/pdf"
              : "image/png",
          },
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
      const rowsMatch = url.pathname.match(
        /^\/api\/v1\/feishu\/batch-approval\/tasks\/([^/]+)\/rows$/,
      );
      if (rowsMatch) {
        return jsonResponse({
          code: 0,
          data: options.detailRows
            ? options.detailRows(rowsMatch[1], url.searchParams)
            : {
                columns: [],
                rows: [],
                total: 0,
                unfiltered_total: 0,
                offset: 0,
                limit: 50,
              },
        });
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
      if (url.pathname === "/api/v1/feishu/batch-approval/view-configs") {
        return jsonResponse({ code: 0, data: { views: {} } });
      }
      if (url.pathname === "/api/v1/feishu/batch-approval/field-catalog") {
        return jsonResponse({
          code: 0,
          data: { widgets: options.catalog ?? [] },
        });
      }
      if (url.pathname === "/api/v1/feishu/batch-approval/view-config") {
        if (method === "POST") {
          options.onSaveView?.(body);
          return jsonResponse({ code: 0, data: { view: body } });
        }
        return jsonResponse({
          code: 0,
          data: {
            approval_code: url.searchParams.get("approval_code") ?? "",
            view: options.viewConfig ?? null,
          },
        });
      }
      if (url.pathname === "/api/v1/feishu/batch-approval/view-config/reset") {
        return jsonResponse({ code: 0, data: { reset: true } });
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

  it("审批定义侧栏可收起，给表格腾出空间", async () => {
    stubBatchApi();
    renderTestApp({ path: "/batch-approval", authenticated: true });
    await screen.findByText("报销单 A");

    const user = userEvent.setup();
    await user.click(
      screen.getByRole("button", { name: "收起审批定义侧栏" }),
    );
    expect(screen.queryByText("全部待办")).not.toBeInTheDocument();

    await user.click(
      screen.getByRole("button", { name: "展开审批定义侧栏" }),
    );
    expect(screen.getByText("全部待办")).toBeInTheDocument();
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
        { id: "amount", name: "金额", value: { text: "200" } },
        { id: "dept", name: "部门", value: "研发" },
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
    expect(screen.getAllByText("参考人民币金额").length).toBeGreaterThan(0);
    expect(screen.getAllByText("8794.79").length).toBeGreaterThan(0);
    expect(screen.getByText("部门审批（我的节点）")).toBeInTheDocument();
    expect(screen.getByText("财务审批")).toBeInTheDocument();
    // 表头用类型标记区分实例数据、表单控件和个人计算字段。
    expect(screen.getAllByText("计算").length).toBeGreaterThan(0);
    expect(screen.getAllByText("表单").length).toBeGreaterThan(0);
    // 实例编号属于技术定位字段，默认不展示。
    expect(screen.queryByText("实例编号")).not.toBeInTheDocument();

    const user = userEvent.setup();
    await filterColumn(user, "报销审批", "金额", "100");
    expect(screen.getByText("报销单 A")).toBeInTheDocument();
    expect(screen.queryByText("报销单 A2")).not.toBeInTheDocument();

    await filterColumn(user, "请假审批", "当前节点", "主管审批");
    expect(screen.getByText("报销单 A")).toBeInTheDocument();
    expect(screen.getByText("请假单 B")).toBeInTheDocument();
    expect(screen.queryByText("外部单 C")).not.toBeInTheDocument();
  });

  it("字段设置按用户保存列可见性与计算字段规则", async () => {
    let saved: unknown;
    const view = {
      computed_fields: [
        {
          id: "c1",
          name: "参考人民币金额",
          left_field_id: "amount",
          left_field_name: "金额",
          operator: "multiply",
          right_field_id: "rate",
          right_field_name: "汇率",
          precision: 2,
          enabled: true,
        },
      ],
      columns: [
        { key: "base:title", visible: true },
        { key: "form:amount", visible: true },
        { key: "form:dept", visible: false },
        { key: "computed:c1", visible: true },
      ],
    };
    stubBatchApi({
      catalog: [
        { id: "amount", name: "金额", type: "number" },
        { id: "dept", name: "部门", type: "select" },
        { id: "rate", name: "汇率", type: "text" },
        {
          id: "items",
          name: "费用明细",
          type: "fieldList",
          children: ["itemName", "qty"],
        },
        {
          id: "itemName",
          name: "物品",
          type: "input",
          parent_id: "items",
        },
        { id: "qty", name: "数量", type: "number", parent_id: "items" },
      ],
      viewConfig: view,
      onSaveView: (body) => {
        saved = body;
      },
      tasksResponse: jsonResponse({
        code: 0,
        data: { tasks: [TASKS[0]], truncated: false, views: { D1: view } },
      }),
    });
    renderTestApp({ path: "/batch-approval", authenticated: true });

    await screen.findByText("报销单 A");
    // 已保存的布局生效：部门列隐藏，计算列照常展示。
    expect(screen.queryByText("部门")).not.toBeInTheDocument();
    expect(screen.getAllByText("参考人民币金额").length).toBeGreaterThan(0);

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "字段设置" }));
    const dialog = await screen.findByRole("dialog");
    expect(await within(dialog).findByText("显示字段")).toBeInTheDocument();
    // 配置面板同样标注每列的类型，便于区分实例数据与个人计算字段。
    expect(within(dialog).getAllByText("实例").length).toBeGreaterThan(0);
    expect(within(dialog).getAllByText("表单").length).toBeGreaterThan(0);

    // 明细内的列并入同一个字段设置面板，按明细分组。
    expect(
      within(dialog).getByText("明细内的列 · 费用明细"),
    ).toBeInTheDocument();
    expect(
      within(dialog).getByRole("checkbox", {
        name: "显示 费用明细 / 物品",
      }),
    ).toBeChecked();

    const dept = within(dialog).getByRole("checkbox", { name: "显示 部门" });
    expect(dept).not.toBeChecked();
    await user.click(dept);

    await user.click(within(dialog).getByRole("button", { name: "计算字段" }));
    expect(
      await within(dialog).findByText("参考人民币金额"),
    ).toBeInTheDocument();
    await user.click(
      within(dialog).getByRole("checkbox", { name: "启用 参考人民币金额" }),
    );
    await user.click(within(dialog).getByRole("button", { name: "保存" }));

    await waitFor(() => expect(saved).toBeDefined());
    expect(saved).toMatchObject({
      approval_code: "D1",
      computed_fields: [
        expect.objectContaining({
          id: "c1",
          name: "参考人民币金额",
          enabled: false,
        }),
      ],
    });
    const columns = (
      saved as { columns: Array<{ key: string; visible: boolean }> }
    ).columns;
    expect(columns).toContainEqual({ key: "form:dept", visible: true });
    expect(columns).toContainEqual({
      key: "detail:items:itemName",
      visible: true,
    });
    expect(columns.some((column) => column.key === "computed:c1")).toBe(false);
  });

  it("明细控件先显示行数与首行预览，展开后加载全部明细行", async () => {
    const detailTask = {
      ...TASKS[0],
      form_fields: [
        { id: "amount", name: "金额", value: { text: "100" } },
        {
          id: "items",
          name: "费用明细",
          field_type: "fieldList",
          value: null,
          row_count: 3,
          preview: ["笔记本", "4500"],
        },
      ],
    };
    stubBatchApi({
      tasksResponse: jsonResponse({
        code: 0,
        data: { tasks: [detailTask], truncated: false },
      }),
      detailRows: (taskId, params) => {
        expect(taskId).toBe("t1");
        expect(params.get("field_id")).toBe("items");
        return {
          field_id: "items",
          name: "费用明细",
          columns: [
            { id: "name", name: "物品", type: "input" },
            { id: "price", name: "单价", type: "amount" },
          ],
          rows: [
            [
              { id: "name", type: "input", value: "笔记本" },
              { id: "price", type: "amount", value: 4500 },
            ],
            [
              { id: "name", type: "input", value: "鼠标" },
              { id: "price", type: "amount", value: 120 },
            ],
            [
              { id: "name", type: "input", value: "键盘" },
              { id: "price", type: "amount", value: 300 },
            ],
          ],
          total: 3,
          unfiltered_total: 3,
          offset: 0,
          limit: 50,
        };
      },
    });
    renderTestApp({ path: "/batch-approval", authenticated: true });

    await screen.findByText("报销单 A");
    expect(
      document.querySelector('[data-slot="detail-count"]'),
    ).toHaveTextContent("3 行");
    expect(screen.getByText("首行：笔记本 · 4500")).toBeInTheDocument();
    expect(
      document.querySelector(".ant-table-row-expand-icon"),
    ).toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "全部展开" }));
    expect(await screen.findByText("物品")).toBeInTheDocument();
    expect(screen.getByText("笔记本")).toBeInTheDocument();
    expect(screen.getByText("鼠标")).toBeInTheDocument();
    expect(screen.getByText("键盘")).toBeInTheDocument();
    expect(screen.getByText("共 3 行 · 已加载 3 行")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "全部收起" }),
    ).toBeInTheDocument();
  });

  it("附件字段展示文件名，图片和 PDF 可预览，Excel 可下载", async () => {
    const attachmentTask = {
      ...TASKS[0],
      form_fields: [
        {
          id: "invoice",
          name: "发票",
          value: ["https://files.example.com/发票.pdf"],
          field_type: "attachmentV2",
          ext: "'发票.pdf'",
        },
        {
          id: "voucher",
          name: "凭证",
          value: ["https://files.example.com/凭证.png"],
          field_type: "imageV2",
          ext: "'凭证.png'",
        },
        {
          id: "detail",
          name: "明细",
          value: ["https://files.example.com/明细.xlsx"],
          field_type: "attachmentV2",
          ext: "'明细.xlsx'",
        },
        {
          id: "note",
          name: "说明",
          value: "请上传发票原件",
          field_type: "text",
          ext: null,
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
    expect(screen.queryByText("说明")).not.toBeInTheDocument();
    expect(screen.queryByText("请上传发票原件")).not.toBeInTheDocument();
    expect(screen.getAllByTitle("拖动调整列宽").length).toBeGreaterThan(0);
    expect(
      screen.getByRole("button", { name: "预览 发票.pdf" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "预览 凭证.png" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "预览 明细.xlsx" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "下载 明细.xlsx" }),
    ).toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "预览 发票.pdf" }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText("发票.pdf")).toBeInTheDocument();
    expect(
      await within(dialog).findByRole("button", { name: "下一页" }),
    ).toBeInTheDocument();
    expect(dialog.querySelector("iframe")).toBeNull();
    expect(
      within(dialog).getByRole("link", { name: "下载附件" }),
    ).toHaveAttribute("download", "发票.pdf");
  });

  it("一条审批含多个附件时可左右切换预览", async () => {
    const attachmentTask = {
      ...TASKS[0],
      form_fields: [
        {
          id: "attachments",
          name: "附件",
          value: [
            "https://files.example.com/第一份.pdf",
            "https://files.example.com/第二份.pdf",
          ],
          field_type: "attachmentV2",
          ext: "'第一份.pdf','第二份.pdf'",
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

    const user = userEvent.setup();
    await user.click(
      await screen.findByRole("button", { name: "预览 第一份.pdf" }),
    );
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText("1 / 2")).toBeInTheDocument();
    await user.click(
      within(dialog).getByRole("button", { name: "下一个附件" }),
    );
    expect(within(dialog).getByText("第二份.pdf")).toBeInTheDocument();
    expect(within(dialog).getByText("2 / 2")).toBeInTheDocument();
    await user.click(
      within(dialog).getByRole("button", { name: "下一个附件" }),
    );
    expect(within(dialog).getByText("第一份.pdf")).toBeInTheDocument();
    expect(within(dialog).getByText("1 / 2")).toBeInTheDocument();
  });

  it("Excel 附件在弹窗内解析为表格预览", async () => {
    const attachmentTask = {
      ...TASKS[0],
      form_fields: [
        {
          id: "detail",
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

    const user = userEvent.setup();
    await user.click(
      await screen.findByRole("button", { name: "预览 明细.xlsx" }),
    );
    const dialog = await screen.findByRole("dialog");
    expect(await within(dialog).findByText("差旅")).toBeInTheDocument();
    expect(within(dialog).getByText("金额")).toBeInTheDocument();
    expect(within(dialog).getByText("100")).toBeInTheDocument();
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

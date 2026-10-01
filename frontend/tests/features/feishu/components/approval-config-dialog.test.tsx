/**
 * 建配置向导（四步：坐标 → Code+控件预览 → 申请人/回填列+时区 → 提交）。
 *
 * 数据访问走注入的 `client`（真实实现在 `api.ts::useApprovalWizardClient`），
 * 不依赖会话与目录。
 *
 * 钉住的事：
 * - 第一步工具提示说明双权限要求（feishu.approval.write + feishu.datasource.write）；
 * - 表列表为空时允许手填 table_id；
 * - 第二步控件预览展示 id/名称/类型/必填；
 * - 第三步字段下拉 + 手填切换、时区默认 Asia/Shanghai；
 * - 第四步提交六件套；失败原因展示在最后一步。
 */

import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { ApprovalConfigDialog } from "@/features/feishu/components/ApprovalConfigDialog";
import type { ApprovalWizardClient } from "@/features/feishu/api";
import type { BitableField, BitableTable } from "@/features/feishu/types";

const BASE_TOKEN = "appbcbWCzen6";
const TABLE_ID = "tblsRc9GRRX";
const FIELDS: BitableField[] = [
  { fieldId: "fldApp", fieldName: "申请人", type: 1 },
  { fieldId: "fldBack", fieldName: "审批编号", type: 1 },
];

const TABLES: BitableTable[] = [{ tableId: TABLE_ID, name: "差旅报销台账" }];

function stubClient(
  overrides: Partial<ApprovalWizardClient> = {},
): ApprovalWizardClient {
  return {
    listTables: vi.fn().mockResolvedValue(TABLES),
    listViews: vi.fn().mockResolvedValue([]),
    listFields: vi.fn().mockResolvedValue(FIELDS),
    listWidgets: vi.fn().mockResolvedValue([
      { id: "widget-1", name: "费用金额", type: "input", required: true },
      { id: "widget-2", name: "出差原因", type: "textarea", required: false },
    ]),
    createConfig: vi.fn().mockResolvedValue(7),
    ...overrides,
  };
}

function renderDialog(client: ApprovalWizardClient) {
  return render(
    <ApprovalConfigDialog
      client={client}
      onCancel={vi.fn()}
      onSubmitted={vi.fn()}
    />,
  );
}

/// 走到第二步（第一步：填 Base Token → 拉表 → 选表 → 下一步）。
async function driveToStep2(
  user: ReturnType<typeof userEvent.setup>,
  client: ApprovalWizardClient,
  props: { onCancel?: () => void; onSubmitted?: () => void } = {},
) {
  render(
    <ApprovalConfigDialog
      client={client}
      onCancel={props.onCancel ?? vi.fn()}
      onSubmitted={props.onSubmitted ?? vi.fn()}
    />,
  );
  await user.type(screen.getByLabelText("多维表格 Base Token"), BASE_TOKEN);
  await user.click(screen.getByRole("button", { name: "拉取数据表" }));
  await user.click(await screen.findByRole("combobox", { name: "数据表" }));
  await user.click(await screen.findByRole("option", { name: /差旅报销台账/ }));
  await user.click(screen.getByRole("button", { name: "下一步" }));
}

describe("建配置向导 · 第一步（坐标）", () => {
  it("工具提示说明双权限要求", async () => {
    renderDialog(stubClient());
    expect(
      await screen.findByText(
        /feishu.approval.write 与 feishu.datasource.write/,
      ),
    ).toBeInTheDocument();
  });

  it("表列表为空时允许手填 table_id", async () => {
    const client = stubClient({
      listTables: vi.fn().mockResolvedValue([]),
    });
    renderDialog(client);
    await userEvent
      .setup()
      .type(screen.getByLabelText("多维表格 Base Token"), BASE_TOKEN);
    await userEvent
      .setup()
      .click(screen.getByRole("button", { name: "拉取数据表" }));

    const input = await screen.findByLabelText("数据表 ID（手填）");
    await userEvent.setup().type(input, TABLE_ID);
    // 手填 table_id 后第一步即可继续
    expect(screen.getByRole("button", { name: "下一步" })).toBeEnabled();
  });

  it("表列表拉取失败时切手填并显示可见错误（不卡死第一步）", async () => {
    // 回归（对抗性验证抓到）：失败路径原先只 setSubmitError 且该错误只在第四步
    // 渲染——身份缺 feishu.datasource.write 时向导彻底卡死在第一步且无提示。
    const client = stubClient({
      listTables: vi
        .fn()
        .mockRejectedValue(
          new Error("目录里没有该操作：feishu.datasource.list_bitable_tables"),
        ),
    });
    renderDialog(client);
    await userEvent
      .setup()
      .type(screen.getByLabelText("多维表格 Base Token"), BASE_TOKEN);
    await userEvent
      .setup()
      .click(screen.getByRole("button", { name: "拉取数据表" }));

    // 手填输入框出现（而不是停在“没有可选项”的禁用态）
    const input = await screen.findByLabelText("数据表 ID（手填）");
    await userEvent.setup().type(input, TABLE_ID);
    // 失败原因在第一步就地可见
    expect(screen.getByRole("alert")).toHaveTextContent("数据表拉取失败");
    expect(screen.getByRole("button", { name: "下一步" })).toBeEnabled();
  });
});

describe("建配置向导 · 第二步（审批 Code + 控件预览）", () => {
  it("预览展示 id/名称/类型/必填", async () => {
    const user = userEvent.setup();
    const client = stubClient();
    await driveToStep2(user, client);

    await user.type(screen.getByLabelText("审批定义 Code"), "CODE-TRAVEL");
    await user.click(screen.getByRole("button", { name: "预览控件" }));

    expect(await screen.findByText("widget-1")).toBeInTheDocument();
    expect(screen.getByText("费用金额")).toBeInTheDocument();
    expect(screen.getByText("input")).toBeInTheDocument();
    expect(screen.getByText("textarea")).toBeInTheDocument();
    // 表头「必填」+ widget-1 的行值各一处；widget-2 是「可选」
    expect(screen.getAllByText("必填").length).toBe(2);
    expect(screen.getByText("可选")).toBeInTheDocument();
  });
});

describe("建配置向导 · 第三步（列 + 时区）", () => {
  it("字段下拉选择 + 时区默认 Asia/Shanghai", async () => {
    const user = userEvent.setup();
    const client = stubClient();
    await driveToStep2(user, client);
    await user.type(screen.getByLabelText("审批定义 Code"), "CODE-TRAVEL");
    await user.click(screen.getByRole("button", { name: "下一步" }));

    const applicant = await screen.findByRole("combobox", {
      name: "申请人员字段",
    });
    await user.click(applicant);
    await user.click(await screen.findByRole("option", { name: /申请人/ }));
    await user.click(screen.getByRole("combobox", { name: "回填字段" }));
    await user.click(await screen.findByRole("option", { name: /审批编号/ }));

    expect(screen.getByLabelText("Base 时区（IANA 名）")).toHaveValue(
      "Asia/Shanghai",
    );
    expect(screen.getByRole("button", { name: "下一步" })).toBeEnabled();
  });

  it("列表没有想要的列时允许手填 field_id", async () => {
    const user = userEvent.setup();
    const client = stubClient({
      listFields: vi.fn().mockResolvedValue([]),
    });
    await driveToStep2(user, client);
    await user.type(screen.getByLabelText("审批定义 Code"), "CODE-TRAVEL");
    await user.click(screen.getByRole("button", { name: "下一步" }));

    // 字段列表为空 → 直接是两个文本框
    const inputs = await screen.findAllByPlaceholderText("field_id 或列名");
    expect(inputs.length).toBe(2);
    await user.type(inputs[0]!, "申请人");
    await user.type(inputs[1]!, "审批编号");
    expect(screen.getByRole("button", { name: "下一步" })).toBeEnabled();
  });
});

describe("建配置向导 · 第四步（提交）", () => {
  it("提交 create_config 六件套，成功后回调 onSubmitted", async () => {
    const user = userEvent.setup();
    const onSubmitted = vi.fn();
    const client = stubClient();
    await driveToStep2(user, client, { onSubmitted });
    await user.type(screen.getByLabelText("审批定义 Code"), "CODE-TRAVEL");
    await user.click(screen.getByRole("button", { name: "下一步" }));

    const applicant = await screen.findByRole("combobox", {
      name: "申请人员字段",
    });
    await user.click(applicant);
    await user.click(await screen.findByRole("option", { name: /申请人/ }));
    await user.click(screen.getByRole("combobox", { name: "回填字段" }));
    await user.click(await screen.findByRole("option", { name: /审批编号/ }));
    await user.click(screen.getByRole("button", { name: "下一步" }));

    // 第四步摘要
    expect(await screen.findByText("创建配置")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "创建配置" }));

    await waitFor(() => {
      expect(client.createConfig).toHaveBeenCalledWith({
        baseToken: BASE_TOKEN,
        tableId: TABLE_ID,
        approvalCode: "CODE-TRAVEL",
        applicantField: "fldApp",
        backfillField: "fldBack",
        baseTimezone: "Asia/Shanghai",
      });
    });
    await waitFor(() => expect(onSubmitted).toHaveBeenCalled());
  });

  it("提交失败：服务端原因展示在最后一步，向导不关", async () => {
    const user = userEvent.setup();
    const onCancel = vi.fn();
    const client = stubClient({
      createConfig: vi
        .fn()
        .mockRejectedValue(new Error("该多维表格已配置，请先查询或删除后重建")),
    });
    await driveToStep2(user, client, { onCancel });
    await user.type(screen.getByLabelText("审批定义 Code"), "CODE-TRAVEL");
    await user.click(screen.getByRole("button", { name: "下一步" }));
    await user.click(
      await screen.findByRole("combobox", { name: "申请人员字段" }),
    );
    await user.click(await screen.findByRole("option", { name: /申请人/ }));
    await user.click(screen.getByRole("combobox", { name: "回填字段" }));
    await user.click(await screen.findByRole("option", { name: /审批编号/ }));
    await user.click(screen.getByRole("button", { name: "下一步" }));

    await user.click(await screen.findByRole("button", { name: "创建配置" }));

    // 原因展示在最后一步（role=alert）
    const alert = await screen.findByRole("alert");
    expect(within(alert).getByText(/该多维表格已配置/)).toBeInTheDocument();
    expect(onCancel).not.toHaveBeenCalled();
  });
});

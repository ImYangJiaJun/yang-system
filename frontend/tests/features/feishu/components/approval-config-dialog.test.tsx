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
import type {
  ApprovalConfigItem,
  BitableField,
  BitableTable,
} from "@/features/feishu/types";

const BASE_TOKEN = "appbcbWCzen6";
const TABLE_ID = "tblsRc9GRRX";
const FIELDS: BitableField[] = [
  { fieldId: "fldApp", fieldName: "申请人", type: 1 },
  { fieldId: "fldBack", fieldName: "审批编号", type: 1 },
  { fieldId: "fldAmount", fieldName: "费用金额", type: 1 },
];

const TABLES: BitableTable[] = [{ tableId: TABLE_ID, name: "差旅报销台账" }];

const SAVED_CONFIG: ApprovalConfigItem = {
  id: 7,
  title: "测试配置",
  baseToken: BASE_TOKEN,
  tableId: TABLE_ID,
  approvalCode: "CODE-TRAVEL",
  applicantField: "fldApp",
  backfillField: "fldBack",
  baseTimezone: "Asia/Shanghai",
  enabled: true,
  formSnapshotAt: null,
  updatedAt: 1,
  maps: [
    {
      widgetId: "widget-1",
      widgetName: "费用金额",
      widgetType: "input",
      bitableField: "fldAmount",
      bitableFieldName: "费用金额",
      required: true,
      converter: "direct",
    },
  ],
};

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
    updateConfig: vi.fn().mockResolvedValue(undefined),
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
  it("拉取期间禁用空下拉，成功显示数量并保留两张表供选择", async () => {
    const user = userEvent.setup();
    let resolveTables!: (tables: BitableTable[]) => void;
    const client = stubClient({
      listTables: vi.fn().mockReturnValue(
        new Promise<BitableTable[]>((resolve) => {
          resolveTables = resolve;
        }),
      ),
    });
    renderDialog(client);
    expect(screen.getByRole("combobox", { name: "数据表" })).toBeDisabled();
    await user.type(screen.getByLabelText("多维表格 Base Token"), BASE_TOKEN);
    await user.click(screen.getByRole("button", { name: "拉取数据表" }));
    expect(screen.getByRole("combobox", { name: "数据表" })).toBeDisabled();
    resolveTables([
      { tableId: "tblEOjX9Hc1XHHZ4", name: "技术测试专用" },
      { tableId: "tblJAv5MdIczULFt", name: "技术测试专用_提交" },
    ]);
    expect(await screen.findByRole("status")).toHaveTextContent(
      "已拉取 2 张数据表",
    );
    await user.click(screen.getByRole("combobox", { name: "数据表" }));
    expect(await screen.findAllByRole("option")).toHaveLength(2);
    await user.click(screen.getByRole("option", { name: /技术测试专用_提交/ }));
    expect(screen.getByRole("button", { name: "下一步" })).toBeEnabled();
  });

  it("失败后重试为空时清除旧错误并明确提示空结果", async () => {
    const user = userEvent.setup();
    renderDialog(
      stubClient({
        listTables: vi
          .fn()
          .mockRejectedValueOnce(new Error("临时失败"))
          .mockResolvedValueOnce([]),
      }),
    );
    await user.type(screen.getByLabelText("多维表格 Base Token"), BASE_TOKEN);
    await user.click(screen.getByRole("button", { name: "拉取数据表" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("临时失败");
    await user.click(screen.getByRole("button", { name: "拉取数据表" }));
    expect(await screen.findByRole("status")).toHaveTextContent(
      "未查询到可用的数据表",
    );
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.getByLabelText("数据表 ID（手填）")).toBeInTheDocument();
  });

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
  it("必填未选择时阻止继续；明细控件优先匹配带父名称的列", async () => {
    const user = userEvent.setup();
    const client = stubClient({
      listWidgets: vi.fn().mockResolvedValue([
        {
          id: "widget-1",
          name: "金额",
          qualifiedName: "付款明细_金额",
          type: "input",
          required: true,
        },
        { id: "widget-2", name: "出差原因", type: "textarea", required: true },
      ]),
      listFields: vi
        .fn()
        .mockResolvedValue([
          ...FIELDS,
          { fieldId: "bare", fieldName: "金额", type: 1 },
          { fieldId: "qualified", fieldName: "付款明细_金额", type: 1 },
        ]),
    });
    await driveToStep2(user, client);
    expect(client.listFields).not.toHaveBeenCalled();
    await user.type(screen.getByLabelText("审批定义 Code"), "CODE-TRAVEL");
    await user.click(screen.getByRole("button", { name: "预览控件" }));
    expect(
      await screen.findByRole("combobox", {
        name: "付款明细_金额对应的多维表格列",
      }),
    ).toHaveTextContent("付款明细_金额");
    expect(screen.getByRole("button", { name: "下一步" })).toBeDisabled();
    await user.click(
      screen.getByRole("combobox", { name: "出差原因对应的多维表格列" }),
    );
    await user.click(await screen.findByRole("option", { name: /费用金额/ }));
    expect(screen.getByRole("button", { name: "下一步" })).toBeEnabled();
  });

  it("关联审批显示超链接或实例 Code 的映射说明并按名预选", async () => {
    const user = userEvent.setup();
    const client = stubClient({
      listWidgets: vi
        .fn()
        .mockResolvedValue([
          { id: "connect-1", name: "原申请", type: "connect", required: true },
        ]),
      listFields: vi
        .fn()
        .mockResolvedValue([
          ...FIELDS,
          { fieldId: "fldLink", fieldName: "原申请", type: 15 },
        ]),
    });
    await driveToStep2(user, client);
    await user.type(screen.getByLabelText("审批定义 Code"), "CODE-TRAVEL");
    await user.click(screen.getByRole("button", { name: "预览控件" }));
    expect(
      await screen.findByText(/关联审批：选择超链接列或实例 Code 文本列/),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("combobox", { name: "原申请对应的多维表格列" }),
    ).toHaveTextContent("原申请");
    expect(screen.getByRole("button", { name: "下一步" })).toBeEnabled();
  });

  it("预览失败明确显示原因，重试成功后可继续；更改 Code 要重新预览", async () => {
    const user = userEvent.setup();
    const client = stubClient();
    vi.mocked(client.listFields).mockRejectedValueOnce(
      new Error("没有读取权限"),
    );
    await driveToStep2(user, client);
    await user.type(screen.getByLabelText("审批定义 Code"), "CODE-TRAVEL");
    await user.click(screen.getByRole("button", { name: "预览控件" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("没有读取权限");
    expect(screen.getByRole("button", { name: "下一步" })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "预览控件" }));
    await screen.findByRole("combobox", { name: "费用金额对应的多维表格列" });
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "下一步" })).toBeEnabled();
    await user.type(screen.getByLabelText("审批定义 Code"), "-NEW");
    expect(screen.getByRole("button", { name: "下一步" })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "预览控件" }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "下一步" })).toBeEnabled(),
    );
    expect(client.listWidgets).toHaveBeenLastCalledWith("CODE-TRAVEL-NEW");
  });

  it("首次按名预选列，手改后重新预览保留选择", async () => {
    const user = userEvent.setup();
    const client = stubClient({
      listFields: vi
        .fn()
        .mockResolvedValue([
          ...FIELDS,
          { fieldId: "fldOther", fieldName: "实际付款金额", type: 1 },
        ]),
    });
    await driveToStep2(user, client);
    await user.type(screen.getByLabelText("审批定义 Code"), "CODE-TRAVEL");
    await user.click(screen.getByRole("button", { name: "预览控件" }));
    const pick = await screen.findByRole("combobox", {
      name: "费用金额对应的多维表格列",
    });
    expect(pick).toHaveTextContent("费用金额");
    await user.click(pick);
    await user.click(
      await screen.findByRole("option", { name: /实际付款金额/ }),
    );
    await user.click(screen.getByRole("button", { name: "预览控件" }));
    expect(
      await screen.findByRole("combobox", { name: "费用金额对应的多维表格列" }),
    ).toHaveTextContent("实际付款金额");
    await user.click(screen.getByRole("button", { name: "下一步" }));
    await user.click(screen.getByRole("combobox", { name: "申请人员字段" }));
    await user.click(await screen.findByRole("option", { name: /申请人/ }));
    await user.click(screen.getByRole("combobox", { name: "回填字段" }));
    await user.click(await screen.findByRole("option", { name: /审批编号/ }));
    await user.click(screen.getByRole("button", { name: "下一步" }));
    expect(screen.getByText("费用金额 → 实际付款金额")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "创建配置" }));
    await waitFor(() =>
      expect(client.createConfig).toHaveBeenCalledWith(
        expect.objectContaining({
          maps: [{ widgetId: "widget-1", bitableField: "fldOther" }],
        }),
      ),
    );
  }, 15000);

  it("预览展示 id/名称/类型/必填", async () => {
    const user = userEvent.setup();
    const client = stubClient();
    await driveToStep2(user, client);

    await user.type(screen.getByLabelText("审批定义 Code"), "CODE-TRAVEL");
    await user.click(screen.getByRole("button", { name: "预览控件" }));

    expect(await screen.findByText("widget-1")).toBeInTheDocument();
    expect(screen.getAllByText(/费用金额/).length).toBeGreaterThan(0);
    expect(screen.getByText("input")).toBeInTheDocument();
    expect(screen.getByText("textarea")).toBeInTheDocument();
    // 表头「必填」+ widget-1 的行值各一处；widget-2 是「可选」
    expect(screen.getAllByText("必填").length).toBe(2);
    expect(screen.getByText("可选")).toBeInTheDocument();
  });
});

describe("编辑字段映射", () => {
  it.each([
    ["费用金额", false],
    ["费用金额", true],
    ["明细_费用金额", false],
    ["明细_费用金额", true],
  ] as const)(
    "明细父级只展示，子控件自动匹配列 %s（父级存在同名列：%s）",
    async (columnName, sameNameColumn) => {
      const user = userEvent.setup();
      const client = stubClient({
        listWidgets: vi.fn().mockResolvedValue([
          { id: "detail", name: "明细", type: "fieldList", required: true },
          {
            id: "widget-1",
            name: "费用金额",
            qualifiedName: "明细_费用金额",
            type: "input",
            required: true,
          },
        ]),
        listFields: vi
          .fn()
          .mockResolvedValue([
            ...FIELDS.map((field) =>
              field.fieldId === "fldAmount"
                ? { ...field, fieldName: columnName }
                : field,
            ),
            ...(sameNameColumn
              ? [{ fieldId: "fldDetail", fieldName: "明细", type: 1 }]
              : []),
          ]),
      });
      await driveToStep2(user, client);
      await user.type(screen.getByLabelText("审批定义 Code"), "CODE-TRAVEL");
      await user.click(screen.getByRole("button", { name: "预览控件" }));
      const parent = (await screen.findByText("detail")).closest("tr");
      expect(parent).toHaveTextContent("明细");
      expect(parent).toHaveTextContent("明细分组，无需选列");
      expect(parent?.querySelector('[role="combobox"]')).toBeNull();
      expect(
        screen.getByRole("combobox", { name: "明细_费用金额对应的多维表格列" }),
      ).toHaveTextContent(columnName);
      expect(screen.getByRole("button", { name: "下一步" })).toBeEnabled();
      await user.click(screen.getByRole("button", { name: "下一步" }));
      await user.click(screen.getByRole("combobox", { name: "申请人员字段" }));
      await user.click(await screen.findByRole("option", { name: /申请人/ }));
      await user.click(screen.getByRole("combobox", { name: "回填字段" }));
      await user.click(await screen.findByRole("option", { name: /审批编号/ }));
      await user.click(screen.getByRole("button", { name: "下一步" }));
      expect(screen.getByText("明细（明细分组）")).toBeInTheDocument();
      expect(
        screen.getByText(`明细_费用金额 → ${columnName}`),
      ).toBeInTheDocument();
      await user.click(screen.getByRole("button", { name: "创建配置" }));
      await waitFor(() =>
        expect(client.createConfig).toHaveBeenCalledWith(
          expect.objectContaining({
            maps: [{ widgetId: "widget-1", bitableField: "fldAmount" }],
          }),
        ),
      );
    },
    15000,
  );

  it("保留原映射与可选不映射，确认显示名称并调用更新", async () => {
    const user = userEvent.setup();
    const client = stubClient({
      listFields: vi
        .fn()
        .mockResolvedValue([
          ...FIELDS,
          { fieldId: "reason", fieldName: "出差原因", type: 1 },
        ]),
    });
    render(
      <ApprovalConfigDialog
        client={client}
        onCancel={vi.fn()}
        initialConfig={SAVED_CONFIG}
      />,
    );
    await user.click(screen.getByRole("button", { name: "预览控件" }));
    expect(
      await screen.findByRole("combobox", { name: "出差原因对应的多维表格列" }),
    ).toHaveTextContent("不映射");
    await user.click(screen.getByRole("button", { name: "下一步" }));
    await user.click(screen.getByRole("button", { name: "下一步" }));
    expect(screen.getByText("费用金额 → 费用金额")).toBeInTheDocument();
    expect(screen.getByText("出差原因 → 不映射")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() =>
      expect(client.updateConfig).toHaveBeenCalledWith(7, {
        maps: [{ widgetId: "widget-1", bitableField: "fldAmount" }],
        baseTimezone: "Asia/Shanghai",
      }),
    );
    expect(client.createConfig).not.toHaveBeenCalled();
  });

  it.each(["deleted-column", "removed-widget"])(
    "旧映射失效 %s 时阻止继续且允许修复",
    async (kind) => {
      const user = userEvent.setup();
      const config = {
        ...SAVED_CONFIG,
        maps: [
          ...SAVED_CONFIG.maps,
          {
            ...SAVED_CONFIG.maps[0],
            widgetId: kind === "removed-widget" ? "removed" : "widget-2",
            widgetName: "旧备注",
            bitableField: "deleted",
            required: false,
          },
        ],
      };
      render(
        <ApprovalConfigDialog
          client={stubClient()}
          onCancel={vi.fn()}
          initialConfig={config}
        />,
      );
      await user.click(screen.getByRole("button", { name: "预览控件" }));
      await screen.findByRole("combobox", { name: "费用金额对应的多维表格列" });
      expect(screen.getByRole("button", { name: "下一步" })).toBeDisabled();
      if (kind === "removed-widget") {
        expect(screen.getByRole("alert")).toHaveTextContent("旧备注");
        await user.click(screen.getByRole("button", { name: "清除旧映射" }));
      } else {
        const pick = screen.getByRole("combobox", {
          name: "出差原因对应的多维表格列",
        });
        expect(pick).toHaveTextContent("列已删除");
        await user.click(pick);
        await user.click(await screen.findByRole("option", { name: "不映射" }));
      }
      expect(screen.getByRole("button", { name: "下一步" })).toBeEnabled();
    },
  );
});

describe("建配置向导 · 第三步（列 + 时区）", () => {
  it("字段下拉选择 + 时区默认 Asia/Shanghai", async () => {
    const user = userEvent.setup();
    const client = stubClient();
    await driveToStep2(user, client);
    await user.type(screen.getByLabelText("审批定义 Code"), "CODE-TRAVEL");
    await user.click(screen.getByRole("button", { name: "预览控件" }));
    await screen.findByRole("combobox", { name: "费用金额对应的多维表格列" });
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

  it("字段拉取为空时显示错误并阻止保存空映射", async () => {
    const user = userEvent.setup();
    const client = stubClient({
      listFields: vi.fn().mockResolvedValue([]),
    });
    await driveToStep2(user, client);
    await user.type(screen.getByLabelText("审批定义 Code"), "CODE-TRAVEL");
    await user.click(screen.getByRole("button", { name: "预览控件" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("没有可用列");
    expect(screen.getByRole("button", { name: "下一步" })).toBeDisabled();
  });
});

describe("建配置向导 · 第四步（提交）", () => {
  it("提交 create_config 六件套，成功后回调 onSubmitted", async () => {
    const user = userEvent.setup();
    const onSubmitted = vi.fn();
    const client = stubClient();
    await driveToStep2(user, client, { onSubmitted });
    await user.type(screen.getByLabelText("审批定义 Code"), "CODE-TRAVEL");
    await user.click(screen.getByRole("button", { name: "预览控件" }));
    await screen.findByRole("combobox", { name: "费用金额对应的多维表格列" });
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
        maps: [{ widgetId: "widget-1", bitableField: "fldAmount" }],
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
    await user.click(screen.getByRole("button", { name: "预览控件" }));
    await screen.findByRole("combobox", { name: "费用金额对应的多维表格列" });
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

/**
 * 表级配置向导：选表 → 选视图 → 勾字段 → 配标识与父列。
 *
 * 这一份用例钉的都是**讲错的代价**远大于讲少的几件事：
 * - 视图的作用域（实测列出字段的 `view_id` 不生效，说错会让运维白折腾）；
 * - 字段列表不按类型过滤（设计决策：全列出来自己判断，但类型码必须带出来）；
 * - 父列下拉只列**已勾选**的其它字段（父列没勾就等于读不到它的文案）；
 * - `source_key` 的默认值必须直接可用（形状由后端 `valid_source_key` 定）。
 *
 * 数据访问走注入的 `client`（真实实现在 `api.ts` 的 `useTableWizardClient()`），
 * 因此这里不依赖会话与界面目录——那两样由 `api.test.ts` 单独钉住。
 */

import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { DatasourceTableWizard } from "@/features/feishu/components/DatasourceTableWizard";
import type {
  CreateTableSubmission,
  TableWizardClient,
} from "@/features/feishu/api";
import { SOURCE_KEY_PATTERN, fieldTypeLabel } from "@/features/feishu/types";

const APP_TOKEN = "ZoCWb82JQaCCiAspCqbcUvlsnwg";
const TABLE_ID = "tblauuOafa4acvT3";
const VIEW_ID = "vewAEKSbvO";

/// 目标表实测的四个字段。刻意混入公式（20）与一个未收录的类型码（999）：
/// 全部列出是设计决策，任何「只列单选/多选」的过滤都会让这两条消失。
const FIELDS = [
  { fieldId: "fldTyg5VBz", fieldName: "费用大类/Main Exp Cat*", type: 3 },
  { fieldId: "fldEblAr7X", fieldName: "费用类型/Fee Type*", type: 3 },
  { fieldId: "fldM0j5Do3", fieldName: "银行流水摘要-编码", type: 20 },
  { fieldId: "fldQ7LcB6y", fieldName: "公司名称/Company name", type: 999 },
];

const TABLES = [{ tableId: TABLE_ID, name: "公司往来付款台账" }];
const VIEWS = [
  { viewId: VIEW_ID, viewName: "全部记录", viewType: "grid" },
  { viewId: "vewOTHER", viewName: "只看本月", viewType: "grid" },
];

function stubClient(
  overrides: Partial<TableWizardClient> = {},
): TableWizardClient {
  return {
    listTables: vi.fn().mockResolvedValue(TABLES),
    listViews: vi.fn().mockResolvedValue(VIEWS),
    listFields: vi.fn().mockResolvedValue(FIELDS),
    createTable: vi
      .fn()
      .mockResolvedValue({ datasourceId: 7, credentials: [] }),
    ...overrides,
  };
}

/// 走到「勾字段」那一步：填名称与 app_token → 拉表 → 选表 → 下一步 → 选视图 → 下一步。
async function driveToFields(
  user: ReturnType<typeof userEvent.setup>,
  client: TableWizardClient,
  options: { renderWizard?: boolean } = {},
) {
  if (options.renderWizard !== false) {
    render(
      <DatasourceTableWizard
        client={client}
        onCancel={vi.fn()}
        onSubmitted={vi.fn()}
      />,
    );
  }

  await user.type(screen.getByLabelText(/名称/), "公司往来付款");
  await user.type(screen.getByLabelText(/Base Token/), APP_TOKEN);
  await user.click(screen.getByRole("button", { name: "拉取数据表" }));

  await user.click(await screen.findByRole("combobox", { name: "数据表" }));
  await user.click(
    await screen.findByRole("option", { name: /公司往来付款台账/ }),
  );
  await user.click(screen.getByRole("button", { name: "下一步" }));

  await user.click(await screen.findByRole("combobox", { name: "视图" }));
  await user.click(await screen.findByRole("option", { name: /全部记录/ }));
  await user.click(screen.getByRole("button", { name: "下一步" }));
}

describe("配置向导 · 视图选择器", () => {
  it("视图选择器说明它只决定拉取哪些行，不决定能勾哪些字段", async () => {
    // 实测：列出字段的 view_id 参数不生效。UI 若把两件事讲成一件事，
    // 运维会以为换视图能换出一批字段。
    const user = userEvent.setup();
    const client = stubClient();
    render(
      <DatasourceTableWizard
        client={client}
        onCancel={vi.fn()}
        onSubmitted={vi.fn()}
      />,
    );

    await user.type(screen.getByLabelText(/名称/), "公司往来付款");
    await user.type(screen.getByLabelText(/Base Token/), APP_TOKEN);
    await user.click(screen.getByRole("button", { name: "拉取数据表" }));
    await user.click(await screen.findByRole("combobox", { name: "数据表" }));
    await user.click(
      await screen.findByRole("option", { name: /公司往来付款台账/ }),
    );
    await user.click(screen.getByRole("button", { name: "下一步" }));

    expect(await screen.findByText(/只决定拉取哪些行/)).toBeInTheDocument();
    expect(screen.getByText(/不决定能勾哪些字段/)).toBeInTheDocument();
  });
});

describe("配置向导 · 字段勾选与父列", () => {
  it("字段列表全部列出、不按类型过滤，但带出类型码", async () => {
    const user = userEvent.setup();
    const client = stubClient();
    await driveToFields(user, client);

    // 公式列与未收录的类型码都在，且各自的类型码可见
    for (const field of FIELDS) {
      expect(await screen.findByLabelText(field.fieldName)).toBeInTheDocument();
    }
    expect(screen.getAllByText("3 单选")).toHaveLength(2);
    expect(screen.getByText("20 公式")).toBeInTheDocument();
    expect(screen.getByText("999 未收录")).toBeInTheDocument();
    expect(fieldTypeLabel(999)).toBe("999 未收录");
  });

  it("勾选后可为字段指定父列，且父列下拉只列已勾选项", async () => {
    const user = userEvent.setup();
    const client = stubClient();
    await driveToFields(user, client);

    await user.click(await screen.findByLabelText("费用大类/Main Exp Cat*"));
    await user.click(await screen.findByLabelText("费用类型/Fee Type*"));
    await user.click(screen.getByRole("button", { name: "下一步" }));

    // 两行「父列」，行序 = 字段列表序（费用大类在前）：打开费用类型那一行的下拉
    const parentSelects = await screen.findAllByLabelText("父列");
    await user.click(parentSelects[1]);

    // 只能在自己的队友里挑：自己不算候选，没勾的字段也不在候选里
    expect(
      screen.queryByRole("option", { name: "费用类型/Fee Type*" }),
    ).toBeNull();
    expect(
      screen.queryByRole("option", { name: "公司名称/Company name" }),
    ).toBeNull();
    expect(
      await screen.findByRole("option", { name: "费用大类/Main Exp Cat*" }),
    ).toBeInTheDocument();
  });

  it("source_key 默认按 field_id 派生，且直接可用（形状合法）", async () => {
    const user = userEvent.setup();
    const client = stubClient();
    await driveToFields(user, client);

    await user.click(await screen.findByLabelText("费用类型/Fee Type*"));
    await user.click(screen.getByRole("button", { name: "下一步" }));

    const sourceKey = await screen.findByLabelText("源标识");
    // field_id 带大写，派生值必须落到合法形状里（大写会被后端拒）
    expect(sourceKey).toHaveValue("fldeblar7x");
    expect(SOURCE_KEY_PATTERN.test("fldeblar7x")).toBe(true);
  });

  it("提交的父指针是 field_id，不是 source_key", async () => {
    const user = userEvent.setup();
    const createTable = vi
      .fn()
      .mockResolvedValue({ datasourceId: 7, credentials: [] });
    const client = stubClient({ createTable });
    await driveToFields(user, client);

    await user.click(await screen.findByLabelText("费用大类/Main Exp Cat*"));
    await user.click(await screen.findByLabelText("费用类型/Fee Type*"));
    await user.click(screen.getByRole("button", { name: "下一步" }));

    // 两行「父列」：给费用类型选一个父（费用大类）
    const parentSelects = await screen.findAllByLabelText("父列");
    await user.click(parentSelects[1]);
    await user.click(
      await screen.findByRole("option", { name: "费用大类/Main Exp Cat*" }),
    );

    await user.click(screen.getByRole("button", { name: "创建数据源" }));

    const submission = createTable.mock.calls[0]?.[0] as CreateTableSubmission;
    expect(submission.appToken).toBe(APP_TOKEN);
    expect(submission.tableId).toBe(TABLE_ID);
    expect(submission.viewId).toBe(VIEW_ID);
    expect(submission.fields).toEqual([
      {
        fieldId: "fldTyg5VBz",
        fieldName: "费用大类/Main Exp Cat*",
        type: 3,
        sourceKey: "fldtyg5vbz",
        parentFieldId: null,
      },
      {
        fieldId: "fldEblAr7X",
        fieldName: "费用类型/Fee Type*",
        type: 3,
        sourceKey: "fldeblar7x",
        parentFieldId: "fldTyg5VBz",
      },
    ]);
  });

  it("一个字段都不勾时不能进下一步（后端会拒空字段列表）", async () => {
    const user = userEvent.setup();
    const client = stubClient();
    await driveToFields(user, client);

    expect(screen.getByRole("button", { name: "下一步" })).toBeDisabled();
    await user.click(await screen.findByLabelText("费用类型/Fee Type*"));
    expect(screen.getByRole("button", { name: "下一步" })).toBeEnabled();
  });

  it("源标识不合法或重复时明说，且不让提交", async () => {
    const user = userEvent.setup();
    const client = stubClient();
    await driveToFields(user, client);

    await user.click(await screen.findByLabelText("费用大类/Main Exp Cat*"));
    await user.click(await screen.findByLabelText("费用类型/Fee Type*"));
    await user.click(screen.getByRole("button", { name: "下一步" }));

    const inputs = await screen.findAllByLabelText("源标识");
    await user.clear(inputs[1]);
    await user.type(inputs[1], "Bad-Key");
    expect(screen.getByRole("alert")).toHaveTextContent(/小写字母开头/);
    expect(screen.getByRole("button", { name: "创建数据源" })).toBeDisabled();

    // 形状修好但撞上另一行的标识：同样拦住（那是全局唯一索引，创建时必炸）
    await user.clear(inputs[1]);
    await user.type(inputs[1], "fldtyg5vbz");
    expect(screen.getByRole("alert")).toHaveTextContent(/重复/);
    expect(screen.getByRole("button", { name: "创建数据源" })).toBeDisabled();
  });
});

describe("配置向导 · 数据源名称", () => {
  it("名称是必填的，空名称不进第二步", async () => {
    const user = userEvent.setup();
    const client = stubClient();
    render(
      <DatasourceTableWizard
        client={client}
        onCancel={vi.fn()}
        onSubmitted={vi.fn()}
      />,
    );

    await user.type(screen.getByLabelText(/Base Token/), APP_TOKEN);
    await user.click(screen.getByRole("button", { name: "拉取数据表" }));
    await user.click(await screen.findByRole("combobox", { name: "数据表" }));
    await user.click(
      await screen.findByRole("option", { name: /公司往来付款台账/ }),
    );

    expect(screen.getByRole("button", { name: "下一步" })).toBeDisabled();
    await user.type(screen.getByLabelText(/名称/), "公司往来付款");
    expect(screen.getByRole("button", { name: "下一步" })).toBeEnabled();
  });
});

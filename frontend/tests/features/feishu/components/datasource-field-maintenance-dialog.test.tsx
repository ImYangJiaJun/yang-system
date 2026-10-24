/**
 * 外部数据源维护对话框。
 *
 * 这一份钉住五件事（每条都对应一种「界面说对了、请求发错了」的失效形态）：
 * - 缺失字段以**字段名**为主，ID/标识只作排查信息；
 * - 改绑请求体精确为 `{datasource_id, source_key, field_id, parent_field_id}`；
 * - 停用必须二次确认，文案写清 40301 的后果；
 * - 新增成功当场给出完整 URL 与**一次性** Token（各带复制）；
 * - 后端拒绝原文留在对话框中，不换成泛化文案。
 */

import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ActionDemoSchema, UiCatalog } from "@/engine";
import type {
  AddedDatasourceField,
  DatasourceMaintenanceClient,
  FeishuInvokeDeps,
  RepointFieldResult,
} from "@/features/feishu/api";
import { repointDatasourceField } from "@/features/feishu/api";
import { DatasourceFieldMaintenanceDialog } from "@/features/feishu/components/DatasourceFieldMaintenanceDialog";
import type {
  BitableField,
  DatasourceFieldBinding,
  DatasourceItem,
  HealthReport,
} from "@/features/feishu/types";
import { approvalOptionsUrl } from "@/features/feishu/types";

const SOURCE_KEY = "old_rate";
const DATASOURCE_ID = 7;

const REMOTE_FIELDS: BitableField[] = [
  { fieldId: "fldGONE", fieldName: "银行流水摘要", type: 1 },
  { fieldId: "fldB", fieldName: "汇率/Exchange Rate", type: 1 },
  { fieldId: "fldP", fieldName: "费用类型", type: 3 },
];

const REPORT: HealthReport = {
  ok: false,
  missingFields: [
    {
      fieldId: "fldGONE",
      fieldName: "银行流水摘要",
      sourceKey: SOURCE_KEY,
      affectedChildren: [],
    },
  ],
  duplicateNameFields: [],
  unsupportedTypeFields: [],
  multiValueParentFields: [],
  unboundFields: [
    {
      fieldId: "fldNEW",
      fieldName: "远端新增列",
      fieldType: 3,
      uiType: "SingleSelect",
    },
  ],
  viewMissing: false,
  tableMissing: false,
  unchecked: [],
};

const DATASOURCE: DatasourceItem = {
  id: DATASOURCE_ID,
  fields: [
    {
      fieldId: "fldGONE",
      fieldName: "银行流水摘要",
      sourceKey: SOURCE_KEY,
      parentFieldId: null,
      enabled: true,
      encryptEnabled: false,
      defaultLocale: "zh_cn",
    },
  ],
  title: "公司往来付款",
  status: "active",
  updatedAt: 1758000000,
  ingestMode: "pull",
  bitableBaseToken: "app1",
  bitableTableId: "tblA",
  bitableViewId: null,
  lastPullAt: null,
  lastSuccessAt: null,
  consecutiveFailures: 0,
  lastError: null,
  lastPullStatus: "partial",
};

const PARENT_BINDING: DatasourceFieldBinding = {
  fieldId: "fldP",
  fieldName: "费用类型",
  sourceKey: "fee_type",
  parentFieldId: null,
  enabled: true,
  encryptEnabled: false,
  defaultLocale: "zh_cn",
};

const DATASOURCE_WITH_PARENT: DatasourceItem = {
  ...DATASOURCE,
  fields: [DATASOURCE.fields[0], PARENT_BINDING],
};

const REPOINT_RESULT: RepointFieldResult = {
  sourceKey: SOURCE_KEY,
  fieldId: "fldB",
  fieldName: "汇率/Exchange Rate",
  updatedPortals: 0,
  affectedApprovalConfigs: [],
};

const ADDED: AddedDatasourceField = {
  sourceKey: "tblA_fldNEW",
  fieldId: "fldNEW",
  fieldName: "远端新增列",
  token: "one-time-token",
};

function makeClient(
  overrides: Partial<DatasourceMaintenanceClient> = {},
): DatasourceMaintenanceClient {
  return {
    listFields: vi.fn().mockResolvedValue(REMOTE_FIELDS),
    health: vi.fn().mockResolvedValue(REPORT),
    repoint: vi.fn().mockResolvedValue(REPOINT_RESULT),
    setEnabled: vi.fn().mockResolvedValue({
      sourceKey: SOURCE_KEY,
      enabled: false,
      changed: true,
    }),
    addField: vi.fn().mockResolvedValue(ADDED),
    ...overrides,
  };
}

function renderDialog(
  client: DatasourceMaintenanceClient,
  datasource: DatasourceItem = DATASOURCE,
) {
  const onChanged = vi.fn();
  render(
    <DatasourceFieldMaintenanceDialog
      open
      datasource={datasource}
      client={client}
      onChanged={onChanged}
      onClose={vi.fn()}
    />,
  );
  return onChanged;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("DatasourceFieldMaintenanceDialog · 当前绑定", () => {
  it("缺失字段以字段名为主，ID 与 source_key 只作排查信息", async () => {
    renderDialog(makeClient());

    // 字段名会出现多次（绑定行 + 新增字段的父字段下拉），这里只断言「以名字展示」。
    expect((await screen.findAllByText("银行流水摘要")).length).toBeGreaterThan(
      0,
    );
    expect(screen.getByText("远端已删除")).toBeInTheDocument();
    expect(screen.getByText("fldGONE")).toBeInTheDocument();
    expect(screen.getByText(SOURCE_KEY)).toBeInTheDocument();
  });

  it("改绑请求体精确为 datasource_id / source_key / field_id / parent_field_id", async () => {
    const user = userEvent.setup();
    const calls: Array<{ url: string; body: unknown }> = [];
    vi.stubGlobal(
      "fetch",
      vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
        calls.push({
          url: typeof input === "string" ? input : input.toString(),
          body: init?.body ? JSON.parse(String(init.body)) : undefined,
        });
        return Promise.resolve(
          new Response(
            JSON.stringify({
              code: 0,
              message: "改绑成功",
              data: {
                source_key: SOURCE_KEY,
                field_id: "fldB",
                field_name: "汇率/Exchange Rate",
                updated_portals: 0,
                affected_approval_configs: [],
              },
            }),
            { status: 200, headers: { "content-type": "application/json" } },
          ),
        );
      }),
    );

    const action: ActionDemoSchema = {
      operation_id: "feishu.datasource.repoint_datasource_field",
      title: "改绑字段",
      description: "",
      method: "POST",
      path: "/api/v1/feishu/datasources/field/repoint",
      params: [],
      input_schema: {},
      output_schema: {},
      request_media_type: "json",
      response_kind: "json",
      requires_auth: true,
    };
    const catalog: UiCatalog = {
      schema_version: "2.3",
      revision: "a".repeat(64),
      actions: [action],
      table_views: [],
      modules: [],
    };
    const deps: FeishuInvokeDeps = {
      catalog,
      session: { token: "tok-1" },
    };
    const client = makeClient({
      repoint: (input) => repointDatasourceField(input, deps),
    });
    renderDialog(client);

    await user.click(
      await screen.findByRole("button", { name: "改绑：银行流水摘要" }),
    );
    await user.selectOptions(screen.getByLabelText("改绑目标字段"), "fldB");
    await user.click(screen.getByRole("button", { name: "保存改绑" }));

    await waitFor(() => expect(calls).toHaveLength(1));
    expect(calls[0]?.url).toBe("/api/v1/feishu/datasources/field/repoint");
    expect(calls[0]?.body).toEqual({
      datasource_id: DATASOURCE_ID,
      source_key: SOURCE_KEY,
      field_id: "fldB",
      parent_field_id: null,
    });
  });

  it("改绑可以同时把父字段改指到另一条启用绑定", async () => {
    const user = userEvent.setup();
    const repoint = vi.fn().mockResolvedValue(REPOINT_RESULT);
    renderDialog(makeClient({ repoint }), DATASOURCE_WITH_PARENT);

    await user.click(
      await screen.findByRole("button", { name: "改绑：银行流水摘要" }),
    );
    await user.selectOptions(screen.getByLabelText("改绑目标字段"), "fldB");
    await user.selectOptions(screen.getByLabelText("改绑父字段"), "fldP");
    await user.click(screen.getByRole("button", { name: "保存改绑" }));

    await waitFor(() => expect(repoint).toHaveBeenCalled());
    expect(repoint.mock.calls[0]?.[0]).toMatchObject({
      datasourceId: DATASOURCE_ID,
      sourceKey: SOURCE_KEY,
      fieldId: "fldB",
      parentFieldId: "fldP",
    });
  });

  it("只改父级（不换列）也可以保存，解除父级发显式 null", async () => {
    const user = userEvent.setup();
    const repoint = vi.fn().mockResolvedValue(REPOINT_RESULT);
    const childDatasource: DatasourceItem = {
      ...DATASOURCE_WITH_PARENT,
      fields: [
        { ...DATASOURCE.fields[0], parentFieldId: "fldP" },
        PARENT_BINDING,
      ],
    };
    renderDialog(makeClient({ repoint }), childDatasource);

    await user.click(
      await screen.findByRole("button", { name: "改绑：银行流水摘要" }),
    );
    // 目标列保持当前值；只把父字段改成「无父」。
    await user.selectOptions(screen.getByLabelText("改绑父字段"), "__none__");
    await user.click(screen.getByRole("button", { name: "保存改绑" }));

    await waitFor(() => expect(repoint).toHaveBeenCalled());
    expect(repoint.mock.calls[0]?.[0]).toMatchObject({
      fieldId: "fldGONE",
      parentFieldId: null,
    });
  });

  it("后端拒绝原文留在对话框里（例如父字段还有子字段）", async () => {
    const user = userEvent.setup();
    const repoint = vi
      .fn()
      .mockRejectedValue(
        new Error(
          "字段「银行流水摘要」是这些子字段的父级：「费用类型」（fldB）。请先把子字段改指到其他父字段或解除父级，再改绑。",
        ),
      );
    renderDialog(makeClient({ repoint }));

    await user.click(
      await screen.findByRole("button", { name: "改绑：银行流水摘要" }),
    );
    await user.selectOptions(screen.getByLabelText("改绑目标字段"), "fldB");
    await user.click(screen.getByRole("button", { name: "保存改绑" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "是这些子字段的父级",
    );
  });
});

describe("DatasourceFieldMaintenanceDialog · 停用与新增", () => {
  it("停用要二次确认，文案写清 40301 的后果，确认后才发请求", async () => {
    const user = userEvent.setup();
    const setEnabled = vi.fn().mockResolvedValue({
      sourceKey: SOURCE_KEY,
      enabled: false,
      changed: true,
    });
    renderDialog(makeClient({ setEnabled }));

    await user.click(
      await screen.findByRole("button", { name: "停用：银行流水摘要" }),
    );
    // 确认块在请求之前出现，后果写在正文里
    expect(
      screen.getByText(/飞书控件会暂时取不到选项（40301），重新启用即可恢复/),
    ).toBeInTheDocument();
    expect(setEnabled).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "确认停用" }));
    await waitFor(() =>
      expect(setEnabled).toHaveBeenCalledWith({
        datasourceId: DATASOURCE_ID,
        sourceKey: SOURCE_KEY,
        enabled: false,
      }),
    );
  });

  it("新增成功：展示完整 URL 与一次性 Token，两者都能复制", async () => {
    const user = userEvent.setup();
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    const addField = vi.fn().mockResolvedValue(ADDED);
    renderDialog(makeClient({ addField }));

    await user.click(
      await screen.findByRole("button", { name: "添加：远端新增列" }),
    );

    expect(
      await screen.findByText("已新增字段「远端新增列」"),
    ).toBeInTheDocument();
    expect(addField).toHaveBeenCalledWith({
      datasourceId: DATASOURCE_ID,
      fieldId: "fldNEW",
    });

    const url = approvalOptionsUrl(window.location.origin, ADDED.sourceKey);
    expect(screen.getByText(url)).toBeInTheDocument();
    expect(screen.getByText("one-time-token")).toBeInTheDocument();
    expect(
      screen.getByText(/请把 URL 与 Token 粘到飞书审批后台的对应控件/),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "复制 Token" }));
    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith("one-time-token"),
    );
    await user.click(screen.getByRole("button", { name: "复制接口地址" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith(url));
  });

  it("新增字段可以同时指定父字段", async () => {
    const user = userEvent.setup();
    const addField = vi.fn().mockResolvedValue(ADDED);
    renderDialog(makeClient({ addField }), DATASOURCE_WITH_PARENT);

    await user.selectOptions(
      await screen.findByLabelText("新增父字段：远端新增列"),
      "fldP",
    );
    await user.click(screen.getByRole("button", { name: "添加：远端新增列" }));

    await waitFor(() =>
      expect(addField).toHaveBeenCalledWith({
        datasourceId: DATASOURCE_ID,
        fieldId: "fldNEW",
        parentFieldId: "fldP",
      }),
    );
  });

  it("新增被拒时原样展示后端原因", async () => {
    const user = userEvent.setup();
    const addField = vi
      .fn()
      .mockRejectedValue(new Error("远端字段 fldNEW 已绑定在另一条数据源上"));
    renderDialog(makeClient({ addField }));

    await user.click(
      await screen.findByRole("button", { name: "添加：远端新增列" }),
    );

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "已绑定在另一条数据源上",
    );
    expect(screen.queryByText(/已新增字段/)).toBeNull();
  });
});

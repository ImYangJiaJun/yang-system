import { effectScope, nextTick, ref, type EffectScope } from "vue";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  StepUpRequiredError,
  type InvocationResult,
  type SessionContext,
} from "src/api/client";
import type {
  ActionDemoSchema,
  ActionPresentationSchema,
  TableViewSchema,
} from "src/contracts/ui-catalog";
import { useColumnPreferences } from "./useColumnPreferences";
import { useRelationOptions } from "./useRelationOptions";
import { usePresentedActions, useTableActions } from "./useTableActions";
import { useTableQuery } from "./useTableQuery";
import { useTableSelection } from "./useTableSelection";

const scopes: EffectScope[] = [];

afterEach(() => {
  for (const scope of scopes.splice(0)) scope.stop();
});

describe("Step-up action orchestration", () => {
  it("只在 428 后获取 proof，并仅重试原操作一次", async () => {
    const protectedAction = action("admin.user.set_admin");
    const protectedPresentation = presentation("admin.user.set_admin", {
      placement: "toolbar",
      interaction: "invoke",
    });
    const result: InvocationResult = {
      kind: "json",
      status: 200,
      durationMs: 1,
      data: { ok: true },
    };
    const invoke = vi
      .fn()
      .mockRejectedValueOnce(
        new StepUpRequiredError("需要重新认证", {
          challenge: "signed-challenge",
          expiresIn: 120,
        }),
      )
      .mockResolvedValueOnce(result);
    const reauthenticate = vi.fn().mockResolvedValue("one-shot-proof");
    const reload = vi.fn().mockResolvedValue(undefined);
    const actions = inScope(() =>
      usePresentedActions({
        presentations: () => [protectedPresentation],
        businessFields: () => [],
        actions: () => [protectedAction],
        session: () => ({ token: "access-token", tenantId: "7" }),
        selectedRows: () => [],
        reload,
        emitCustom: vi.fn(),
        invoke,
        confirm: async () => true,
        reauthenticate,
        notify: vi.fn(),
      }),
    );

    await actions.openAction(protectedPresentation);

    expect(reauthenticate).toHaveBeenCalledOnce();
    expect(reauthenticate).toHaveBeenCalledWith("signed-challenge", {
      token: "access-token",
      tenantId: "7",
    });
    expect(invoke).toHaveBeenCalledTimes(2);
    expect(invoke.mock.calls[0]?.[4]).toBeUndefined();
    expect(invoke.mock.calls[1]?.[4]).toEqual({
      stepUpProof: "one-shot-proof",
    });
    expect(reload).toHaveBeenCalledOnce();
    expect(JSON.stringify({ ...sessionStorage })).not.toContain(
      "one-shot-proof",
    );
  });

  it("取消重认证时不重试敏感操作", async () => {
    const protectedAction = action("org.user.del");
    const protectedPresentation = presentation("org.user.del", {
      placement: "row",
      interaction: "invoke",
    });
    const invoke = vi.fn().mockRejectedValue(
      new StepUpRequiredError("需要重新认证", {
        challenge: "signed-challenge",
        expiresIn: 120,
      }),
    );
    const actions = inScope(() =>
      usePresentedActions({
        presentations: () => [protectedPresentation],
        businessFields: () => [],
        actions: () => [protectedAction],
        session: () => ({ token: "access-token", tenantId: "7" }),
        selectedRows: () => [],
        reload: vi.fn(),
        emitCustom: vi.fn(),
        invoke,
        confirm: async () => true,
        reauthenticate: vi.fn().mockResolvedValue(undefined),
        notify: vi.fn(),
      }),
    );

    await actions.openAction(protectedPresentation, { id: 9 });
    expect(invoke).toHaveBeenCalledOnce();
  });
});

function inScope<T>(create: () => T): T {
  const scope = effectScope();
  scopes.push(scope);
  return scope.run(create)!;
}

function action(operationId: string): ActionDemoSchema {
  return {
    operation_id: operationId,
    title: operationId,
    description: "",
    method: "POST",
    path: `/api/v1/${operationId}`,
    params: [],
    input_schema: { type: "object", properties: {} },
    output_schema: {},
    request_media_type: "json",
    response_kind: "json",
    requires_auth: true,
  };
}

function presentation(
  operationId: string,
  overrides: Partial<ActionPresentationSchema> = {},
): ActionPresentationSchema {
  return {
    operation_id: operationId,
    title: operationId,
    placement: "toolbar",
    interaction: "invoke",
    ...overrides,
  };
}

function view(overrides: Partial<TableViewSchema> = {}): TableViewSchema {
  return {
    view_id: "items",
    title: "项目",
    table: "items",
    data_action: "items.list",
    columns: [
      {
        field: "id",
        title: "ID",
        description: "",
        widget: "integer",
        required: true,
        searchable: false,
        filterable: true,
        sortable: true,
        filter: { operators: ["eq"], default_operator: "eq" },
      },
      {
        field: "name",
        title: "名称",
        description: "",
        widget: "text",
        required: true,
        searchable: true,
        filterable: true,
        sortable: true,
        filter: {
          operators: ["contains"],
          default_operator: "contains",
        },
      },
    ],
    form: { fields: [] },
    query: {
      search_fields: ["name"],
      filter_fields: ["id", "name"],
      default_sort: [{ field: "id", direction: "asc" }],
      default_page_size: 20,
      max_page_size: 100,
    },
    actions: [],
    action_presentations: [],
    ...overrides,
  };
}

function tableResult(id: number): InvocationResult {
  return {
    kind: "json",
    status: 200,
    durationMs: 1,
    data: {
      items: [{ id, name: `项目 ${id}` }],
      page: 1,
      page_size: 20,
      total: 1,
    },
  };
}

describe("useTableQuery", () => {
  it("取消旧查询并阻止迟到响应覆盖新上下文", async () => {
    const session = ref<SessionContext>({});
    const dataAction = action("items.list");
    const pending: Array<(value: InvocationResult) => void> = [];
    const invoke = vi.fn(
      (actionInput: ActionDemoSchema, values: Record<string, unknown>) => {
        void actionInput;
        void values;
        return new Promise<InvocationResult>((resolve) =>
          pending.push(resolve),
        );
      },
    );
    const query = inScope(() =>
      useTableQuery({
        view: () => view(),
        dataAction: () => dataAction,
        session,
        invoke,
      }),
    );
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke.mock.calls[0]?.[1]).toMatchObject({
      order_by: [{ field: "id", direction: "Asc" }],
    });

    session.value = { token: "new-token" };
    await nextTick();
    expect(invoke).toHaveBeenCalledTimes(2);

    pending[0]?.(tableResult(1));
    await nextTick();
    expect(query.loading.value).toBe(true);
    expect(query.rows.value).toEqual([]);

    pending[1]?.(tableResult(2));
    await vi.waitFor(() => expect(query.loading.value).toBe(false));
    expect(query.rows.value[0]?.id).toBe(2);
  });
});

describe("useRelationOptions", () => {
  it("关系选项只接受最新请求结果", async () => {
    const relationView = view({
      columns: [
        {
          ...view().columns[0]!,
          field: "owner_id",
          relation: {
            operation_id: "users.options",
            value_field: "id",
            label_fields: ["name"],
          },
        },
      ],
    });
    const pending: Array<(value: InvocationResult) => void> = [];
    const invoke = vi.fn(
      () => new Promise<InvocationResult>((resolve) => pending.push(resolve)),
    );
    const relations = inScope(() =>
      useRelationOptions({
        view: () => relationView,
        actions: () => [action("users.options")],
        session: () => ({}),
        invoke,
      }),
    );

    const first = relations.load([{ owner_id: 1 }]);
    const second = relations.load([{ owner_id: 2 }]);
    pending[0]?.({
      kind: "json",
      status: 200,
      durationMs: 1,
      data: {
        items: [{ value: 1, label: "旧用户" }],
        page: 1,
        limit: 20,
        total: 1,
      },
    });
    await first;
    expect(relations.relationOptions.value).toEqual({});

    pending[1]?.({
      kind: "json",
      status: 200,
      durationMs: 1,
      data: {
        items: [{ value: 2, label: "新用户" }],
        page: 1,
        limit: 20,
        total: 1,
      },
    });
    await second;
    expect(relations.labelFor("users.options", 2)).toBe("新用户");
  });
});

describe("useTableSelection", () => {
  it("翻页或刷新替换可用行时清理旧选择", async () => {
    const rows = ref([{ key: "root.0", depth: 0, data: { id: 1 } }]);
    const selection = inScope(() => useTableSelection(rows));
    selection.selectedDisplayRows.value = [rows.value[0]!];

    rows.value = [{ key: "root.0", depth: 0, data: { id: 2 } }];
    await nextTick();

    expect(selection.selectedRows.value).toEqual([]);
  });
});

describe("useColumnPreferences", () => {
  it("至少保留一列，并在 View 变化时恢复契约默认列", async () => {
    const currentView = ref(view());
    const preferences = inScope(() =>
      useColumnPreferences(currentView, () => false),
    );

    preferences.setColumnVisible("name", false);
    preferences.setColumnVisible("id", false);
    expect(preferences.visibleColumnNames.value).toEqual(["id"]);

    currentView.value = view({
      view_id: "next",
      columns: [{ ...view().columns[1]!, field: "title" }],
    });
    await nextTick();
    expect(preferences.visibleColumnNames.value).toEqual(["title"]);
  });
});

describe("useTableActions", () => {
  it("Action 成功后关闭弹窗并按统一策略刷新表格", async () => {
    const reload = vi.fn().mockResolvedValue(undefined);
    const notify = vi.fn();
    const invoke = vi.fn().mockResolvedValue({
      kind: "json",
      status: 200,
      durationMs: 1,
      data: {},
      message: "保存成功",
    } satisfies InvocationResult);
    const itemAction = action("items.create");
    const itemPresentation = presentation("items.create");
    const actions = inScope(() =>
      useTableActions({
        view: () =>
          view({
            actions: ["items.create"],
            action_presentations: [itemPresentation],
          }),
        actions: () => [itemAction],
        session: () => ({ token: "access-token" }),
        selectedRows: () => [],
        reload,
        emitCustom: vi.fn(),
        invoke,
        confirm: async () => true,
        notify,
        handleAttachment: vi.fn(),
        redirect: vi.fn(),
      }),
    );

    await actions.openAction(itemPresentation);

    expect(invoke).toHaveBeenCalledOnce();
    expect(reload).toHaveBeenCalledOnce();
    expect(notify).toHaveBeenCalledWith("positive", "保存成功");
    expect(actions.actionDialog.value).toBe(false);
    expect(actions.actionLoading.value).toBe(false);
  });

  it("统一执行器覆盖正式页 placement 与 interaction 矩阵", async () => {
    const placements = ["toolbar", "row", "bulk"] as const;
    const interactions = [
      "form",
      "invoke",
      "download",
      "preview",
      "navigate",
      "custom",
    ] as const;

    for (const placement of placements) {
      for (const interaction of interactions) {
        const operationId = `matrix.${placement}.${interaction}`;
        const itemAction = {
          ...action(operationId),
          response_kind:
            interaction === "download" ||
            interaction === "preview" ||
            interaction === "navigate"
              ? interaction === "navigate"
                ? "redirect"
                : interaction
              : "json",
          input_schema: {
            type: "object",
            properties: { record_id: { type: "integer" } },
          },
        } satisfies ActionDemoSchema;
        const itemPresentation = presentation(operationId, {
          placement,
          interaction,
          record_parameter: "record_id",
          ...(interaction === "custom"
            ? { view_id: "demo.items.insight" }
            : {}),
        });
        const reload = vi.fn().mockResolvedValue(undefined);
        const emitCustom = vi.fn();
        const handleAttachment = vi.fn();
        const redirect = vi.fn();
        const result = {
          kind:
            itemAction.response_kind === "redirect"
              ? "redirect"
              : itemAction.response_kind,
          status: 200,
          durationMs: 1,
          ...(itemAction.response_kind === "download" ||
          itemAction.response_kind === "preview"
            ? { blobUrl: "blob:matrix" }
            : {}),
          ...(itemAction.response_kind === "redirect"
            ? { location: "/matrix-target" }
            : { data: {} }),
        } satisfies InvocationResult;
        const invoke = vi.fn().mockResolvedValue(result);
        const actions = inScope(() =>
          usePresentedActions({
            presentations: () => [itemPresentation],
            businessFields: () => [],
            actions: () => [itemAction],
            session: () => ({ token: "access-token" }),
            selectedRows: () => [{ id: 7, name: "已选择" }],
            reload,
            emitCustom,
            invoke,
            confirm: async () => true,
            notify: vi.fn(),
            handleAttachment,
            redirect,
          }),
        );
        const row = { id: 9, name: "当前行" };

        await actions.openAction(
          itemPresentation,
          placement === "row" ? row : undefined,
        );
        if (interaction === "custom") {
          expect(emitCustom).toHaveBeenCalledWith(
            itemPresentation,
            placement === "row" ? row : undefined,
          );
          expect(invoke).not.toHaveBeenCalled();
          continue;
        }
        if (interaction === "form") {
          expect(actions.actionDialog.value).toBe(true);
          await actions.submitAction();
        }

        expect(invoke).toHaveBeenCalledOnce();
        const values = invoke.mock.calls[0]?.[1] as Record<string, unknown>;
        if (placement === "row") expect(values.record_id).toBe(9);
        if (placement === "bulk") {
          expect(values.selected).toEqual([{ id: 7, name: "已选择" }]);
        }
        expect(reload).toHaveBeenCalledOnce();
        expect(handleAttachment).toHaveBeenCalledWith(result);
        if (interaction === "navigate") {
          expect(redirect).toHaveBeenCalledWith("/matrix-target");
        } else {
          expect(redirect).not.toHaveBeenCalled();
        }
      }
    }
  });
});

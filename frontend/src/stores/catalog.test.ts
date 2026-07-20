import { createPinia, setActivePinia } from "pinia";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionContext } from "src/api/client";
import type { UiCatalog } from "src/contracts/ui-catalog";

const { fetchUiCatalogMock, invokeActionMock } = vi.hoisted(() => ({
  fetchUiCatalogMock: vi.fn(),
  invokeActionMock: vi.fn(),
}));

vi.mock("src/api/client", async (importOriginal) => ({
  ...(await importOriginal<typeof import("src/api/client")>()),
  fetchUiCatalog: fetchUiCatalogMock,
  invokeAction: invokeActionMock,
}));

import { useCatalogStore } from "./catalog";

function catalog(revision: string, operationId: string): UiCatalog {
  return {
    schema_version: "2.2",
    revision,
    actions: [
      {
        operation_id: operationId,
        title: operationId,
        description: "",
        method: "GET",
        path: `/api/v1/${operationId}`,
        params: [],
        input_schema: {},
        output_schema: {},
        request_media_type: "json",
        response_kind: "json",
        requires_auth: false,
      },
    ],
    table_views: [],
  };
}

describe("catalog store", () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    sessionStorage.clear();
    fetchUiCatalogMock.mockReset();
    invokeActionMock.mockReset();
  });

  it("忽略已过期请求，旧请求结束不会清除新请求的 loading", async () => {
    const pending: Array<{
      context: SessionContext;
      resolve: (value: UiCatalog) => void;
    }> = [];
    fetchUiCatalogMock.mockImplementation(
      (context: SessionContext) =>
        new Promise<UiCatalog>((resolve) => pending.push({ context, resolve })),
    );
    const store = useCatalogStore();

    const firstLoad = store.loadCatalog();
    store.token = "new-token";
    const secondLoad = store.loadCatalog();
    expect(pending.map((request) => request.context.token)).toEqual([
      undefined,
      "new-token",
    ]);

    pending[0]?.resolve(catalog("a".repeat(64), "stale"));
    await firstLoad;
    expect(store.loading).toBe(true);
    expect(store.catalog).toBeUndefined();

    pending[1]?.resolve(catalog("b".repeat(64), "current"));
    await secondLoad;
    expect(store.loading).toBe(false);
    expect(store.catalog?.actions[0]?.operation_id).toBe("current");
  });

  it("登录和退出立即同步当前会话", () => {
    const store = useCatalogStore();

    store.setAccessToken("access-token");
    expect(store.token).toBe("access-token");
    expect(sessionStorage.getItem("yang.token")).toBe("access-token");

    store.tenantId = "tenant-7";
    store.clearSession();
    expect(store.token).toBe("");
    expect(store.tenantId).toBe("");
    expect(sessionStorage.getItem("yang.token")).toBeNull();
    expect(sessionStorage.getItem("yang.tenant-id")).toBeNull();
  });

  it("通过我的企业 Action 加载名称选项并隐藏内部租户输入", async () => {
    const store = useCatalogStore();
    store.token = "access-token";
    store.catalog = catalog("c".repeat(64), "org.access.list");
    invokeActionMock.mockResolvedValue({
      kind: "json",
      status: 200,
      durationMs: 1,
      data: {
        items: [{ id: 7, name: "示例企业", code: "ACME" }],
        total: 1,
        page: 1,
        limit: 100,
        total_pages: 1,
      },
    });

    await store.loadOrganizations();

    expect(store.organizations).toEqual([
      { id: 7, name: "示例企业", code: "ACME" },
    ]);
    expect(invokeActionMock.mock.calls[0]?.[2]).toEqual({
      token: "access-token",
    });
    store.selectOrganization(store.organizations[0]);
    expect(store.tenantId).toBe("7");
    expect(store.selectedOrganization?.name).toBe("示例企业");
  });
});

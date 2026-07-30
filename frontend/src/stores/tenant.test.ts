import { createPinia, setActivePinia } from "pinia";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ActionDemoSchema } from "src/contracts/ui-catalog";

const { invokeActionMock } = vi.hoisted(() => ({
  invokeActionMock: vi.fn(),
}));

vi.mock("src/api/client", async (importOriginal) => ({
  ...(await importOriginal<typeof import("src/api/client")>()),
  invokeAction: invokeActionMock,
}));

import { useTenantStore } from "./tenant";

const organizationAction: ActionDemoSchema = {
  operation_id: "org.tenant.list",
  title: "我的企业",
  description: "",
  method: "GET",
  path: "/api/v1/org/tenant",
  params: [],
  input_schema: {},
  output_schema: {},
  request_media_type: "json",
  response_kind: "json",
  requires_auth: true,
};

function page(id: number, name: string) {
  return {
    kind: "json" as const,
    status: 200,
    durationMs: 1,
    data: {
      items: [{ id, name, code: `ORG-${id}` }],
      total: 1,
      page: 1,
      limit: 100,
      total_pages: 1,
    },
  };
}

describe("tenant store", () => {
  beforeEach(() => {
    sessionStorage.clear();
    setActivePinia(createPinia());
    invokeActionMock.mockReset();
  });

  it("加载企业选项并显式持久化租户选择", async () => {
    invokeActionMock.mockResolvedValue(page(7, "示例企业"));
    const store = useTenantStore();

    await store.loadOrganizations([organizationAction], "access-token");

    expect(store.organizations).toEqual([
      { id: 7, name: "示例企业", code: "ORG-7" },
    ]);
    expect(invokeActionMock.mock.calls[0]?.[2]).toEqual({
      token: "access-token",
    });
    store.selectOrganization(store.organizations[0]);
    expect(store.tenantId).toBe("7");
    expect(store.selectedOrganization?.name).toBe("示例企业");
    expect(sessionStorage.getItem("yang.tenant-id")).toBe("7");
  });

  it("忽略租户列表的迟到响应", async () => {
    const pending: Array<(value: ReturnType<typeof page>) => void> = [];
    invokeActionMock.mockImplementation(
      () =>
        new Promise<ReturnType<typeof page>>((resolve) =>
          pending.push(resolve),
        ),
    );
    const store = useTenantStore();

    const first = store.loadOrganizations([organizationAction], "old-token");
    const second = store.loadOrganizations([organizationAction], "new-token");
    pending[0]?.(page(1, "旧企业"));
    await first;
    expect(store.loading).toBe(true);
    expect(store.organizations).toEqual([]);

    pending[1]?.(page(2, "新企业"));
    await second;
    expect(store.loading).toBe(false);
    expect(store.organizations[0]?.name).toBe("新企业");
  });
});

import { createPinia, setActivePinia } from "pinia";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionContext } from "src/api/client";
import type { UiCatalog } from "src/contracts/ui-catalog";

const { fetchUiCatalogMock } = vi.hoisted(() => ({
  fetchUiCatalogMock: vi.fn(),
}));

vi.mock("src/api/client", async (importOriginal) => ({
  ...(await importOriginal<typeof import("src/api/client")>()),
  fetchUiCatalog: fetchUiCatalogMock,
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
    modules: [],
  };
}

describe("catalog store", () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    fetchUiCatalogMock.mockReset();
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

    const firstLoad = store.loadCatalog({});
    const secondLoad = store.loadCatalog({ token: "new-token" });
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

  it("reset 会取消当前请求并清空 Catalog 状态", async () => {
    let signal: AbortSignal | undefined;
    fetchUiCatalogMock.mockImplementation(
      (_context: SessionContext, nextSignal: AbortSignal) => {
        signal = nextSignal;
        return new Promise<UiCatalog>(() => undefined);
      },
    );
    const store = useCatalogStore();

    void store.loadCatalog({});
    store.reset();

    expect(signal?.aborted).toBe(true);
    expect(store.catalog).toBeUndefined();
    expect(store.loading).toBe(false);
    expect(store.error).toBeUndefined();
  });
});

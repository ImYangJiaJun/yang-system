import { createPinia, setActivePinia } from "pinia";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { UiCatalog } from "src/contracts/ui-catalog";

const { fetchUiCatalogMock } = vi.hoisted(() => ({
  fetchUiCatalogMock: vi.fn(),
}));

vi.mock("src/api/client", async (importOriginal) => ({
  ...(await importOriginal<typeof import("src/api/client")>()),
  fetchUiCatalog: fetchUiCatalogMock,
}));

import { useApplicationLifecycleStore } from "./application-lifecycle";
import { useSessionStore } from "./session";

const emptyCatalog: UiCatalog = {
  schema_version: "2.2",
  revision: "b".repeat(64),
  actions: [],
  table_views: [],
  modules: [],
};

describe("application lifecycle", () => {
  beforeEach(() => {
    sessionStorage.clear();
    setActivePinia(createPinia());
    fetchUiCatalogMock.mockReset();
    fetchUiCatalogMock.mockResolvedValue(emptyCatalog);
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("start 幂等，应用上下文变化只保留一组重载 watcher", async () => {
    const lifecycle = useApplicationLifecycleStore();
    const session = useSessionStore();

    const firstDispose = lifecycle.start();
    const secondDispose = lifecycle.start();
    await vi.runAllTicks();
    expect(firstDispose).toBe(secondDispose);
    expect(fetchUiCatalogMock).toHaveBeenCalledTimes(1);

    session.token = "access-1";
    session.token = "access-2";
    await vi.advanceTimersByTimeAsync(400);

    expect(fetchUiCatalogMock).toHaveBeenCalledTimes(2);
    expect(fetchUiCatalogMock.mock.calls[1]?.[0]).toEqual({
      token: "access-2",
      tenantId: undefined,
    });
  });

  it("dispose 后不再响应上下文变化", async () => {
    const lifecycle = useApplicationLifecycleStore();
    const session = useSessionStore();
    const dispose = lifecycle.start();
    await vi.runAllTicks();

    dispose();
    session.token = "after-dispose";
    await vi.advanceTimersByTimeAsync(500);

    expect(fetchUiCatalogMock).toHaveBeenCalledTimes(1);
  });
});

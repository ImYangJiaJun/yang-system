import { ref } from "vue";
import { createPinia, setActivePinia } from "pinia";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  SESSION_EXPIRED_EVENT,
  SESSION_REFRESHED_EVENT,
} from "src/api/auth-session";
import { SESSION_SIGNAL_STORAGE_KEY } from "src/api/session-coordination";
import type { UiCatalog } from "src/contracts/ui-catalog";

const { fetchUiCatalogMock } = vi.hoisted(() => ({
  fetchUiCatalogMock: vi.fn(),
}));

vi.mock("src/api/client", async (importOriginal) => ({
  ...(await importOriginal<typeof import("src/api/client")>()),
  fetchUiCatalog: fetchUiCatalogMock,
}));

import { startApplication, type ApplicationRouter } from "./startApplication";
import { useSessionStore } from "src/stores/session";

const emptyCatalog: UiCatalog = {
  schema_version: "2.2",
  revision: "c".repeat(64),
  actions: [],
  table_views: [],
  modules: [],
};
const disposers: Array<() => void> = [];

function router(name = "business") {
  return {
    currentRoute: ref({ name }),
    replace: vi.fn().mockResolvedValue(undefined),
  } as unknown as ApplicationRouter;
}

describe("application startup", () => {
  beforeEach(() => {
    sessionStorage.clear();
    setActivePinia(createPinia());
    fetchUiCatalogMock.mockReset();
    fetchUiCatalogMock.mockResolvedValue(emptyCatalog);
  });

  afterEach(() => {
    for (const dispose of disposers.splice(0)) dispose();
  });

  it("在唯一生命周期内同步 refresh 事件，并在 dispose 后移除 listener", () => {
    const session = useSessionStore();
    const dispose = startApplication(router());
    disposers.push(dispose);

    window.dispatchEvent(
      new CustomEvent(SESSION_REFRESHED_EVENT, {
        detail: {
          accessToken: "access-1",
        },
      }),
    );
    expect(session.token).toBe("access-1");

    dispose();
    disposers.pop();
    window.dispatchEvent(
      new CustomEvent(SESSION_REFRESHED_EVENT, {
        detail: {
          accessToken: "access-2",
        },
      }),
    );
    expect(session.token).toBe("access-1");
  });

  it("会话失效时级联清空状态并导航到登录页", () => {
    const session = useSessionStore();
    session.setTokenPair({
      accessToken: "access-token",
    });
    const applicationRouter = router();
    disposers.push(startApplication(applicationRouter));

    window.dispatchEvent(new CustomEvent(SESSION_EXPIRED_EVENT));

    expect(session.token).toBe("");
    expect(applicationRouter.replace).toHaveBeenCalledWith({
      name: "login",
      query: { reason: "session-expired" },
    });
  });

  it("其他标签页退出时只接受版本化信号并级联清空当前内存会话", () => {
    const session = useSessionStore();
    session.setTokenPair({
      accessToken: "access-token",
    });
    const applicationRouter = router();
    disposers.push(startApplication(applicationRouter));

    window.dispatchEvent(
      new StorageEvent("storage", {
        key: SESSION_SIGNAL_STORAGE_KEY,
        newValue: JSON.stringify({
          version: 1,
          id: "remote-logout-1",
          sender: "other-tab",
          type: "session-ended",
          reason: "logout",
        }),
      }),
    );

    expect(session.token).toBe("");
    expect(applicationRouter.replace).toHaveBeenCalledWith({
      name: "login",
      query: { reason: "session-expired" },
    });
  });
});

import { createPinia, setActivePinia } from "pinia";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { clearStoredSession } from "src/api/auth-session";
import { useApplicationSession } from "src/composables/useApplicationSession";
import type { UiCatalog } from "src/contracts/ui-catalog";
import { useCatalogStore } from "./catalog";
import { useCatalogNavigationStore } from "./catalog-navigation";
import { useIdentityStore } from "./identity";
import { useSessionStore } from "./session";

const emptyCatalog: UiCatalog = {
  schema_version: "2.2",
  revision: "a".repeat(64),
  actions: [],
  table_views: [],
  modules: [],
};

describe("application session", () => {
  beforeEach(() => {
    clearStoredSession();
    setActivePinia(createPinia());
    vi.unstubAllGlobals();
  });

  it("登录通过显式协调动作清空旧身份、Catalog 和导航", () => {
    const session = useSessionStore();
    const identity = useIdentityStore();
    const catalog = useCatalogStore();
    const navigation = useCatalogNavigationStore();
    const applicationSession = useApplicationSession();
    identity.select("user");
    catalog.catalog = emptyCatalog;
    navigation.query = "old";

    applicationSession.beginSession({
      accessToken: "access-token",
    });

    expect(session.token).toBe("access-token");
    expect(identity.accountIdentity).toBeUndefined();
    expect(catalog.catalog).toBeUndefined();
    expect(navigation.query).toBe("");
    expect(sessionStorage.getItem("yang.token")).toBeNull();
    expect(sessionStorage.getItem("yang.refresh-token")).toBeNull();
    expect(sessionStorage.getItem("yang.account-identity")).toBeNull();
  });

  it("忽略 Web Storage 中注入的旧 Access Token", () => {
    sessionStorage.setItem("yang.token", "attacker-controlled-token");
    setActivePinia(createPinia());

    const session = useSessionStore();

    expect(session.token).toBe("");
    expect(session.loggedIn).toBe(false);
  });

  it("自动刷新 Token 时保留当前身份上下文", () => {
    const session = useSessionStore();
    const identity = useIdentityStore();
    const applicationSession = useApplicationSession();
    identity.select("user");

    applicationSession.acceptRefreshedTokenPair({
      accessToken: "access-new",
    });

    expect(session.token).toBe("access-new");
    expect(identity.accountIdentity).toBe("user");
  });

  it("页面重载只通过 HttpOnly Refresh Cookie 恢复内存会话", async () => {
    const fetchMock = vi.fn(async (_url: string, init: RequestInit) => {
      expect(init.credentials).toBe("include");
      return new Response(
        JSON.stringify({
          code: 0,
          message: "成功",
          data: { access_token: "restored-access" },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    });
    vi.stubGlobal("fetch", fetchMock);
    const session = useSessionStore();

    await expect(session.restoreFromCookie()).resolves.toBe(true);

    expect(session.token).toBe("restored-access");
    expect(session.loggedIn).toBe(true);
    expect(sessionStorage.getItem("yang.token")).toBeNull();
    expect(fetchMock).toHaveBeenCalledOnce();
  });

  it("伪造旧 Token 且 Refresh Cookie 无效时保持未认证并清除上下文", async () => {
    sessionStorage.setItem("yang.token", "forged-access");
    sessionStorage.setItem("yang.account-identity", "user");
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({ code: 40102, message: "刷新会话 Cookie 缺失" }),
            {
              status: 401,
              headers: { "content-type": "application/json" },
            },
          ),
      ),
    );
    setActivePinia(createPinia());
    const session = useSessionStore();

    await expect(session.restoreFromCookie()).resolves.toBe(false);

    expect(session.loggedIn).toBe(false);
    expect(sessionStorage.getItem("yang.token")).toBeNull();
    expect(sessionStorage.getItem("yang.account-identity")).toBeNull();
  });

  it("退出确定性级联清空所有会话相关 owner", () => {
    const session = useSessionStore();
    const identity = useIdentityStore();
    const catalog = useCatalogStore();
    const applicationSession = useApplicationSession();
    session.setTokenPair({
      accessToken: "access-token",
    });
    identity.select("user");
    catalog.catalog = emptyCatalog;

    applicationSession.clearSession();

    expect(session.token).toBe("");
    expect(identity.accountIdentity).toBeUndefined();
    expect(catalog.catalog).toBeUndefined();
    expect(sessionStorage.length).toBe(0);
  });
});

import { createPinia, setActivePinia } from "pinia";
import { beforeEach, describe, expect, it } from "vitest";
import { useApplicationSession } from "src/composables/useApplicationSession";
import type { UiCatalog } from "src/contracts/ui-catalog";
import { useCatalogStore } from "./catalog";
import { useCatalogNavigationStore } from "./catalog-navigation";
import { useIdentityStore } from "./identity";
import { useSessionStore } from "./session";
import { useTenantStore } from "./tenant";

const emptyCatalog: UiCatalog = {
  schema_version: "2.2",
  revision: "a".repeat(64),
  actions: [],
  table_views: [],
  modules: [],
};

describe("application session", () => {
  beforeEach(() => {
    sessionStorage.clear();
    setActivePinia(createPinia());
  });

  it("登录通过显式协调动作清空旧身份、租户、Catalog 和导航", () => {
    const session = useSessionStore();
    const identity = useIdentityStore();
    const tenant = useTenantStore();
    const catalog = useCatalogStore();
    const navigation = useCatalogNavigationStore();
    const applicationSession = useApplicationSession();
    identity.select("admin");
    tenant.setTenantId("tenant-7");
    catalog.catalog = emptyCatalog;
    navigation.query = "old";

    applicationSession.beginSession({
      accessToken: "access-token",
    });

    expect(session.token).toBe("access-token");
    expect(identity.accountIdentity).toBeUndefined();
    expect(tenant.tenantId).toBe("");
    expect(catalog.catalog).toBeUndefined();
    expect(navigation.query).toBe("");
    expect(sessionStorage.getItem("yang.token")).toBe("access-token");
    expect(sessionStorage.getItem("yang.refresh-token")).toBeNull();
    expect(sessionStorage.getItem("yang.account-identity")).toBeNull();
    expect(sessionStorage.getItem("yang.tenant-id")).toBeNull();
  });

  it("自动刷新 Token 时保留当前身份和租户上下文", () => {
    const session = useSessionStore();
    const identity = useIdentityStore();
    const tenant = useTenantStore();
    const applicationSession = useApplicationSession();
    identity.select("org");
    tenant.setTenantId("tenant-7");

    applicationSession.acceptRefreshedTokenPair({
      accessToken: "access-new",
    });

    expect(session.token).toBe("access-new");
    expect(identity.accountIdentity).toBe("org");
    expect(tenant.tenantId).toBe("tenant-7");
  });

  it("退出确定性级联清空所有会话相关 owner", () => {
    const session = useSessionStore();
    const identity = useIdentityStore();
    const tenant = useTenantStore();
    const catalog = useCatalogStore();
    const applicationSession = useApplicationSession();
    session.setTokenPair({
      accessToken: "access-token",
    });
    identity.select("admin");
    tenant.setTenantId("tenant-7");
    catalog.catalog = emptyCatalog;

    applicationSession.clearSession();

    expect(session.token).toBe("");
    expect(identity.accountIdentity).toBeUndefined();
    expect(tenant.tenantId).toBe("");
    expect(catalog.catalog).toBeUndefined();
    expect(sessionStorage.length).toBe(0);
  });
});

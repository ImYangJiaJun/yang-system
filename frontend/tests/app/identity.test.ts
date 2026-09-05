import { beforeEach, describe, expect, it } from "vitest";

import {
  clearStoredIdentity,
  loadStoredIdentity,
  resolveIdentityLanding,
  storeIdentity,
} from "@/app/identity";

beforeEach(() => sessionStorage.clear());

describe("身份存储", () => {
  it("选择/清空与旧实现同 key（yang.account-identity）", () => {
    expect(loadStoredIdentity()).toBeUndefined();
    storeIdentity("admin");
    expect(sessionStorage.getItem("yang.account-identity")).toBe("admin");
    expect(loadStoredIdentity()).toBe("admin");
    clearStoredIdentity();
    expect(loadStoredIdentity()).toBeUndefined();
  });

  it("空白值视为未选择", () => {
    sessionStorage.setItem("yang.account-identity", "  ");
    expect(loadStoredIdentity()).toBeUndefined();
  });
});

describe("身份落点", () => {
  const two = [{ id: "user" }, { id: "admin" }];

  it("已存身份仍可见时直接进入", () => {
    expect(resolveIdentityLanding(two, "admin")).toEqual({
      kind: "direct",
      identity: "admin",
    });
  });

  it("单身份直接进入，多身份进选择页，零身份空态", () => {
    expect(resolveIdentityLanding([{ id: "user" }], undefined)).toEqual({
      kind: "direct",
      identity: "user",
    });
    expect(resolveIdentityLanding(two, undefined).kind).toBe("select");
    expect(resolveIdentityLanding([], undefined).kind).toBe("none");
  });

  it("已存身份在 Catalog 中不可见时按未存储处理", () => {
    expect(resolveIdentityLanding(two, "ghost").kind).toBe("select");
    expect(resolveIdentityLanding([{ id: "user" }], "ghost")).toEqual({
      kind: "direct",
      identity: "user",
    });
  });
});

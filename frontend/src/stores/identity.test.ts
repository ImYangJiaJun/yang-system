import { createPinia, setActivePinia } from "pinia";
import { beforeEach, describe, expect, it } from "vitest";
import { useIdentityStore } from "./identity";

const IDENTITY_KEY = "yang.account-identity";

describe("identity store", () => {
  beforeEach(() => {
    sessionStorage.clear();
    setActivePinia(createPinia());
  });

  it("无持久化身份时初始为空", () => {
    const identity = useIdentityStore();
    expect(identity.accountIdentity).toBeUndefined();
  });

  it("恢复持久化的 user 身份", () => {
    sessionStorage.setItem(IDENTITY_KEY, "user");
    const identity = useIdentityStore();
    expect(identity.accountIdentity).toBe("user");
  });

  it("恢复持久化的任意业务身份（不再硬编码 user）", () => {
    sessionStorage.setItem(IDENTITY_KEY, "merchant");
    const identity = useIdentityStore();
    expect(identity.accountIdentity).toBe("merchant");
  });

  it("忽略空白持久化值", () => {
    sessionStorage.setItem(IDENTITY_KEY, "   ");
    const identity = useIdentityStore();
    expect(identity.accountIdentity).toBeUndefined();
  });

  it("select 持久化身份，clear 清除持久化", () => {
    const identity = useIdentityStore();
    identity.select("admin");
    expect(identity.accountIdentity).toBe("admin");
    expect(sessionStorage.getItem(IDENTITY_KEY)).toBe("admin");

    identity.clear();
    expect(identity.accountIdentity).toBeUndefined();
    expect(sessionStorage.getItem(IDENTITY_KEY)).toBeNull();
  });
});

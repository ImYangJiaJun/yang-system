import { describe, expect, it } from "vitest";
import { resolveAccessRedirect } from "./access-policy";

describe("frontend access policy", () => {
  it("未登录访问正式界面时回到登录页", () => {
    expect(
      resolveAccessRedirect("protected", {
        authenticated: false,
        accountIdentity: undefined,
      }),
    ).toBe("/login");
  });

  it("登录后未选择角色时先进入角色选择页", () => {
    expect(
      resolveAccessRedirect("protected", {
        authenticated: true,
        accountIdentity: undefined,
      }),
    ).toBe("/roles");
    expect(
      resolveAccessRedirect("login", {
        authenticated: true,
        accountIdentity: undefined,
      }),
    ).toBe("/roles");
  });

  it("已选择角色后只允许进入该角色的模块", () => {
    expect(
      resolveAccessRedirect(
        "protected",
        { authenticated: true, accountIdentity: "user" },
        "admin",
      ),
    ).toBe("/roles");
    expect(
      resolveAccessRedirect(
        "protected",
        { authenticated: true, accountIdentity: "user" },
        "user",
      ),
    ).toBeUndefined();
  });
});

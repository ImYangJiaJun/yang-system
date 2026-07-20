import { expect, test } from "@playwright/test";

function catalogAction(operationId: string) {
  return {
    operation_id: operationId,
    title: operationId === "account.user.me" ? "当前用户" : "平台账号列表",
    description: "",
    method: "GET",
    path: `/api/v1/${operationId.replaceAll(".", "/")}`,
    params: [],
    input_schema: {},
    output_schema: {},
    request_media_type: "json",
    response_kind: "json",
    requires_auth: true,
  };
}

test("默认入口是登录界面", async ({ page }) => {
  await page.goto("/");

  await expect(page).toHaveURL("/login");
  await expect(page.getByRole("heading", { name: "用户登录" })).toBeVisible();
});

test("账号密码登录后先选择角色再进入对应模块", async ({ page }) => {
  await page.route("**/.well-known/yang/ui-catalog", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          schema_version: "2.2",
          revision: "a".repeat(64),
          actions: [
            catalogAction("account.user.me"),
            catalogAction("admin.user.list"),
          ],
          table_views: [],
        },
      }),
    }),
  );
  await page.route("**/api/v1/users/login", async (route) => {
    expect(route.request().postDataJSON()).toEqual({
      username: "alice",
      password: "correct-password",
    });
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          access_token: "access-token",
          refresh_token: "refresh-token",
        },
      }),
    });
  });

  await page.goto("/login");
  await page.getByLabel("帐号").fill("alice");
  await page.getByLabel("密码").fill("correct-password");
  await page.getByRole("button", { name: "登录" }).click();

  await expect(page).toHaveURL("/roles");
  await expect(
    page.getByRole("heading", { name: "选择本次使用的角色" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "选择管理平台角色" }).click();

  await expect(page).toHaveURL("/module/admin.user");
  await expect(page.getByRole("tab")).toHaveCount(0);
  await expect(page.getByTestId("module-nav-admin.user")).toBeVisible();
  await expect(page.getByTestId("module-nav-account.user")).toHaveCount(0);
  await expect
    .poll(() => page.evaluate(() => sessionStorage.getItem("yang.token")))
    .toBe("access-token");
  await expect
    .poll(() =>
      page.evaluate(() => sessionStorage.getItem("yang.account-identity")),
    )
    .toBe("admin");
});

test("登录失败时停留在登录页且不保存凭据", async ({ page }) => {
  await page.route("**/api/v1/users/login", (route) =>
    route.fulfill({
      status: 401,
      contentType: "application/json",
      body: JSON.stringify({ code: 40101, message: "账号或密码错误" }),
    }),
  );

  await page.goto("/login");
  await page.getByLabel("帐号").fill("alice");
  await page.getByLabel("密码").fill("wrong-password");
  await page.getByRole("button", { name: "登录" }).click();

  await expect(page).toHaveURL("/login");
  await expect(page.getByText("账号或密码错误")).toBeVisible();
  await expect
    .poll(() => page.evaluate(() => sessionStorage.getItem("yang.token")))
    .toBeNull();
});

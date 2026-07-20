import { expect, test } from "@playwright/test";

test("账号密码登录后保存会话并进入正式控制台", async ({ page }) => {
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

  await expect(page).toHaveURL("/");
  await page.getByRole("button", { name: "账号菜单" }).click();
  await expect(page.getByRole("button", { name: "退出帐号" })).toBeVisible();
  await expect
    .poll(() => page.evaluate(() => sessionStorage.getItem("yang.token")))
    .toBe("access-token");
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

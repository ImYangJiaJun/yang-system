import { expect, type Page } from "@playwright/test";

/**
 * E2E 共享支撑：演示后端无认证端点，会话经 route mock 注入。
 *
 * - mockSessionRestore：拦截 refresh 返回内存 token，页面重载后会话即可恢复
 *  （v2 的 Token 只在内存，page.goto 触发整页重载必须先打这个桩）。
 * - loginWithMockedCredentials：走真实登录 UI（login.spec 自证链路用）。
 */
export async function mockSessionRestore(page: Page) {
  await page.route("**/api/v1/users/refresh", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: { access_token: "e2e-access-token" },
      }),
      headers: {
        "Set-Cookie":
          "yang_refresh=e2e-refresh; Path=/api/v1/users; HttpOnly; SameSite=Strict",
      },
    }),
  );
}

export async function loginWithMockedCredentials(page: Page) {
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
        data: { access_token: "access-token" },
      }),
      headers: {
        "Set-Cookie":
          "yang_refresh=refresh-token; Path=/api/v1/users; HttpOnly; SameSite=Strict",
      },
    });
  });
  await page.goto("/login");
  await page.getByLabel("帐号").fill("alice");
  await page.getByLabel("密码", { exact: true }).fill("correct-password");
  await page.getByRole("button", { name: "登录" }).click();
  // 演示 Catalog 无 Module 与 identity，登录后直达应用中心。
  await expect(
    page.getByRole("heading", { name: "应用中心", level: 1 }),
  ).toBeVisible();
}

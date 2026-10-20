import { expect, type APIRequestContext, type Page } from "@playwright/test";

/**
 * E2E 共享支撑：演示后端无认证端点，会话经 route mock 注入。
 *
 * - mockSessionRestore：拦截 refresh 返回内存 token，页面重载后会话即可恢复
 *  （v2 的 Token 只在内存，page.goto 触发整页重载必须先打这个桩）。
 * - resetDemoData：演示后端的内存数据跨用例共享，开工前重置回种子，
 *  串行与 CI 重试都不再依赖前一个用例的增删副作用。
 * - loginWithMockedCredentials：走真实登录 UI（login.spec 自证链路用）。
 */
export async function resetDemoData(request: APIRequestContext) {
  // 相对 baseURL（前端 dev server）走 vite `/api` 代理到演示后端。
  const response = await request.post("/api/v1/demo/items/reset");
  expect(response.ok()).toBe(true);
}

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
  await page.getByRole("button", { name: "登录", exact: true }).click();
  // 演示 Catalog 无 Module 与 identity，登录后直达应用中心。
  await expect(
    page.getByRole("heading", { name: "应用中心", level: 1 }),
  ).toBeVisible();
}

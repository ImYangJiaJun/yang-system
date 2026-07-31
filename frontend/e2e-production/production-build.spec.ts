import { expect, test, type Page } from "@playwright/test";

async function serveAuthorizedModule(page: Page) {
  await page.addInitScript(() => {
    sessionStorage.setItem("yang.account-identity", "user");
  });
  await page.route("**/api/v1/users/refresh", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: { access_token: "production-build-token" },
      }),
      headers: {
        "Set-Cookie":
          "yang_refresh=production-build-refresh; Path=/api/v1/users; HttpOnly; SameSite=Strict",
      },
    }),
  );
  await page.route("**/.well-known/yang/ui-catalog", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          schema_version: "2.3",
          revision: "e".repeat(64),
          actions: [
            {
              operation_id: "account.user.me",
              title: "当前用户",
              description: "读取生产产物深链接",
              method: "GET",
              path: "/api/v1/account/user/me",
              params: [],
              input_schema: { type: "object", properties: {} },
              output_schema: {
                type: "object",
                properties: {
                  username: { type: "string", title: "用户名" },
                },
              },
              request_media_type: "json",
              response_kind: "json",
              requires_auth: true,
            },
          ],
          table_views: [],
          modules: [
            {
              module_id: "account.user",
              identity: {
                id: "user",
                title: "个人账户",
                icon: "person",
                order: 10,
              },
              title: "生产产物用户中心",
              description: "dist/spa 深链接",
              icon: "account",
              order: 10,
              primary_action: "account.user.me",
              actions: [],
              action_presentations: [],
              views: [],
            },
          ],
        },
      }),
    }),
  );
  await page.route("**/api/v1/account/user/me", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: { username: "production-user" },
      }),
    }),
  );
}

test("dist/spa 从正式模块深链接启动且不包含开发运行时", async ({ page }) => {
  await serveAuthorizedModule(page);

  const navigation = await page.goto("/module/account.user");

  expect(navigation?.headers()["x-yang-spa-fallback"]).toBe("index.html");
  await expect(
    page.getByRole("heading", { name: "生产产物用户中心" }),
  ).toBeVisible();
  await expect(
    page.getByText("production-user", { exact: true }),
  ).toBeVisible();
  const scriptSources = await page
    .locator("script[src]")
    .evaluateAll((nodes) =>
      nodes.map((node) => (node as HTMLScriptElement).src),
    );
  expect(scriptSources.length).toBeGreaterThan(0);
  expect(scriptSources).not.toContainEqual(expect.stringContaining("@vite"));
  await expect(page.locator("[data-vite-dev-id]")).toHaveCount(0);
});

test("生产路由移除 Workbench，缺失静态资源和 API 不被 history fallback 掩盖", async ({
  page,
  request,
}) => {
  await serveAuthorizedModule(page);

  await page.goto("/workbench");
  await expect(page).toHaveURL("/roles");
  await expect(page.getByText("YANG 接口工作台", { exact: true })).toHaveCount(
    0,
  );

  const missingAsset = await request.get("/assets/not-a-real-build-file.js");
  expect(missingAsset.status()).toBe(404);
  expect(missingAsset.headers()["content-type"]).not.toContain("text/html");

  const dottedRoute = await request.get("/module/report.json");
  expect(dottedRoute.status()).toBe(200);
  expect(dottedRoute.headers()["x-yang-spa-fallback"]).toBe("index.html");
  expect(dottedRoute.headers()["content-type"]).toContain("text/html");

  const missingApi = await request.get("/api/v1/not-a-real-endpoint");
  expect(missingApi.status()).toBe(404);
  expect(missingApi.headers()["content-type"] || "").not.toContain("text/html");
  expect(await missingApi.text()).not.toContain("<!doctype html>");
});

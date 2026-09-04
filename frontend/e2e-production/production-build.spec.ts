import { expect, test, type Page } from "@playwright/test";

/// 生产构建 E2E（对齐旧 e2e-production/production-build.spec.ts，路由改为 v2 /m/ 形态）。

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
              description: "dist 深链接",
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

test("dist 从正式模块深链接启动且满足生产响应头与缓存契约", async ({
  page,
  request,
}) => {
  await serveAuthorizedModule(page);

  const navigation = await page.goto("/m/account.user");

  expect(navigation?.headers()["x-yang-spa-fallback"]).toBe("index.html");
  expect(navigation?.headers()["cache-control"]).toBe("no-store");
  const contentSecurityPolicy =
    navigation?.headers()["content-security-policy"] || "";
  expect(contentSecurityPolicy).toContain("default-src 'self'");
  expect(contentSecurityPolicy).toContain("script-src 'self'");
  expect(contentSecurityPolicy).toContain("connect-src 'self'");
  expect(contentSecurityPolicy).toContain("frame-ancestors 'none'");
  expect(contentSecurityPolicy).not.toContain("'unsafe-eval'");
  expect(navigation?.headers()["strict-transport-security"]).toBe(
    "max-age=31536000; includeSubDomains",
  );
  expect(navigation?.headers()["x-content-type-options"]).toBe("nosniff");
  expect(navigation?.headers()["x-frame-options"]).toBe("DENY");
  expect(navigation?.headers()["referrer-policy"]).toBe("no-referrer");
  expect(navigation?.headers()["cross-origin-opener-policy"]).toBe(
    "same-origin",
  );
  expect(navigation?.headers()["permissions-policy"]).toBe(
    "camera=(), geolocation=(), microphone=()",
  );
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
  const firstScriptSource = scriptSources[0];
  if (!firstScriptSource) {
    throw new Error("生产入口没有可验证缓存策略的脚本资产");
  }
  const asset = await request.get(new URL(firstScriptSource).pathname);
  expect(asset.status()).toBe(200);
  expect(asset.headers()["cache-control"]).toBe(
    "public, max-age=31536000, immutable",
  );
  expect(asset.headers()["x-content-type-options"]).toBe("nosniff");
  expect(asset.headers()["x-yang-spa-fallback"]).toBeUndefined();
});

test("生产构建移除 Workbench，缺失静态资源和 API 不被 history fallback 掩盖", async ({
  page,
  request,
}) => {
  await serveAuthorizedModule(page);

  // 生产构建不含 workbench 路由：落入无匹配页，且工作台文案不存在于产物。
  await page.goto("/workbench");
  await expect(page.getByText("接口演示", { exact: true })).toHaveCount(0);

  const missingAsset = await request.get("/assets/not-a-real-build-file.js");
  expect(missingAsset.status()).toBe(404);
  expect(missingAsset.headers()["content-type"]).not.toContain("text/html");

  const missingApi = await request.get("/api/v1/not-a-real-endpoint");
  expect(missingApi.status()).toBe(404);
  expect(missingApi.headers()["content-type"] || "").not.toContain("text/html");
  expect(await missingApi.text()).not.toContain("<!doctype html>");
});

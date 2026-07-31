import { expect, test } from "@playwright/test";

const RELATED_REQUEST_ID = "0123456789abcdef0123456789abcdef";

test("API 与全局错误以无敏感正文指纹进入统一上报链", async ({ page }) => {
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
        data: { access_token: "observability-token" },
      }),
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
          revision: "9".repeat(64),
          actions: [
            {
              operation_id: "demo.failure.load",
              title: "失败数据",
              description: "验证诊断关联",
              method: "GET",
              path: "/api/v1/demo/failure",
              params: [],
              input_schema: { type: "object", properties: {} },
              output_schema: { type: "object" },
              request_media_type: "json",
              response_kind: "json",
              requires_auth: true,
            },
          ],
          table_views: [],
          modules: [
            {
              module_id: "demo.failure",
              identity: {
                id: "user",
                title: "个人账户",
                icon: "person",
                order: 10,
              },
              title: "诊断关联",
              description: "request id",
              icon: "monitor_heart",
              order: 10,
              primary_action: "demo.failure.load",
              actions: [],
              action_presentations: [],
              views: [],
            },
          ],
        },
      }),
    }),
  );
  await page.route("**/api/v1/demo/failure", (route) =>
    route.fulfill({
      status: 503,
      contentType: "application/json",
      headers: { "X-Request-Id": RELATED_REQUEST_ID },
      body: JSON.stringify({
        code: 500001,
        message: "不得进入诊断事件的后端敏感正文",
        details: { password: "never-report-me" },
      }),
    }),
  );

  const reports: Array<{
    authorization: string | undefined;
    body: Record<string, unknown>;
  }> = [];
  await page.route("**/api/v1/observability/frontend-errors", async (route) => {
    reports.push({
      authorization: route.request().headers()["authorization"],
      body: route.request().postDataJSON() as Record<string, unknown>,
    });
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: { accepted: true },
      }),
    });
  });

  await page.goto("/module/demo.failure");
  await expect(
    page.getByText("不得进入诊断事件的后端敏感正文", { exact: true }),
  ).toBeVisible();
  await expect.poll(() => reports.length).toBe(1);

  expect(reports[0]).toEqual({
    authorization: "Bearer observability-token",
    body: {
      event_id: expect.stringMatching(
        /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i,
      ),
      kind: "api",
      route: "module-page",
      fingerprint: expect.stringMatching(/^[0-9a-f]{16}$/),
      operation: "demo.failure.load",
      related_request_id: RELATED_REQUEST_ID,
      status: 503,
      error_code: 500001,
    },
  });
  expect(JSON.stringify(reports)).not.toContain("敏感正文");
  expect(JSON.stringify(reports)).not.toContain("never-report-me");

  await page.evaluate(() => {
    window.dispatchEvent(
      new ErrorEvent("error", {
        error: new Error("不得进入事件的浏览器运行时正文"),
        message: "不得进入事件的浏览器运行时正文",
      }),
    );
  });
  await expect.poll(() => reports.length).toBe(2);
  expect(reports[1]).toEqual({
    authorization: "Bearer observability-token",
    body: {
      event_id: expect.stringMatching(
        /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i,
      ),
      kind: "runtime",
      route: "module-page",
      fingerprint: expect.stringMatching(/^[0-9a-f]{16}$/),
    },
  });
  expect(JSON.stringify(reports[1])).not.toContain("浏览器运行时正文");
});

test("成功但畸形的 Catalog 响应仍保留 request id 关联", async ({ page }) => {
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
        data: { access_token: "observability-token" },
      }),
    }),
  );
  await page.route("**/.well-known/yang/ui-catalog", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      headers: { "X-Request-Id": RELATED_REQUEST_ID },
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          schema_version: "corrupted",
          secret: "must-not-cross-observability-boundary",
        },
      }),
    }),
  );

  const reports: Array<Record<string, unknown>> = [];
  await page.route("**/api/v1/observability/frontend-errors", async (route) => {
    reports.push(route.request().postDataJSON() as Record<string, unknown>);
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: { accepted: true },
      }),
    });
  });

  await page.goto("/roles");
  await expect.poll(() => reports.length).toBe(1);
  expect(reports[0]).toMatchObject({
    kind: "contract",
    operation: "account.user.ui_catalog",
    related_request_id: RELATED_REQUEST_ID,
  });
  expect(JSON.stringify(reports)).not.toContain("corrupted");
  expect(JSON.stringify(reports)).not.toContain(
    "must-not-cross-observability-boundary",
  );
});

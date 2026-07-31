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

function catalogModule(
  moduleId: string,
  identity: "user" | "admin",
  primaryAction: string,
) {
  return {
    module_id: moduleId,
    identity: {
      id: identity,
      title: identity === "user" ? "个人账户" : "管理平台",
      icon: identity === "user" ? "person" : "administrator",
      order: identity === "user" ? 10 : 30,
    },
    title: moduleId === "account.user" ? "用户中心" : "平台账号",
    description: "",
    icon: identity === "user" ? "account" : "admin_users",
    order: 10,
    primary_action: primaryAction,
    actions: [],
    action_presentations: [],
    views: [],
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
          schema_version: "2.3",
          revision: "a".repeat(64),
          actions: [
            catalogAction("account.user.me"),
            catalogAction("admin.user.list"),
          ],
          table_views: [],
          modules: [
            catalogModule("account.user", "user", "account.user.me"),
            catalogModule("admin.user", "admin", "admin.user.list"),
          ],
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
        },
      }),
      headers: {
        "Set-Cookie":
          "yang_refresh=refresh-token; Path=/api/v1/users; HttpOnly; SameSite=Strict",
      },
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
    .toBeNull();
  await expect
    .poll(() =>
      page.evaluate(() => sessionStorage.getItem("yang.refresh-token")),
    )
    .toBeNull();
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

test("访问令牌过期后自动刷新并留在当前流程", async ({ page }) => {
  const catalogAuthorizations: Array<string | undefined> = [];
  let refreshRequests = 0;
  await page.route("**/.well-known/yang/ui-catalog", async (route) => {
    const authorization = route.request().headers().authorization;
    catalogAuthorizations.push(authorization);
    if (authorization === "Bearer access-old") {
      await route.fulfill({
        status: 401,
        contentType: "application/json",
        body: JSON.stringify({ code: 40102, message: "Token 已过期" }),
      });
      return;
    }
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          schema_version: "2.3",
          revision: "b".repeat(64),
          actions: [catalogAction("account.user.me")],
          table_views: [],
          modules: [catalogModule("account.user", "user", "account.user.me")],
        },
      }),
    });
  });
  await page.route("**/api/v1/users/refresh", async (route) => {
    refreshRequests += 1;
    expect(route.request().postDataJSON()).toEqual({});
    const accessToken = refreshRequests === 1 ? "access-old" : "access-new";
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          access_token: accessToken,
        },
      }),
      headers: {
        "Set-Cookie":
          "yang_refresh=refresh-new; Path=/api/v1/users; HttpOnly; SameSite=Strict",
      },
    });
  });

  await page.goto("/roles");

  await expect(page.getByTestId("role-option-user")).toBeVisible();
  await expect(page).toHaveURL("/roles");
  expect(catalogAuthorizations).toEqual([
    "Bearer access-old",
    "Bearer access-new",
  ]);
  expect(refreshRequests).toBe(2);
  await expect
    .poll(() => page.evaluate(() => sessionStorage.getItem("yang.token")))
    .toBeNull();
  await expect
    .poll(() =>
      page.evaluate(() => sessionStorage.getItem("yang.refresh-token")),
    )
    .toBeNull();
});

test("伪造 Web Storage Token 且 Refresh Cookie 无效时保持未认证", async ({
  page,
}) => {
  await page.addInitScript(() => {
    sessionStorage.setItem("yang.token", "access-expired");
    sessionStorage.setItem("yang.tenant-id", "7");
    sessionStorage.setItem("yang.account-identity", "admin");
  });
  await page.route("**/.well-known/yang/ui-catalog", (route) =>
    route.fulfill({
      status: 401,
      contentType: "application/json",
      body: JSON.stringify({ code: 40102, message: "Token 已过期" }),
    }),
  );
  await page.route("**/api/v1/users/refresh", (route) =>
    route.fulfill({
      status: 401,
      contentType: "application/json",
      body: JSON.stringify({ code: 40103, message: "Refresh Token 已失效" }),
    }),
  );

  await page.goto("/roles");

  await expect(page).toHaveURL("/login");
  for (const key of [
    "yang.token",
    "yang.refresh-token",
    "yang.tenant-id",
    "yang.account-identity",
  ]) {
    await expect
      .poll(() => page.evaluate((name) => sessionStorage.getItem(name), key))
      .toBeNull();
  }
});

test("enforce CSP 拒绝内联脚本且不开放 unsafe-eval", async ({ page }) => {
  await page.goto("/login");
  const policy = await page
    .locator('meta[http-equiv="Content-Security-Policy"]')
    .getAttribute("content");

  expect(policy).toContain("script-src 'self'");
  expect(policy).not.toContain("'unsafe-eval'");
  const executed = await page.evaluate(() => {
    const marker = "__yang_inline_script_executed__";
    const script = document.createElement("script");
    script.textContent = `window.${marker} = true`;
    document.head.append(script);
    return (window as unknown as Record<string, unknown>)[marker] === true;
  });
  expect(executed).toBe(false);
});

test("多标签页串行轮换 Refresh Cookie 并同步退出且不共享持久化 Token", async ({
  context,
  page,
}) => {
  let activeRefreshes = 0;
  let maxConcurrentRefreshes = 0;
  await context.route("**/api/v1/users/refresh", async (route) => {
    activeRefreshes += 1;
    maxConcurrentRefreshes = Math.max(maxConcurrentRefreshes, activeRefreshes);
    await new Promise((resolve) => setTimeout(resolve, 100));
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: { access_token: "shared-memory-access" },
      }),
      headers: {
        "Set-Cookie":
          "yang_refresh=rotated-refresh; Path=/api/v1/users; HttpOnly; SameSite=Strict",
      },
    });
    activeRefreshes -= 1;
  });
  await context.route("**/.well-known/yang/ui-catalog", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          schema_version: "2.3",
          revision: "e".repeat(64),
          actions: [catalogAction("account.user.me")],
          table_views: [],
          modules: [catalogModule("account.user", "user", "account.user.me")],
        },
      }),
    }),
  );
  await context.route("**/api/v1/users/logout", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ code: 0, message: "成功", data: null }),
      headers: {
        "Set-Cookie":
          "yang_refresh=; Path=/api/v1/users; HttpOnly; SameSite=Strict; Max-Age=0",
      },
    }),
  );
  const otherPage = await context.newPage();

  await Promise.all([page.goto("/roles"), otherPage.goto("/roles")]);
  await expect(page.getByTestId("role-option-user")).toBeVisible();
  await expect(otherPage.getByTestId("role-option-user")).toBeVisible();
  expect(maxConcurrentRefreshes).toBe(1);
  await expect
    .poll(() => page.evaluate(() => sessionStorage.getItem("yang.token")))
    .toBeNull();
  await expect
    .poll(() => otherPage.evaluate(() => sessionStorage.getItem("yang.token")))
    .toBeNull();

  await page.getByRole("button", { name: "退出登录" }).click();

  await expect(page).toHaveURL("/login");
  await expect(otherPage).toHaveURL(/\/login\?reason=session-expired$/);
});

import { expect, test, type Page } from "@playwright/test";

function action(
  operationId: string,
  title: string,
  description: string,
  method: "GET" | "POST" = "GET",
) {
  const listProperties = operationId.endsWith(".list")
    ? {
        page: { type: "integer" },
        limit: { type: "integer" },
        ...(operationId === "admin.user.list"
          ? { search: { type: ["string", "null"] } }
          : {}),
      }
    : {};
  const createProperties =
    operationId === "org.tenant.create"
      ? {
          name: { type: "string", title: "企业名称" },
          code: { type: "string", title: "企业编号" },
        }
      : {};
  return {
    operation_id: operationId,
    title,
    description,
    method,
    path: operationId.startsWith("org.tenant.")
      ? "/api/v1/tenants"
      : `/api/v1/${operationId.replaceAll(".", "/")}`,
    params: [],
    input_schema: {
      type: "object",
      properties: { ...listProperties, ...createProperties },
      ...(operationId === "org.tenant.create"
        ? { required: ["name", "code"] }
        : {}),
    },
    output_schema: { type: "object" },
    request_media_type: "json",
    response_kind: "json",
    requires_auth: true,
  };
}

async function serveCatalog(page: Page, actions: ReturnType<typeof action>[]) {
  await page.addInitScript(() => {
    sessionStorage.setItem("yang.token", "account-space-test-token");
  });
  await page.route("**/.well-known/yang/ui-catalog", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          schema_version: "2.2",
          revision: "c".repeat(64),
          actions,
          table_views: [],
        },
      }),
    }),
  );
}

test("登录后无需刷新即可获得完整账号身份目录", async ({ page }) => {
  const userActions = [
    action("account.user.me", "当前用户", "查看当前登录账号"),
  ];
  const authenticatedActions = [
    ...userActions,
    action("admin.user.list", "平台账号列表", "分页查看平台账号"),
    action("org.tenant.list", "我的企业", "查看当前账号加入的企业"),
    action("org.tenant.create", "创建企业", "创建新的企业账户", "POST"),
  ];
  await page.route("**/.well-known/yang/ui-catalog", (route) => {
    const authenticated = Boolean(route.request().headers().authorization);
    return route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          schema_version: "2.2",
          revision: "c".repeat(64),
          actions: authenticated ? authenticatedActions : userActions,
          table_views: [],
        },
      }),
    });
  });
  await page.route("**/api/v1/users/login", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          access_token: "new-session-token",
          refresh_token: "new-refresh-token",
        },
      }),
    }),
  );

  await page.goto("/");
  await expect(page.getByRole("tab", { name: "个人账户" })).toBeVisible();
  await expect(page.getByRole("tab", { name: "管理平台" })).toHaveCount(0);

  await page.getByRole("link", { name: "登录" }).click();
  await page.getByLabel("帐号").fill("alice");
  await page.getByLabel("密码").fill("correct-password");
  await page.getByRole("button", { name: "登录" }).click();

  await expect(page).toHaveURL("/");
  await expect(page.getByRole("tab", { name: "管理平台" })).toBeVisible();
  await expect(page.getByRole("tab", { name: "企业账户" })).toBeVisible();
  await expect(page.getByTestId("module-page-account.user")).toBeVisible();
  await expect(page.getByTestId("module-page-admin.user")).toHaveCount(0);
});

test("每个已授权后端 Module 都生成对应的 BR 页面", async ({ page }) => {
  await serveCatalog(page, [
    action("account.user.me", "当前用户", "查看当前登录账号"),
    action("admin.user.list", "平台账号列表", "分页查看平台账号"),
    action("admin.user.add", "添加平台账号", "创建平台账号", "POST"),
    action("org.tenant.list", "我的企业", "查看当前账号加入的企业"),
    action("org.tenant.create", "创建企业", "创建新的企业账户", "POST"),
  ]);
  await page.route("**/api/v1/admin/user/list**", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          items: [
            {
              id: 11,
              user_user: 7,
              username: "alice",
              name: "Alice",
              position: "管理员",
              status: "active",
              admin: true,
              created_at: 1_700_000_000,
              updated_at: 1_700_000_000,
            },
          ],
          total: 1,
          page: 1,
          limit: 20,
        },
      }),
    }),
  );
  await page.route("**/api/v1/account/user/me**", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          id: 7,
          username: "alice",
          status: "active",
          created_at: 1_700_000_000,
          updated_at: 1_700_000_000,
        },
      }),
    }),
  );
  let createdOrganization: unknown;
  await page.route("**/api/v1/tenants**", async (route) => {
    if (route.request().method() === "POST") {
      createdOrganization = route.request().postDataJSON();
      await route.fulfill({
        status: 201,
        contentType: "application/json",
        body: JSON.stringify({
          code: 0,
          message: "成功",
          data: { id: 24, name: "新企业", code: "NEWCO" },
        }),
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
          items: [{ id: 23, name: "示例企业", code: "ACME" }],
          total: 1,
          page: 1,
          limit: 100,
          total_pages: 1,
        },
      }),
    });
  });

  await page.goto("/");

  await expect(page.getByRole("tab", { name: "个人账户" })).toBeVisible();
  await expect(page.getByRole("tab", { name: "管理平台" })).toBeVisible();
  await expect(page.getByRole("tab", { name: "企业账户" })).toBeVisible();
  await expect(page.getByTestId("module-page-account.user")).toBeVisible();
  await expect(page.getByTestId("module-page-admin.user")).toHaveCount(0);
  await expect(page.getByTestId("module-page-org.tenant")).toHaveCount(0);

  await page.getByRole("tab", { name: "管理平台" }).click();
  await expect(page).toHaveURL("/module/admin.user");
  await expect(page.getByRole("heading", { name: "平台账号" })).toBeVisible();
  await expect(page.getByText("平台账号列表", { exact: true })).toBeVisible();
  await expect(page.getByText("alice", { exact: true })).toBeVisible();

  await page.getByRole("button", { name: "添加平台账号" }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "添加平台账号" }),
  ).toBeVisible();
  await expect(page.locator(".operation-id")).toHaveCount(0);
  await expect(page.locator(".route-line")).toHaveCount(0);
  await page.getByRole("button", { name: "关闭" }).click();

  await page.getByRole("tab", { name: "应用中心" }).click();
  await expect(page.getByTestId("module-page-admin.user")).toBeVisible();
  await expect(page.getByTestId("module-page-account.user")).toHaveCount(0);
  await expect(page.getByTestId("module-page-org.tenant")).toHaveCount(0);

  await page.getByRole("button", { name: "账号菜单" }).click();
  const accountMenu = page.locator(".account-switcher-menu");
  await expect(
    accountMenu.getByText("个人账户", { exact: true }),
  ).toBeVisible();
  await expect(
    accountMenu.getByText("管理平台", { exact: true }).last(),
  ).toBeVisible();
  await expect(
    accountMenu.getByText("示例企业", { exact: true }),
  ).toBeVisible();
  await expect(page.getByLabel("企业租户 ID")).toHaveCount(0);

  await accountMenu.getByText("个人账户", { exact: true }).click();
  await expect(page).toHaveURL("/module/account.user");
  await expect(page.getByRole("heading", { name: "用户中心" })).toBeVisible();

  await page.getByRole("button", { name: "账号菜单" }).click();
  await accountMenu.getByText("示例企业", { exact: true }).click();
  await expect(page).toHaveURL("/module/org.tenant");
  await expect(
    page.getByText("示例企业", { exact: true }).first(),
  ).toBeVisible();
  await expect
    .poll(() => page.evaluate(() => sessionStorage.getItem("yang.tenant-id")))
    .toBe("23");

  await page.getByRole("button", { name: "创建企业" }).click();
  const createDialog = page.getByRole("dialog");
  await createDialog.getByLabel("企业名称").fill("新企业");
  await createDialog.getByLabel("企业编号").fill("NEWCO");
  await createDialog.getByRole("button", { name: "创建企业" }).click();
  await expect
    .poll(() => createdOrganization)
    .toEqual({
      name: "新企业",
      code: "NEWCO",
    });
});

test("直接访问未授权 Module 页面时保持 fail-closed", async ({ page }) => {
  await serveCatalog(page, [
    action("account.user.me", "当前用户", "查看当前登录账号"),
  ]);

  await page.goto("/module/admin.user");

  await expect(
    page.getByRole("heading", { name: "当前身份无法访问该模块" }),
  ).toBeVisible();
  await expect(page.getByRole("tab", { name: "管理平台" })).toHaveCount(0);
  await expect(page.getByText("平台账号列表", { exact: true })).toHaveCount(0);
});

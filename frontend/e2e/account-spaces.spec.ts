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

function modulesForActions(actions: ReturnType<typeof action>[]) {
  const ids = new Set(actions.map((item) => item.operation_id));
  const identity = (
    id: "user" | "admin" | "org",
    title: string,
    icon: string,
    order: number,
  ) => ({ id, title, icon, order });
  const module = (
    moduleId: string,
    moduleIdentity: ReturnType<typeof identity>,
    title: string,
    icon: string,
    primaryAction?: string,
    secondaryAction?: string,
  ) => ({
    module_id: moduleId,
    identity: moduleIdentity,
    title,
    description: "",
    icon,
    order: 10,
    ...(primaryAction ? { primary_action: primaryAction } : {}),
    actions:
      secondaryAction && ids.has(secondaryAction) ? [secondaryAction] : [],
    action_presentations:
      secondaryAction && ids.has(secondaryAction)
        ? [
            {
              operation_id: secondaryAction,
              title:
                actions.find((item) => item.operation_id === secondaryAction)
                  ?.title ?? secondaryAction,
              placement: "toolbar",
              interaction: "form",
            },
          ]
        : [],
    views: [],
  });
  const result = [];
  if (ids.has("account.user.me")) {
    result.push(
      module(
        "account.user",
        identity("user", "个人账户", "person", 10),
        "用户中心",
        "account",
        "account.user.me",
      ),
    );
  }
  if (ids.has("admin.user.list")) {
    result.push(
      module(
        "admin.user",
        identity("admin", "管理平台", "administrator", 30),
        "平台账号",
        "admin_users",
        "admin.user.list",
        "admin.user.add",
      ),
    );
  }
  const orgIdentity = identity("org", "企业账户", "organization", 20);
  if (ids.has("org.tenant.list")) {
    result.push(
      module(
        "org.tenant",
        orgIdentity,
        "我的企业",
        "organizations",
        "org.tenant.list",
        "org.tenant.create",
      ),
    );
  }
  if (ids.has("org.org.list")) {
    result.push(
      module(
        "org.org",
        orgIdentity,
        "企业资料",
        "organization_profile",
        "org.org.list",
      ),
    );
  }
  if (ids.has("org.user.select")) {
    result.push(
      module(
        "org.user",
        orgIdentity,
        "企业成员",
        "organization_members",
        "org.user.select",
      ),
    );
  }
  return result;
}

async function serveCatalog(
  page: Page,
  actions: ReturnType<typeof action>[],
  identity: "user" | "admin" | "org" = "user",
) {
  await page.addInitScript((selectedIdentity) => {
    sessionStorage.setItem("yang.account-identity", selectedIdentity);
  }, identity);
  await page.route("**/api/v1/users/refresh", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: { access_token: "account-space-test-token" },
      }),
      headers: {
        "Set-Cookie":
          "yang_refresh=account-space-refresh; Path=/api/v1/users; HttpOnly; SameSite=Strict",
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
          revision: "c".repeat(64),
          actions,
          table_views: [],
          modules: modulesForActions(actions),
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
          schema_version: "2.3",
          revision: (authenticated ? "d" : "c").repeat(64),
          actions: authenticated ? authenticatedActions : userActions,
          table_views: [],
          modules: modulesForActions(
            authenticated ? authenticatedActions : userActions,
          ),
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
        },
      }),
    }),
  );

  await page.goto("/");
  await expect(page).toHaveURL("/login");
  await page.getByLabel("帐号").fill("alice");
  await page.getByLabel("密码").fill("correct-password");
  await page.getByRole("button", { name: "登录" }).click();

  await expect(page).toHaveURL("/roles");
  await expect(page.getByTestId("role-option-user")).toBeVisible();
  await expect(page.getByTestId("role-option-admin")).toBeVisible();
  await expect(page.getByTestId("role-option-org")).toBeVisible();
  await page.getByRole("button", { name: "选择个人账户角色" }).click();

  await expect(page).toHaveURL("/module/account.user");
  await expect(page.getByTestId("module-nav-account.user")).toBeVisible();
  await expect(page.getByTestId("module-nav-admin.user")).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "用户中心" })).toBeVisible();
});

test("敏感操作收到 428 后重认证，并只携带内存 proof 重试一次", async ({
  page,
}) => {
  const actions = [
    action("admin.user.list", "平台账号列表", "分页查看平台账号"),
    action("admin.user.add", "添加平台账号", "创建平台账号", "POST"),
  ];
  await serveCatalog(page, actions, "admin");
  await page.route("**/api/v1/admin/user/list**", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: { items: [], total: 0, page: 1, limit: 20 },
      }),
    }),
  );
  let protectedAttempts = 0;
  const observedProofs: Array<string | undefined> = [];
  await page.route("**/api/v1/admin/user/add", (route) => {
    protectedAttempts += 1;
    observedProofs.push(route.request().headers()["x-step-up-proof"]);
    if (protectedAttempts === 1) {
      return route.fulfill({
        status: 428,
        contentType: "application/json",
        body: JSON.stringify({
          code: 40110,
          message: "敏感操作需要重新认证",
          data: { challenge: "browser-signed-challenge", expires_in: 120 },
        }),
      });
    }
    return route.fulfill({
      status: 201,
      contentType: "application/json",
      body: JSON.stringify({ code: 0, message: "创建成功", data: { id: 7 } }),
    });
  });
  await page.route("**/api/v1/users/step-up/complete", async (route) => {
    const body = route.request().postDataJSON();
    expect(body).toEqual({
      challenge: "browser-signed-challenge",
      credentials: { username: "admin", password: "correct-password" },
    });
    return route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: { proof: "browser-one-shot-proof", expires_in: 300 },
      }),
    });
  });

  await page.goto("/module/admin.user");
  await page.getByRole("button", { name: "添加平台账号" }).click();
  const actionDialog = page.getByRole("dialog", { name: "添加平台账号" });
  await actionDialog.getByRole("button", { name: "添加平台账号" }).click();

  const stepUpDialog = page.getByRole("dialog", {
    name: "敏感操作重新认证",
  });
  await stepUpDialog.getByLabel("用户名").fill("admin");
  await stepUpDialog.getByLabel("密码").fill("correct-password");
  await stepUpDialog.getByRole("button", { name: "验证并继续" }).click();

  await expect(stepUpDialog).toBeHidden();
  await expect(actionDialog).toBeHidden();
  expect(protectedAttempts).toBe(2);
  expect(observedProofs).toEqual([undefined, "browser-one-shot-proof"]);
  const stored = await page.evaluate(() =>
    JSON.stringify({
      session: { ...sessionStorage },
      local: { ...localStorage },
    }),
  );
  expect(stored).not.toContain("browser-one-shot-proof");
  expect(stored).not.toContain("correct-password");
});

test("每个已授权后端 Module 都生成对应的 BR 页面", async ({ page }) => {
  await serveCatalog(page, [
    action("account.user.me", "当前用户", "查看当前登录账号"),
    action("admin.user.list", "平台账号列表", "分页查看平台账号"),
    action("admin.user.add", "添加平台账号", "创建平台账号", "POST"),
    action("org.tenant.list", "我的企业", "查看当前账号加入的企业"),
    action("org.tenant.create", "创建企业", "创建新的企业账户", "POST"),
    action("org.org.list", "企业资料", "查看当前企业资料"),
    action("org.user.select", "企业成员", "查看当前企业成员"),
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

  await page.goto("/module/account.user");

  await expect(page.getByTestId("module-nav-account.user")).toBeVisible();
  await expect(page.getByTestId("module-nav-admin.user")).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "用户中心" })).toBeVisible();

  await page.getByRole("button", { name: "账号菜单" }).click();
  await page
    .locator(".account-switcher-menu")
    .getByText("管理平台", { exact: true })
    .last()
    .click();
  await expect(page).toHaveURL("/module/admin.user");
  await expect(page.getByTestId("module-nav-admin.user")).toBeVisible();
  await expect(page.getByTestId("module-nav-account.user")).toHaveCount(0);
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
  await expect(page.getByTestId("module-nav-org.tenant")).toBeVisible();
  await expect(page.getByTestId("module-nav-org.org")).toBeVisible();
  await expect(page.getByTestId("module-nav-org.user")).toBeVisible();
  await expect(page.getByTestId("module-nav-account.user")).toHaveCount(0);
  await expect
    .poll(() => page.evaluate(() => sessionStorage.getItem("yang.tenant-id")))
    .toBe("23");

  await page.getByTestId("module-nav-org.org").click();
  await expect(page).toHaveURL("/module/org.org");
  await page.getByTestId("module-nav-org.tenant").click();
  await expect(page).toHaveURL("/module/org.tenant");

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

  await expect(page).toHaveURL("/roles");
  await expect(
    page.getByRole("heading", { name: "选择本次使用的角色" }),
  ).toBeVisible();
  await expect(page.getByText("平台账号列表", { exact: true })).toHaveCount(0);
});

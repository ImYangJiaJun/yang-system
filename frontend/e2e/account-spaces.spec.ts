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
  return {
    operation_id: operationId,
    title,
    description,
    method,
    path: `/api/v1/${operationId.replaceAll(".", "/")}`,
    params: [],
    input_schema: { type: "object", properties: listProperties },
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

test("每个已授权后端 Module 都生成对应的 BR 页面", async ({ page }) => {
  await serveCatalog(page, [
    action("account.user.me", "当前用户", "查看当前登录账号"),
    action("admin.user.list", "平台账号列表", "分页查看平台账号"),
    action("admin.user.add", "添加平台账号", "创建平台账号", "POST"),
    action("org.access.list", "我的企业", "查看当前账号加入的企业"),
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

  await page.goto("/");

  await expect(page.getByRole("tab", { name: "个人账户" })).toBeVisible();
  await expect(page.getByRole("tab", { name: "管理平台" })).toBeVisible();
  await expect(page.getByRole("tab", { name: "企业账户" })).toBeVisible();
  await expect(page.getByTestId("module-page-account.user")).toBeVisible();
  await expect(page.getByTestId("module-page-admin.user")).toBeVisible();
  await expect(page.getByTestId("module-page-org.access")).toBeVisible();

  await page.getByTestId("module-page-admin.user").click();
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

  await page.getByRole("button", { name: "账号菜单" }).click();
  await expect(
    page.getByText("用户中心", { exact: true }).last(),
  ).toBeVisible();
  await expect(
    page.getByText("平台账号", { exact: true }).last(),
  ).toBeVisible();
  await expect(
    page.getByText("我的企业", { exact: true }).last(),
  ).toBeVisible();
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

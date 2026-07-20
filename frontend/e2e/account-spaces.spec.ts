import { expect, test, type Page } from "@playwright/test";

function action(
  operationId: string,
  title: string,
  description: string,
  method: "GET" | "POST" = "GET",
) {
  return {
    operation_id: operationId,
    title,
    description,
    method,
    path: `/api/v1/${operationId.replaceAll(".", "/")}`,
    params: [],
    input_schema: { type: "object", properties: {} },
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

test("已授权目录展示 user、admin、org 三类 BR 账号空间", async ({ page }) => {
  await serveCatalog(page, [
    action("account.user.me", "当前用户", "查看当前登录账号"),
    action("admin.user.list", "平台账号列表", "分页查看平台账号"),
    action("admin.user.add", "添加平台账号", "创建平台账号", "POST"),
    action("org.access.list", "我的企业", "查看当前账号加入的企业"),
  ]);

  await page.goto("/");

  await expect(page.getByRole("tab", { name: "个人账户" })).toBeVisible();
  await expect(page.getByRole("tab", { name: "管理平台" })).toBeVisible();
  await expect(page.getByRole("tab", { name: "企业账户" })).toBeVisible();
  await expect(page.getByTestId("account-space-user")).toBeVisible();
  await expect(page.getByTestId("account-space-admin")).toBeVisible();
  await expect(page.getByTestId("account-space-org")).toBeVisible();

  await page.getByTestId("account-space-admin").click();
  await expect(page).toHaveURL("/space/admin");
  await expect(page.getByRole("heading", { name: "管理平台" })).toBeVisible();
  await expect(page.getByText("平台账号列表", { exact: true })).toBeVisible();
  await expect(page.getByText("添加平台账号", { exact: true })).toBeVisible();

  await page.getByText("平台账号列表", { exact: true }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "平台账号列表" }),
  ).toBeVisible();
  await expect(page.locator(".operation-id")).toHaveCount(0);
  await expect(page.locator(".route-line")).toHaveCount(0);
  await page.getByRole("button", { name: "关闭" }).click();

  await page.getByRole("button", { name: "账号菜单" }).click();
  await expect(
    page.getByText("个人账户", { exact: true }).last(),
  ).toBeVisible();
  await expect(
    page.getByText("管理平台", { exact: true }).last(),
  ).toBeVisible();
  await expect(
    page.getByText("企业账户", { exact: true }).last(),
  ).toBeVisible();
});

test("直接访问未授权管理空间时保持 fail-closed", async ({ page }) => {
  await serveCatalog(page, [
    action("account.user.me", "当前用户", "查看当前登录账号"),
  ]);

  await page.goto("/space/admin");

  await expect(
    page.getByRole("heading", { name: "当前账号无法访问此空间" }),
  ).toBeVisible();
  await expect(page.getByRole("tab", { name: "管理平台" })).toHaveCount(0);
  await expect(page.getByText("平台账号列表", { exact: true })).toHaveCount(0);
});

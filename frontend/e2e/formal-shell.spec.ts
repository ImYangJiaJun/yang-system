import { expect, test } from "@playwright/test";

test("正式控制台使用 BR 生态的应用中心与模块导航", async ({ page }) => {
  await page.goto("/");

  await expect(page.getByRole("heading", { name: "应用中心" })).toBeVisible();
  await expect(page.getByRole("tab", { name: "应用中心" })).toBeVisible();
  await expect(page.getByRole("tab", { name: "业务空间" })).toBeVisible();
  await expect(page.locator(".navigation-mode")).toHaveCount(0);

  await page.getByRole("tab", { name: "开发工作台" }).click();
  await expect(page).toHaveURL("/workbench");
  await expect(
    page.getByText("YANG 接口工作台", { exact: true }),
  ).toBeVisible();
  await expect(page.locator(".navigation-mode")).toBeVisible();
});

test("正式业务入口使用目录投影的通用页面", async ({ page }) => {
  await page.goto("/");

  await page.getByText("项目目录", { exact: true }).first().click();

  await expect(page).toHaveURL("/business");
  await expect(page.getByRole("heading", { name: "项目目录" })).toBeVisible();
  await expect(
    page.locator(".formal-nav-list").getByText("项目目录", { exact: true }),
  ).toBeVisible();
  await expect(page.locator(".navigation-mode")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "新增项目" })).toBeVisible();
});

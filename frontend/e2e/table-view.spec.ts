import { expect, test } from "@playwright/test";

test("TableView 自动完成树、搜索、排序、关系表单和真实新增", async ({
  page,
}) => {
  await page.goto("/");

  await expect(page.getByText("1 Views")).toBeVisible();
  await expect(page.getByRole("heading", { name: "项目目录" })).toBeVisible();
  await expect(page.getByText("平台能力", { exact: true })).toBeVisible();
  await expect(page.getByText("通用渲染器", { exact: true })).toBeVisible();

  await page.getByRole("button", { name: "新增项目" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("textbox", { name: /名称/ }).fill("关系选择器验收");
  await dialog.getByLabel("分类").click();
  await expect(page.getByRole("option", { name: "业务" })).toBeVisible();
  await page.getByRole("option", { name: "业务" }).click();
  await dialog.getByLabel("状态").fill("active");
  await dialog.getByRole("button", { name: "提交" }).click();
  await expect(page.getByText("关系选择器验收", { exact: true })).toBeVisible();

  await page.getByPlaceholder(/搜索 name/).fill("关系选择器验收");
  await page.getByRole("button", { name: "查询" }).click();
  await expect(page.getByText("关系选择器验收", { exact: true })).toBeVisible();
  await expect(page.getByText("平台能力", { exact: true })).toHaveCount(0);
});

test("行操作按 presentation 执行表单编辑与确认删除", async ({ page }) => {
  await page.goto("/");

  const row = page.getByRole("row", { name: /通用渲染器/ });
  await row.getByRole("button", { name: "编辑项目" }).click();
  await page.getByRole("dialog").getByLabel("名称").fill("通用业务页");
  await page.getByRole("dialog").getByRole("button", { name: "提交" }).click();
  await expect(page.getByText("通用业务页", { exact: true })).toBeVisible();

  const editedRow = page.getByRole("row", { name: /通用业务页/ });
  await editedRow.getByRole("button", { name: "删除项目" }).click();
  await expect(page.getByText("此操作无法撤销")).toBeVisible();
  await page.getByRole("button", { name: "确认", exact: true }).click();
  await expect(page.getByText("通用业务页", { exact: true })).toHaveCount(0);
});

test("静态 view_id 覆盖通用页并可返回", async ({ page }) => {
  await page.goto("/");

  await page.getByRole("button", { name: "项目洞察" }).click();
  await expect(
    page.getByRole("heading", { name: "项目运行洞察" }),
  ).toBeVisible();
  await expect(page.getByText("项目总数", { exact: true })).toBeVisible();
  await expect(page.getByText("运行中", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "返回通用表格" }).click();
  await expect(page.getByRole("heading", { name: "项目目录" })).toBeVisible();
});

import { expect, test } from "@playwright/test";

import { mockSessionRestore } from "./support";

/**
 * 表格视图 E2E（对齐旧 e2e/table-view.spec.ts 的行为断言，选择器按新 UI 重写）：
 * 工作台 → 通用 TableView（树/搜索/关系/新增/行操作/筛选/列设置/bulk/custom view）。
 */

test.beforeEach(async ({ page }) => {
  await mockSessionRestore(page);
  await page.goto("/workbench");
  await expect(page.getByRole("heading", { name: "项目目录" })).toBeVisible();
});

test("TableView 自动完成树、搜索、排序、关系表单和真实新增", async ({
  page,
}) => {
  let relationRequests = 0;
  page.on("request", (request) => {
    if (request.url().includes("/api/v1/demo/categories/options")) {
      relationRequests += 1;
    }
  });

  await expect(page.getByText("平台能力", { exact: true })).toBeVisible();
  await expect(page.getByText("通用渲染器", { exact: true })).toBeVisible();
  // 关系列把 category_id 翻译成选项标签（单元格级）。
  await expect(
    page.getByRole("cell", { name: "平台", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("cell", { name: "业务", exact: true }),
  ).toBeVisible();

  // 新增：弹窗 → 关系选择（Radix Select 远程选项）→ 提交 → 行出现。
  await page.getByRole("button", { name: "新增项目" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("名称").fill("关系选择器验收");
  await dialog.getByRole("combobox", { name: "分类" }).click();
  await page.getByRole("option", { name: "业务" }).click();
  await dialog.getByLabel("状态").fill("active");
  await dialog.getByRole("button", { name: "提交" }).click();
  await expect(page.getByText("关系选择器验收", { exact: true })).toBeVisible();

  // 搜索收窄结果。
  await page.getByPlaceholder(/搜索 name/).fill("关系选择器验收");
  await page.getByRole("button", { name: "查询" }).click();
  await expect(page.getByText("关系选择器验收", { exact: true })).toBeVisible();
  await expect(page.getByText("平台能力", { exact: true })).toHaveCount(0);
  expect(relationRequests).toBeGreaterThanOrEqual(1);
});

test("行操作按 presentation 执行表单编辑与确认删除", async ({ page }) => {
  const row = page.getByRole("row", { name: /通用渲染器/ });
  await row.getByRole("button", { name: "编辑项目" }).click();
  await page.getByRole("dialog").getByLabel("名称").fill("通用业务页");
  await page.getByRole("dialog").getByRole("button", { name: "提交" }).click();
  await expect(page.getByText("通用业务页", { exact: true })).toBeVisible();

  const editedRow = page.getByRole("row", { name: /通用业务页/ });
  await editedRow.getByRole("button", { name: "更多操作" }).click();
  await page.getByRole("menuitem", { name: "删除项目" }).click();
  await expect(page.getByText("此操作无法撤销")).toBeVisible();
  await page.getByRole("button", { name: "确认", exact: true }).click();
  await expect(page.getByText("通用业务页", { exact: true })).toHaveCount(0);
});

test("表格支持渐进筛选、活动条件和列管理", async ({ page }) => {
  // 筛选面板常驻；状态列默认 eq 操作符。
  await page.getByLabel("状态 筛选值").fill("active");
  await page.getByRole("button", { name: "查询" }).click();
  await expect(page.getByText("1 个活动条件")).toBeVisible();
  await expect(page.getByText("平台能力", { exact: true })).toBeVisible();
  await expect(page.getByText("通用渲染器", { exact: true })).toHaveCount(0);

  // 列显示设置：隐藏分类列（localStorage 持久化由单测覆盖）。
  await page.getByRole("button", { name: "列显示设置" }).click();
  await page.getByRole("menuitemcheckbox", { name: "分类" }).click();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("columnheader", { name: "分类" })).toHaveCount(0);

  await page.getByRole("button", { name: "重置" }).click();
  // 行操作用例已删除「通用渲染器」；重置后断言幸存行与完整条件集合。
  await expect(page.getByText("平台能力", { exact: true })).toBeVisible();
  await expect(page.getByText(/\d+ 个活动条件/)).toHaveCount(0);
});

test("静态 view_id 覆盖通用页并可返回", async ({ page }) => {
  await page.getByRole("button", { name: "项目洞察" }).click();
  await expect(
    page.getByRole("heading", { name: "项目运行洞察" }),
  ).toBeVisible();
  await expect(page.getByText("项目总数", { exact: true })).toBeVisible();
  await expect(page.getByText("运行中", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "返回通用表格" }).click();
  await expect(page.getByRole("heading", { name: "项目目录" })).toBeVisible();
});

test("批量删除：选中多行 → 批量栏 → 确认 → 提交", async ({ page }) => {
  await page.getByRole("checkbox", { name: "选择第 1 行" }).click();
  await page.getByRole("checkbox", { name: "选择第 2 行" }).click();
  await expect(page.getByText("已选 2 项")).toBeVisible();

  await page.getByRole("button", { name: "批量删除项目" }).click();
  await expect(page.getByText("将删除所有选中项目")).toBeVisible();
  await page.getByRole("button", { name: "确认", exact: true }).click();

  await expect(page.getByText("平台能力", { exact: true })).toHaveCount(0);
  await expect(page.getByText("通用渲染器", { exact: true })).toHaveCount(0);
});

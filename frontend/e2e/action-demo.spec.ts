import { expect, test } from "@playwright/test";

test("后端新增 Action 后默认页面可发现、填参、真实调用并展示结果", async ({
  page,
}) => {
  await page.goto("/workbench");

  await page
    .locator(".navigation-mode")
    .getByText("接口演示", { exact: true })
    .click();
  await expect(page.getByText("12 Actions")).toBeVisible();
  await page.getByRole("button", { name: /回显输入/ }).click();
  await expect(page.getByRole("radio", { name: "接口演示" })).toBeChecked();
  await page.getByLabel("消息").fill("第一性原理验收");
  await page.getByRole("button", { name: "发起真实调用" }).click();

  const result = page.getByTestId("action-result");
  await expect(result).toContainText("HTTP 200");
  await expect(result).toContainText("第一性原理验收");
  await expect(result).toContainText('"length": 7');
  await expect(result).toContainText("request-id:");
});

test("下载、预览和重定向按声明的响应通道安全展示", async ({ page }) => {
  await page.goto("/workbench");
  await page
    .locator(".navigation-mode")
    .getByText("接口演示", { exact: true })
    .click();
  await expect(page.getByText("12 Actions")).toBeVisible();

  await page.getByRole("button", { name: /下载验收文件/ }).click();
  await page.getByRole("button", { name: "发起真实调用" }).click();
  await expect(page.getByTestId("action-result")).toContainText("下载文件");
  await expect(page.getByTestId("action-result")).toContainText("验收报告.txt");
  await expect(page.getByTestId("action-result")).toContainText("request-id:");

  await page.getByRole("button", { name: /预览验收文件/ }).click();
  await page.getByRole("button", { name: "发起真实调用" }).click();
  await expect(page.getByTestId("action-result")).toContainText("打开预览");
  await expect(page.getByTestId("action-result")).toContainText("request-id:");

  await page.getByRole("button", { name: /重定向验收/ }).click();
  await page.getByRole("button", { name: "发起真实调用" }).click();
  await expect(page.getByTestId("action-result")).toContainText(
    "服务端请求重定向",
  );
  await expect(page.getByTestId("action-result")).toContainText(
    "浏览器安全策略隐藏 Location，页面未自动跳转",
  );
});

test("multipart Action 生成受限文件表单并真实上传", async ({ page }) => {
  await page.goto("/workbench");
  await page
    .locator(".navigation-mode")
    .getByText("接口演示", { exact: true })
    .click();

  await page.getByRole("button", { name: /上传验收文件/ }).click();
  await page.getByLabel("title").fill("上传验收");
  await page.getByLabel("file").setInputFiles("e2e/fixtures/report.txt");
  await page.getByRole("button", { name: "发起真实调用" }).click();

  await expect(page.getByText("HTTP 200")).toBeVisible();
  await expect(page.locator(".result-panel")).toContainText("report.txt");
  await expect(page.locator(".result-panel")).toContainText("上传验收");
});

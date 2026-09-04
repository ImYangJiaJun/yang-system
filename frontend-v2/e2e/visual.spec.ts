import { expect, test } from "@playwright/test";

import { mockSessionRestore } from "./support";

/**
 * 视觉回归基线（ADR-5 §2.5）：核心页面明暗双主题截图。
 * 基线只在 chromium 采集（跨浏览器字体栅格化差异不构成产品信号）。
 */

// 视觉基线仅限 chromium 项目采集（跨浏览器字体栅格化差异不构成产品信号）。
test.skip(({ browserName }) => browserName !== "chromium");

// 截图基线写入与多场景截图较慢，放宽用例超时。
test.setTimeout(90_000);

async function stable(page: import("@playwright/test").Page) {
  await page.waitForLoadState("networkidle");
  await page.evaluate(() => document.fonts.ready);
}

test("登录页明暗主题", async ({ page }) => {
  await page.goto("/login");
  await stable(page);
  await expect(page).toHaveScreenshot("login-light.png", {
    maxDiffPixelRatio: 0.02,
  });

  // 登录页无外壳开关，直接打 class（与 AppLayout 的 class 策略一致）。
  await page.evaluate(() => document.documentElement.classList.add("dark"));
  await stable(page);
  await expect(page).toHaveScreenshot("login-dark.png", {
    maxDiffPixelRatio: 0.02,
  });
});

test("工作台表格页与 Action 对话框明暗主题", async ({ page }) => {
  await mockSessionRestore(page);
  await page.goto("/workbench");
  await expect(page.getByRole("heading", { name: "项目目录" })).toBeVisible();
  await expect(page.getByRole("button", { name: "新增项目" })).toBeVisible();
  await stable(page);
  await expect(page).toHaveScreenshot("workbench-table-light.png", {
    maxDiffPixelRatio: 0.02,
  });

  await page.getByRole("button", { name: "切换明暗主题" }).click();
  await stable(page);
  await expect(page).toHaveScreenshot("workbench-table-dark.png", {
    maxDiffPixelRatio: 0.02,
  });

  // Action 对话框（暗色保持）。
  await page.getByRole("button", { name: "新增项目" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByLabel("名称")).toBeVisible();
  await stable(page);
  await expect(page).toHaveScreenshot("action-dialog-dark.png", {
    maxDiffPixelRatio: 0.02,
  });

  // 模态对话框期间背景不可交互：先关闭再切主题，然后重开拍亮色。
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "切换明暗主题" }).click();
  await page.getByRole("button", { name: "新增项目" }).click();
  await expect(page.getByRole("dialog").getByLabel("名称")).toBeVisible();
  await stable(page);
  await expect(page).toHaveScreenshot("action-dialog-light.png", {
    maxDiffPixelRatio: 0.02,
  });
});

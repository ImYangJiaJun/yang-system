import { AxeBuilder } from "@axe-core/playwright";
import { expect, test } from "@playwright/test";

import { mockSessionRestore } from "./support";

/**
 * 可访问性（ADR-5 §2.3）：核心页面 axe 零 critical/serious 违规 + 键盘可达性。
 * 对齐旧 e2e/accessibility.spec.ts 的阻断口径。
 */

function expectNoSeriousViolations(
  violations: Awaited<ReturnType<AxeBuilder["analyze"]>>["violations"],
) {
  const blocking = violations.filter((violation) =>
    ["critical", "serious"].includes(violation.impact ?? ""),
  );
  expect(
    blocking.map(
      (violation) =>
        `${violation.id}: ${violation.nodes.map((node) => node.target).join(", ")}`,
    ),
  ).toEqual([]);
}

test("登录页满足 WCAG AA 且可以纯键盘提交", async ({ page }) => {
  await page.route("**/api/v1/users/login", (route) =>
    route.fulfill({
      status: 401,
      contentType: "application/json",
      body: JSON.stringify({ code: 40101, message: "账号或密码错误" }),
    }),
  );
  await page.goto("/login");

  const results = await new AxeBuilder({ page })
    .withTags(["wcag2aa"])
    .analyze();
  expectNoSeriousViolations(results.violations);

  // 纯键盘：Tab 到帐号输入 → 输入 → Tab → 密码 → Enter 提交。
  await page.getByLabel("帐号").click();
  await page.keyboard.type("alice");
  await page.keyboard.press("Tab");
  await page.keyboard.type("wrong-password");
  await page.keyboard.press("Enter");
  await expect(page.getByText("账号或密码错误")).toBeVisible();
});

test("工作台表格页满足 WCAG AA", async ({ page }) => {
  await mockSessionRestore(page);
  await page.goto("/workbench");
  await expect(page.getByText("平台能力", { exact: true })).toBeVisible();

  const results = await new AxeBuilder({ page })
    .withTags(["wcag2aa"])
    .analyze();
  expectNoSeriousViolations(results.violations);
});

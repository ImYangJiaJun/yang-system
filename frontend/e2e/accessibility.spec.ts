import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Locator, type Page } from "@playwright/test";

const axeTags = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];

async function expectNoWcagViolations(page: Page, include?: string) {
  let builder = new AxeBuilder({ page }).withTags(axeTags);
  if (include) {
    builder = builder.include(include);
  }
  const results = await builder.analyze();
  expect(results.violations).toEqual([]);
}

async function tabTo(page: Page, target: Locator, limit = 12) {
  for (let index = 0; index < limit; index += 1) {
    if (
      await target.evaluate(
        (element) =>
          element === document.activeElement ||
          element.contains(document.activeElement),
      )
    ) {
      return;
    }
    await page.keyboard.press("Tab");
  }
  throw new Error(`键盘 Tab 顺序未在 ${limit} 步内到达目标`);
}

async function expectVisibleFocus(target: Locator) {
  await expect(target).not.toHaveCSS("outline-style", "none");
  await expect(target).toHaveCSS("outline-width", "3px");
}

async function serveAccessibleSession(page: Page) {
  await page.addInitScript(() => {
    sessionStorage.setItem("yang.account-identity", "user");
  });
  await page.route("**/api/v1/users/refresh", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: { access_token: "accessibility-token" },
      }),
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
          revision: "a".repeat(64),
          actions: [
            {
              operation_id: "demo.accessibility.load",
              title: "读取资料",
              description: "读取无障碍测试资料",
              method: "GET",
              path: "/api/v1/demo/accessibility",
              params: [],
              input_schema: { type: "object", properties: {} },
              output_schema: {
                type: "object",
                properties: {
                  name: { type: "string", title: "姓名" },
                },
              },
              request_media_type: "json",
              response_kind: "json",
              requires_auth: true,
            },
            {
              operation_id: "demo.accessibility.edit",
              title: "编辑资料",
              description: "修改无障碍测试资料",
              method: "POST",
              path: "/api/v1/demo/accessibility",
              params: [],
              input_schema: {
                type: "object",
                properties: {
                  name: {
                    type: "string",
                    title: "姓名",
                    description: "请输入姓名",
                  },
                },
                required: ["name"],
              },
              output_schema: { type: "object" },
              request_media_type: "json",
              response_kind: "json",
              requires_auth: true,
            },
          ],
          table_views: [],
          modules: [
            {
              module_id: "demo.accessibility",
              identity: {
                id: "user",
                title: "个人账户",
                icon: "person",
                order: 10,
              },
              title: "无障碍资料",
              description: "键盘与焦点验收",
              icon: "accessibility_new",
              order: 10,
              primary_action: "demo.accessibility.load",
              actions: ["demo.accessibility.edit"],
              action_presentations: [
                {
                  operation_id: "demo.accessibility.edit",
                  title: "编辑资料",
                  placement: "toolbar",
                  interaction: "form",
                },
              ],
              views: [],
            },
          ],
        },
      }),
    }),
  );
  await page.route("**/api/v1/demo/accessibility", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data:
          route.request().method() === "GET"
            ? { name: "无障碍用户" }
            : { updated: true },
      }),
    }),
  );
}

test("登录页满足 WCAG AA 且可以纯键盘提交", async ({ page }) => {
  await page.route("**/api/v1/users/login", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: { access_token: "keyboard-token" },
      }),
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
          revision: "b".repeat(64),
          actions: [],
          table_views: [],
          modules: [],
        },
      }),
    }),
  );

  await page.goto("/login");
  await expectNoWcagViolations(page);

  const username = page.getByLabel("帐号");
  const password = page.getByLabel("密码");
  await expect(username).toBeFocused();
  await expectVisibleFocus(username);
  await page.keyboard.type("keyboard-user");
  await page.keyboard.press("Tab");
  await expect(password).toBeFocused();
  await page.keyboard.type("keyboard-password");
  await tabTo(page, page.getByRole("button", { name: "登录" }));
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL("/roles");
});

test("角色与模块关键旅程满足 WCAG AA、键盘和焦点恢复", async ({ page }) => {
  await serveAccessibleSession(page);
  await page.goto("/roles");
  const roleButton = page.getByRole("button", { name: "选择个人账户角色" });
  await expect(roleButton).toBeVisible();
  await expectNoWcagViolations(page);
  await tabTo(page, roleButton);
  await expectVisibleFocus(roleButton);
  await page.keyboard.press("Enter");

  await expect(page).toHaveURL("/module/demo.accessibility");
  await expect(page.getByRole("heading", { name: "无障碍资料" })).toBeVisible();
  await expectNoWcagViolations(page);

  const trigger = page.getByRole("button", { name: "编辑资料" });
  await tabTo(page, trigger);
  await expectVisibleFocus(trigger);
  await page.keyboard.press("Enter");
  const dialog = page.getByRole("dialog", { name: "编辑资料" });
  await expect(dialog).toBeVisible();
  await expect(page.locator(".q-dialog__backdrop")).not.toHaveClass(
    /q-transition--/,
  );
  await expect(page.locator(".q-dialog__inner")).not.toHaveClass(
    /q-transition--/,
  );
  await expectNoWcagViolations(page, ".action-dialog-card");
  await expect(dialog.getByRole("button", { name: "关闭" })).toBeFocused();
  await expectVisibleFocus(dialog.getByRole("button", { name: "关闭" }));
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await expect(trigger).toBeFocused();
});

import { expect, test } from "@playwright/test";

import { mockSessionRestore } from "./support";

/**
 * 账号设置 E2E（演示后端 + route mock）：/account 页面在认证后可达，
 * 资料展示、修改密码成功后清空会话并跳转登录页。
 */

const ME_PAYLOAD = {
  code: 0,
  message: "成功",
  data: {
    id: 7,
    username: "alice",
    email: "alice@example.com",
    email_verified_at: 1_785_000_000,
    status: "active",
    created_at: 1_700_000_000,
    updated_at: 1_785_000_000,
  },
};

test("已认证用户可进入 /account 并查看资料", async ({ page }) => {
  await mockSessionRestore(page);
  await page.route("**/api/v1/users/me", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(ME_PAYLOAD),
    }),
  );

  await page.goto("/account");

  await expect(
    page.getByRole("heading", { name: "账号设置", level: 1 }),
  ).toBeVisible();
  await expect(page.getByText("alice", { exact: true })).toBeVisible();
  await expect(page.getByText("alice@example.com（已验证）")).toBeVisible();
});

test("修改密码成功后清空会话并回到登录页", async ({ page }) => {
  await mockSessionRestore(page);
  await page.route("**/api/v1/users/me", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(ME_PAYLOAD),
    }),
  );
  await page.route("**/api/v1/users/change-password", async (route) => {
    expect(route.request().postDataJSON()).toEqual({
      old_password: "old-password",
      new_password: "new-password-1",
    });
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "密码已修改",
        data: { relogin_required: true, immediate_convergence: true },
      }),
    });
  });

  await page.goto("/account");
  await page.getByLabel("当前密码").fill("old-password");
  await page.getByLabel("新密码（至少 10 位）").fill("new-password-1");
  await page.getByLabel("确认新密码").fill("new-password-1");
  await page.getByRole("button", { name: "修改密码" }).click();

  await expect(page).toHaveURL(/\/login/);
  await expect(
    page.getByText("凭据已变更，请使用新密码重新登录"),
  ).toBeVisible();
});

test("未认证访问 /account 重定向到登录页", async ({ page }) => {
  await page.route("**/api/v1/users/refresh", (route) =>
    route.fulfill({
      status: 401,
      contentType: "application/json",
      body: JSON.stringify({ code: 40102, message: "刷新会话 Cookie 缺失" }),
    }),
  );

  await page.goto("/account");

  await expect(page).toHaveURL(/\/login/);
});

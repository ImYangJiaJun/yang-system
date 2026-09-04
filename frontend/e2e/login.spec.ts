import { expect, test } from "@playwright/test";

/**
 * 登录/注册/重置 E2E（对齐旧 e2e/login.spec.ts 的行为断言）：
 * 默认入口、失败停留、成功进入、验证码注册、重置凭证、伪造 token 拒绝。
 */

test("默认入口是登录界面", async ({ page }) => {
  await page.goto("/");

  await expect(page).toHaveURL(/\/login/);
  await expect(page.getByRole("heading", { name: "用户登录" })).toBeVisible();
});

test("登录失败时停留在登录页且不保存凭据", async ({ page }) => {
  await page.route("**/api/v1/users/login", (route) =>
    route.fulfill({
      status: 401,
      contentType: "application/json",
      body: JSON.stringify({ code: 40101, message: "账号或密码错误" }),
    }),
  );

  await page.goto("/login");
  await page.getByLabel("帐号").fill("alice");
  await page.getByLabel("密码", { exact: true }).fill("wrong-password");
  await page.getByRole("button", { name: "登录" }).click();

  await expect(page).toHaveURL(/\/login/);
  await expect(page.getByText("账号或密码错误")).toBeVisible();
  await expect
    .poll(() => page.evaluate(() => sessionStorage.getItem("yang.token")))
    .toBeNull();
});

test("登录成功进入应用中心且 Token 不落 Web Storage", async ({ page }) => {
  await page.route("**/api/v1/users/login", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: { access_token: "access-token" },
      }),
    }),
  );

  await page.goto("/login");
  await page.getByLabel("帐号").fill("alice");
  await page.getByLabel("密码", { exact: true }).fill("correct-password");
  await page.getByRole("button", { name: "登录" }).click();

  // 演示 Catalog 无 identity：登录后直达应用中心。
  await expect(page).toHaveURL(/\/$/);
  await expect(
    page.getByRole("heading", { name: "应用中心", level: 1 }),
  ).toBeVisible();
  await expect
    .poll(() => page.evaluate(() => sessionStorage.getItem("yang.token")))
    .toBeNull();
  await expect
    .poll(() => page.evaluate(() => localStorage.getItem("yang.token")))
    .toBeNull();
});

test("注册必须先获取邮箱验证码并提交所有权证明", async ({ page }) => {
  await page.route(
    "**/api/v1/users/registration-email-verifications",
    async (route) => {
      expect(route.request().postDataJSON()).toEqual({
        email: "alice@example.com",
      });
      await route.fulfill({
        status: 202,
        contentType: "application/json",
        body: JSON.stringify({
          code: 0,
          message: "成功",
          data: { accepted: true, expires_in: 600, resend_after: 60 },
        }),
      });
    },
  );
  await page.route("**/api/v1/users/register", async (route) => {
    expect(route.request().postDataJSON()).toEqual({
      username: "alice",
      password: "correct-password1",
      email: "alice@example.com",
      email_code: "123456",
    });
    await route.fulfill({
      status: 201,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          id: 42,
          username: "alice",
          email: "alice@example.com",
          email_verified_at: 1_785_000_000,
        },
      }),
    });
  });

  await page.goto("/register");
  await page.getByLabel("邮箱", { exact: true }).fill("alice@example.com");
  await page.getByRole("button", { name: "发送验证码" }).click();
  await expect(page.getByText(/验证码将在 10 分钟内送达/)).toBeVisible();

  await page.getByLabel("帐号").fill("alice");
  await page.getByLabel("邮箱验证码").fill("123456");
  await page.getByLabel("密码", { exact: true }).fill("correct-password1");
  await page.getByLabel("确认密码").fill("correct-password1");
  await page.getByRole("button", { name: "创建账号" }).click();

  await expect(page).toHaveURL(/\/login\?registered=1/);
  await expect(page.getByText("账号已创建，请登录")).toBeVisible();
});

test("重置成功后清空旧会话且凭证不进入 URL", async ({ page }) => {
  const token = "a".repeat(64);
  await page.route("**/api/v1/users/reset-password", async (route) => {
    expect(route.request().postDataJSON()).toEqual({
      reset_token: token,
      new_password: "replacement-password",
    });
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "密码已重置",
        data: { relogin_required: true },
      }),
    });
  });

  await page.goto("/reset-password");
  await page.getByLabel("重置凭证").fill(token);
  await page.getByLabel("新密码", { exact: true }).fill("replacement-password");
  await page.getByLabel("确认新密码").fill("replacement-password");
  await page.getByRole("button", { name: "重置密码" }).click();

  await expect(page).toHaveURL(/\/login/);
  expect(page.url()).not.toContain(token);
  await expect(
    page.getByText("凭据已变更，请使用新密码重新登录"),
  ).toBeVisible();
});

test("伪造 Web Storage Token 且 Refresh Cookie 无效时保持未认证", async ({
  page,
}) => {
  await page.goto("/login");
  await page.evaluate(() => {
    sessionStorage.setItem("yang.token", "attacker-controlled-token");
    sessionStorage.setItem("yang.account-identity", "user");
  });
  // 终态 401（Cookie 缺失/被拒）才会清空会话存储；演示后端无 refresh 路由返回 404。
  await page.route("**/api/v1/users/refresh", (route) =>
    route.fulfill({
      status: 401,
      contentType: "application/json",
      body: JSON.stringify({ code: 40102, message: "刷新会话 Cookie 缺失" }),
    }),
  );

  await page.goto("/");

  await expect(page).toHaveURL(/\/login/);
  await expect
    .poll(() => page.evaluate(() => sessionStorage.getItem("yang.token")))
    .toBeNull();
  await expect
    .poll(() =>
      page.evaluate(() => sessionStorage.getItem("yang.account-identity")),
    )
    .toBeNull();
});

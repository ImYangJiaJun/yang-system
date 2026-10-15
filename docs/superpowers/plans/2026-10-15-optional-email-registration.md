# 注册不再强制邮箱 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用户名加密码即可注册；邮箱在账户设置里绑定后才能走邮件重置；没绑邮箱的由管理员在用户列表签发一次性凭证，用户自己设新密码。

**Architecture:** 注册契约删掉邮箱和验证码，插入时两列都写 NULL。绑定复用现有 `change_email`（它不看原邮箱）。后台用户列表是一条新的 `app_route` 页面，调用已有的 `list_users` 和 `admin_issue_password_reset`，不新增 Action。

**Tech Stack:** Rust 2021 / axum（经 yang-base）/ React 19 / Vite / Vitest。集成测试要真实 MySQL 与 Redis DB 15。

**Spec:** `docs/superpowers/specs/2026-10-15-optional-email-registration-design.md`

## Global Constraints

- 不新增「管理员直接写新密码」的接口。
- 不把公开找回从邮件链接改成六位验证码。
- 不拆注册验证码引擎；`POST /api/v1/users/registration-email-verifications` 保留。
- 不改 TOTP 的启用条件，也不改 `users` 表结构。
- 生产 Rust 禁止 `unwrap()` / `expect()`。每个 Action 文件恰好一个 `pub(super) async fn handle` 和一个 `pub(super) fn register`。
- 前端文案只改这一处涉及的句子；不引入新的 locale API。

## Review Focus

- 旧客户端仍提交 `email` + `email_code`：必须被 `deny_unknown_fields` 拒绝，不能静默丢掉再注册成功。
- 注册响应里 `email` 为 null：现有前端把「邮箱是非空字符串」当成成功条件，不改会把成功注册显示成失败。
- 未绑定邮箱调用公开找回：响应必须与「邮箱不存在」字节级同形，且不发信。
- 停用账号签发重置凭证：后端已拒绝，前端不得因为列表里看得到该行就提前放行。
- 绑定邮箱成功后会话被撤销：页面必须走现有「重新登录」，不能停在已失效的设置页。

---

### Task 1: 注册契约只收用户名和密码

**Files:**
- Modify: `src/addon/account/user/actions/register.rs`
- Modify: `src/addon/account/domain/repository.rs`（`insert_in_tx`，约 294 行）
- Test: `src/addon/account/user/actions/register.rs` 内 `registration_contract_requires_email_ownership_proof`

**Interfaces:**
- Consumes: `UserRepository::insert_in_tx` 现签名 `(ctx, tx, username, password_hash, email, email_verified_at)`。
- Produces: `insert_in_tx(ctx, tx, username, password_hash) -> Result<i64, BaseError>`。`RegisterInput` 只剩 `username`、`password`。

- [ ] **Step 1: 改失败测试**

把 `registration_contract_requires_email_ownership_proof` 改成：

```rust
#[test]
fn registration_contract_is_username_and_password_only() {
    let params = <RegisterInput as ParamInput>::params();
    let names = params
        .as_slice()
        .iter()
        .map(|param| param.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["username", "password"]);
    assert!(params.as_slice().iter().all(|param| param.required));

    let injected = serde_json::from_value::<RegisterInput>(serde_json::json!({
        "username": "alice",
        "password": "correct-horse-battery",
        "email": "alice@example.com",
        "email_code": "123456"
    }));
    assert!(injected.is_err(), "注册不再接受邮箱字段");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib registration_contract_is_username_and_password_only --locked`

Expected: FAIL，断言期望 `["username", "password"]`，实际仍是四个字段。

- [ ] **Step 3: 改契约与写入**

`RegisterInput` 删掉 `email`、`email_code` 两段。`handle` 删掉 `normalize_email`、`RegistrationEmailVerification::consume`、`current_unix_timestamp` 及其函数（确认本文件无其他调用者后再删）。唯一键冲突文案改为 `"用户名已被其他请求注册"`。

`insert_in_tx` 去掉 `email`、`email_verified_at` 参数，记录里不再 `set` 这两列（NULL 由列默许）。`handle` 的调用改成：

```rust
.insert_in_tx(&ctx, &mut transaction, &username, &password_hash)
```

`register.rs` 顶部不再使用的 `normalize_email`、`RegistrationEmailVerification`、`SystemTime` import 一并删掉。

- [ ] **Step 4: 再跑测试**

Run: `cargo test --lib registration_contract_is_username_and_password_only --locked`

Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add src/addon/account/user/actions/register.rs src/addon/account/domain/repository.rs
git commit -m "feat(account): 注册只收用户名和密码"
```

### Task 2: 注册页与注册客户端去掉邮箱

**Files:**
- Modify: `frontend/src/features/auth/api.ts`（`RegisteredUser`、`register`）
- Modify: `frontend/src/features/auth/pages/RegisterPage.tsx`
- Test: `frontend/tests/features/auth/api.test.ts`

**Interfaces:**
- Consumes: Task 1 的注册响应，`email` 与 `email_verified_at` 为 null。
- Produces: `register(username, password, signal?) -> Promise<{ id, username }>`。

- [ ] **Step 1: 改失败测试**

在 `api.test.ts` 里找到调用 `register` 的用例。请求体断言改为只含 `username`、`password`。成功响应用例改为服务端返回 `"email": null, "email_verified_at": null` 时，`register` 仍 resolve，且结果只有 `id` 与 `username`。

- [ ] **Step 2: 跑测试确认失败**

Run: `pnpm --dir frontend exec vitest run tests/features/auth/api.test.ts`

Expected: FAIL，现实现仍要求邮箱为非空字符串。

- [ ] **Step 3: 改客户端与页面**

`RegisteredUser` 改为 `{ id: number; username: string }`。`register` 只发 `{ username, password }`，成功条件去掉邮箱两段校验。

`RegisterPage.tsx` 删掉 `email`、`emailCode`、`sendingCode`、`resendRemaining` 状态、`sendCode`、重发计时 effect，以及邮箱和验证码两块 JSX。副标题改为「使用用户名和密码创建账号。」提交改为 `register(username.trim(), password)`。校验只留帐号长度、密码长度、两次密码一致。

- [ ] **Step 4: 再跑测试**

Run: `pnpm --dir frontend exec vitest run tests/features/auth/api.test.ts`

Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add frontend/src/features/auth/api.ts frontend/src/features/auth/pages/RegisterPage.tsx frontend/tests/features/auth/api.test.ts
git commit -m "feat(frontend): 注册页去掉邮箱验证码"
```

### Task 3: 设置页把空邮箱显示成绑定

**Files:**
- Modify: `frontend/src/features/account/AccountSettingsPage.tsx`（约 359 行 `emailText`，约 500 行邮箱区块）
- Test: `frontend/tests/features/account/` 下新增 `account-settings-email.test.tsx`（若目录已有设置页测试则加在原文件）

**Interfaces:**
- Consumes: `profile.email` 为 `string | null`。`requestChangeEmail`、`changeEmail` 签名不变。
- Produces: 无新导出。空邮箱时标题「绑定邮箱」、按钮「绑定邮箱」；有邮箱时保持「更换邮箱」。

- [ ] **Step 1: 写失败测试**

渲染设置页两次。`fetchCurrentUser` 第一次返回 `email: null`，断言标题为「绑定邮箱」、按钮文案为「绑定邮箱」、资料区含「未绑定邮箱」。第二次返回 `email: "a@example.com"` 且 `emailVerifiedAt` 为正数，断言标题为「更换邮箱」。

- [ ] **Step 2: 跑测试确认失败**

Run: `pnpm --dir frontend exec vitest run tests/features/account/account-settings-email.test.tsx`

Expected: FAIL，空邮箱时标题仍是「更换邮箱」。

- [ ] **Step 3: 改文案**

```tsx
const emailBound = Boolean(profile?.email && profile.emailVerifiedAt);
const emailHeading = emailBound ? "更换邮箱" : "绑定邮箱";
```

资料区沿用现有 `emailText`（空时已是「未绑定邮箱」）。邮箱区块的 `h2`、提交按钮、说明文案用 `emailHeading`：未绑定时说明改为「向邮箱发送一次性验证码，验证通过后即可用于找回密码」。`sendEmailCode` 与 `submitEmail` 不改。

- [ ] **Step 4: 再跑测试**

Run: `pnpm --dir frontend exec vitest run tests/features/account/account-settings-email.test.tsx`

Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add frontend/src/features/account/AccountSettingsPage.tsx frontend/tests/features/account/account-settings-email.test.tsx
git commit -m "feat(frontend): 未绑定邮箱时显示绑定入口"
```

### Task 4: 后台用户列表与签发重置凭证

**Files:**
- Modify: `src/addon/account/user/mod.rs`（`presentation`，约 68 行）
- Modify: `frontend/src/shell/routes.tsx`（在 `access/workspace` 路由之后加一条）
- Create: `frontend/src/features/account/UserAdminPage.tsx`
- Create: `frontend/src/features/account/admin-api.ts`
- Test: `frontend/tests/features/account/user-admin-page.test.tsx`
- Test: `frontend/tests/shell/routes.test.tsx`（该文件逐条钉住路由级 lazy 的 `hydrateFallbackElement`，新路由必须补一条）

**Interfaces:**
- Consumes: `GET /api/v1/users?page=&page_size=` 返回 `{ users: UserView[] }`，权限 `account.users.read`。`POST /api/v1/users/{id}/password-reset-tokens` 返回 `{ reset_token, expires_in }`，权限 `account.users.reset_credentials`。
- Produces: 路由 `/admin/users`。`listUsers(token, page)` 与 `issuePasswordReset(token, userId)`。

- [ ] **Step 1: 写失败测试**

`user-admin-page.test.tsx`：mock `listUsers` 返回一个启用用户。有 `account.users.reset_credentials` 时渲染「签发重置凭证」按钮；没有该权限时不渲染。点击后 `issuePasswordReset` 被以该用户 id 调用，返回的 `reset_token` 出现在对话框里，且页面上只有这一处。

`routes.test.tsx` 按现有写法补一条：`/admin/users` 是 lazy 路由且带 `hydrateFallbackElement`。

- [ ] **Step 2: 跑测试确认失败**

Run: `pnpm --dir frontend exec vitest run tests/features/account/user-admin-page.test.tsx tests/shell/routes.test.tsx`

Expected: FAIL，路由与页面都不存在。

- [ ] **Step 3: 后端导航与页面**

一个 Module 只有一条 `presentation`，而 `compile.rs` 的 `resolve_owned_action` 要求 `primary_action` 必须属于当前 Module（`reference.module() != &module.name` 直接 `BuildError`）。所以不能把 `account.user.list_users` 挂到另一个模块的投影上。

在 `src/addon/account/mod.rs` 的 `build_addon` 里追加一个无表模块，导航只靠 `app_route`，不设 `primary_action`。页面自己调 `GET /api/v1/users`：

```rust
ModuleSpec::new(ModuleName::new("account.admin_users")?)
    .presentation(
        ModulePresentationSpec::new(crate::addon::admin_identity(), "用户", "admin_users")
            .app_route("/admin/users")
            .description("查看用户并为忘记密码的账号签发重置凭证")
            .order(30),
    )
```

无 `primary_action` 的模块是否仍进入导航，以 `cargo check` 为准。不进导航就把 `list_users` 的 `register` 拆一个只含 `.route` 与 `.permissions` 的空壳到这个新模块——不要这么做。改为把 `admin_identity` 加到 `account.user` 的现有投影上，会把「账号设置」也拖进系统管理，更差。两条都不通时停下来问，不要自己换方案。

`admin-api.ts`：

```ts
export async function listUsers(token: string | undefined, page: number) {
  // GET /api/v1/users?page=&page_size=20，解析 users[]
}
export async function issuePasswordReset(token: string | undefined, userId: number) {
  // POST /api/v1/users/${userId}/password-reset-tokens，解析 reset_token 与 expires_in
}
```

`UserAdminPage.tsx`：进入时 `listUsers`。每行显示用户名、邮箱（空则「未绑定」）、状态。按钮仅在 `useUiCatalog()` 报有 `account.user.admin_issue_password_reset` 时渲染。点击后展示返回的 `reset_token`，关闭对话框即从 state 清掉。失败用 toast，不吞错误。

`routes.tsx` 在 `access/workspace` 后加：

```tsx
{
  path: "admin/users",
  lazy: async () => ({
    Component: (await import("@/features/account/UserAdminPage")).default,
  }),
  hydrateFallbackElement: <RouteFallback />,
},
```

- [ ] **Step 4: 再跑测试**

Run: `pnpm --dir frontend exec vitest run tests/features/account/user-admin-page.test.tsx tests/shell/routes.test.tsx`

Expected: PASS

再跑 `cargo check --all-targets --locked`。无 `primary_action` 的模块若没进导航，按 Step 3 末尾停下来问，不要改挂法。

- [ ] **Step 5: 提交**

```bash
git add src/addon/account/mod.rs frontend/src/shell/routes.tsx frontend/src/features/account/UserAdminPage.tsx frontend/src/features/account/admin-api.ts frontend/tests/features/account/user-admin-page.test.tsx frontend/tests/shell/routes.test.tsx
git commit -m "feat(account): 后台用户列表签发重置凭证"
```

### Task 5: 文档与注册集成测试

**Files:**
- Modify: `docs/contracts/REGISTRATION_EMAIL_VERIFICATION.md` 开头两节
- Modify: `AGENTS.md` 项目概览里「邮箱验证码注册」那句
- Modify: `tests/registration_email_integration.rs`（`registration_body` 与 `registration_email_code_is_private_bounded_and_single_use` 后半）

**Interfaces:**
- Consumes: Task 1 的注册体只有用户名和密码。
- Produces: 无。

- [ ] **Step 1: 改集成测试里的注册**

`registration_body` 改为只含 `username` 与 `password`，删掉 `email`、`code` 参数。`registration_email_code_is_private_bounded_and_single_use` 里「正确验证码完成注册」那段改成：无邮箱字段注册成功，响应 `email` 为 null，数据库 `email` 与 `email_verified_at` 都是 NULL。验证码本身的冷却、Redis 不落原文、已注册邮箱抑制投递仍保留——它们测的是 `request_registration_email`，与注册解耦。

同一文件里其余把 `registration_body` 当「带验证码注册」用的调用，逐个改成无邮箱注册，或删掉「验证码错误则注册失败」这类已不成立的断言。

- [ ] **Step 2: 跑集成测试**

```powershell
$env:YANG_SYSTEM_TEST_DATABASE_URL = "mysql://root:yang-local@127.0.0.1:3306/yang_system_test"
$env:YANG_SYSTEM_TEST_REDIS_URL = "redis://127.0.0.1:6379/15"
cargo test --test registration_email_integration --locked -- --ignored --test-threads=1
```

Expected: PASS。没起依赖就先说明，不把跳过当成通过。

- [ ] **Step 3: 改两份文档**

`REGISTRATION_EMAIL_VERIFICATION.md` 第一段改为：公开注册不再消费验证码；验证码契约仍服务换绑与其他入口。删掉「register 必须提交 email 与 email_code」那条，改成「register 只收 username 与 password，两列写入 NULL」。

`AGENTS.md` 把「邮箱验证码注册」改为「用户名密码注册，邮箱在账户设置里可选绑定」。

- [ ] **Step 4: 提交**

```bash
git add docs/contracts/REGISTRATION_EMAIL_VERIFICATION.md AGENTS.md tests/registration_email_integration.rs
git commit -m "docs(account): 注册契约改为用户名密码"
```

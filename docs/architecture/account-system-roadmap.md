# 账户系统补全路线图（通用业务底座定位）

> - 文档性质：面向实施的设计方案与路线图，不表示下述能力已经完成
> - 系统定位：**yang-system 是后续业务开发的通用系统底座**，不是框架参考骨架
> - 依据：2026-09-05 对 `src/addon/account/`、`src/infrastructure/`、`frontend/src/`、`crates/yang-base` 的源码核实，以及 Ory Kratos / ZITADEL / Keycloak / Logto / better-auth / SuperTokens 等成熟方案的横评
> - 关联文档：`docs/assessments/2026-07-30-account-authz-first-principles-review.md`（账号与授权体系评审）、`docs/architecture/session-ttl.md`、`docs/architecture/authorization-writers.md`

## 实施进度（按阶段更新）

| 阶段 | 状态 | 说明 |
|---|---|---|
| A-0 三阶段发布开关 | ✅ 已完成 | 示例配置 `issue_refresh_credential_version = true`（`config.example.toml:105`、`src/config/mod.rs:878`） |
| A-1 自助密码找回 | ✅ 已完成 | `request_password_reset.rs` + `repository.rs::insert_issued`（2026-09-05 提交 40e5904） |
| A-2 登录等时校验 | ✅ 已完成 | `PasswordEngine::verify_or_dummy` + `login.rs` 接入（2026-09-06 提交 bc808b2） |
| A-3 前端账号中心 | ✅ 已完成 | `frontend/src/features/account/` + `/account` 路由 + 侧边栏入口（提交 c803071 之后） |
| A-4 修改用户名 | ✅ 已完成 | `POST /api/v1/users/change-username`（Step-up + 双版本 + Outbox + 审计） |
| A-5 OpenAPI step-up 快照 | ✅ 已完成 | `build_metadata_app` 内存 proof 存储装配 StepUpServices（提交 bc808b2） |
| B-1 邮箱换绑 | ✅ 已完成 | `[email.change]` 独立验证码段 + `change_email.rs`/`request_change_email.rs` + 前端区块 |
| B-2 邮箱登录 | ✅ 已完成 | `find_credentials_by_email` + login 归一化按 `@` 分派，限流键沿用归一化标识 |
| B-3 密码策略 | ✅ 已完成 | 内置弱密码字典 + 禁止与用户名相同（`policy.rs::WEAK_PASSWORDS`） |
| C-1 会话持久化 | ✅ 已完成 | `user_session` 表 + claims `session_id`（登录生成/refresh 继承）+ 60s 节流 + 设备列表/逐台撤销 |
| C-2 登录历史 | ✅ 已完成 | `login_event` 表 + 成功/失败粗粒度记录 + `GET /users/security-events` |
| C-3 新设备登录提醒 | ✅ 已完成 | `NewDeviceEmailSender` SMTP 实现，best-effort 不阻塞登录 |
| D 管理动作 | ✅ 已完成 | admin_disable/enable + 管理签发重置凭证 + 用户列表（`.permissions` + Step-up + 审计） |
| E-1 TOTP MFA | ✅ 已完成 | `[security.totp]` AEAD 密钥域 + users 三列 + setup/activate/deactivate Action + 登录两段式（密码通过后返回 `SecondFactorRequired` 进入验证码阶段，错码返参数错误；密码错误仍统一 `InvalidPassword` 防枚举）+ Step-up 第二因子强制 + 恢复码单次消费；deactivate 需登录 + Step-up（已激活账号须同时出示第二因子），停用即清空密钥/恢复码并全端失效；认证器不可用时登录第二因子可改用 `[email.mfa]` 备用邮箱验证码（等时密码重验防枚举、独立密钥域、单次消费，框架侧新增 `VerificationCodeSender`/`request_via` 投递端口）；前端交互面（登录两段式弹窗含邮箱验证码切换、Step-up 对话框 mfa_code、账号中心设置弹窗二维码/密钥/恢复码回显与关闭入口）已补齐 |
| E-2 账号删除 | ✅ 已完成 | 匿名化（username 改写 + email 置 NULL + status=deleted + 双版本）+ FK 前置清理 |
| E-3 OIDC 端口 | ✅ 已完成 | `domain/oidc.rs::ExternalIdentityProvider` 端口定义（不建表不接 Client） |

> **外键现状修正**：路线图 3.2 第 10 条「无外键」已不成立——`src/infrastructure/schema.rs:187-198`
> 声明了 `fk_password_reset_token_user` 与 `fk_password_reset_token_requested_by` 两条外键，
> E-2 账号删除/匿名化必须先清理该用户的重置凭证行。


## 一、定位与设计前提

### 1.1 定位修正带来的前提变化

| 设计前提 | 骨架定位（旧，已废止） | 通用底座定位（现行） |
|---|---|---|
| 用户规模 | 少量控制台操作员 | 会增长，可能有终端用户 |
| 缺失能力的处置 | 等真实需求出现再做 | 核心闭环必须齐备，外围能力预留扩展点 |
| 管理动作 | 设计上无管理员，一切自助 | 需要运营/客服类管理动作，但不做「上帝账号」 |
| 外部身份 | 不做 OAuth/OIDC | 预留 IdP 端口，按需接 SSO |
| 账号删除 | 软停用即可 | 需要定义完整的删除/匿名化路径 |
| MFA | 可选 | 必做，只是排期问题 |

### 1.2 保留不变的硬约束

以下不是骨架简化，而是架构资产，新能力必须遵守同一套契约：

- **无「最终管理员」不变量**：账户管理权限经 `access` Addon 的 grants 授予特定身份，实现「有管理动作、无超级管理员」，不引入 Keycloak 式 admin 账号。
- 双版本失效（`authz_version` / `credential_version`）+ Outbox 传播，writer 契约见 `docs/architecture/authorization-writers.md`。
- 敏感操作 Step-up + append-only 审计（`docs/contracts/AUDIT.md`）。
- 防枚举统一响应、验证码/令牌只存摘要、原子单次消费。
- 声明式 Schema 驱动（禁 SQL 迁移文件，`docs/contracts/SCHEMA.md`）；一 Action 一文件（`python scripts/check_architecture.py` 门禁）。
- 资源经 `ToolsBuilder` 显式持有，禁止进程级单例。

## 二、第一性原理：账户系统要回答的问题

账户系统只回答四个问题，外加一条横切约束：

1. **你是谁**（身份建立）——注册、身份标识（username/email）、标识的验证与换绑。
2. **怎么证明是你**（认证）——凭据校验、强度、找回与轮换、第二因子。
3. **证明的有效期**（会话）——签发、续期、可见性、精确撤销。
4. **如何体面结束**（生命周期终结）——停用、删除、凭据失效的传播。

横切约束：**对抗滥用**——枚举、撞库、爆破、会话窃取，且每一步留审计。

「完整」的判定标准：四问 + 一横切都有**闭环**——有创建端必有消费端，有写入必有失效传播，有敏感操作必有 Step-up + 审计。

## 三、现状盘点（源码核实，2026-09-05）

### 3.1 已有能力

| 能力 | 状态 | 关键位置 |
|---|---|---|
| 邮箱验证码注册 | ✅ 防枚举统一响应、摘要入 Redis、原子消费 | `user/actions/request_registration_email.rs:29`、`register.rs` |
| 邮箱归一化 | ✅ 框架 `normalize_email` | `register.rs:52` |
| 用户名+密码登录 | ✅ Argon2（框架 `PasswordEngine`） | `user/actions/login.rs` |
| Refresh Cookie 轮换 | ✅ host-only/HttpOnly/SameSite=Strict + 同源校验 | `domain/context.rs:31-34` |
| 双版本失效 + Outbox | ✅ MySQL 事实源 + Redis 短 TTL 单调缓存 | `domain/authz_version.rs`、`infrastructure/authorization/` |
| Step-up 重认证 | ✅ challenge→proof 一次性、Redis 原子消费 | `infrastructure/authorization/step_up.rs` |
| 密码重置消费端 | ✅ 摘要入库、行锁消费、同用户其他凭证作废 | `domain/password_reset/repository.rs` |
| 改密/停用/全量登出 | ✅ 但受三阶段开关门控（见 4.2） | `user/actions/change_password.rs` 等 |
| 限流 | ✅ Redis Lua 原子计数，IP+身份双维度 | yang-base `action/auth/rate_limit.rs` |
| 审计 | ✅ append-only，含 Step-up 成功/拒绝 | `infrastructure/audit/` |
| 前端会话协议 | ✅ 内存 token、并发去重刷新、跨标签广播、428→proof 重放 | `frontend/src/engine/session/` |
| 框架预留 | ✅ 限流器已有 `AuthOperation::PasswordResetCreate` | yang-base `rate_limit.rs:76` |

### 3.2 缺失能力（差距清单）

1. **密码找回签发端**：`password_reset/repository.rs` 无任何 INSERT 路径（只有 parse/fingerprint/lock/consume），且无 Action 写入 `password_reset_token` 表——该流程当前是断链。
2. **前端账号中心**：`GET /users/me`、`POST /users/change-password`、`POST /users/disable` 均已存在，但前端没有任何调用点、没有 `/account` 路由。
3. **登录时序枚举旁路**：`login.rs:49-62` 用户不存在时跳过 Argon2 直接返回 `InvalidPassword`，响应时间差泄露用户名存在性。
4. **OpenAPI 快照缺 step-up 端点**：`step_up.rs:207` 在未配置 `StepUpManager` 时不注册，而元数据导出路径 `build_metadata_app`（`src/app.rs:47-54`）恰好不传 StepUpServices，导致前端已依赖的 `POST /users/step-up/complete` 不在契约快照中。
5. 邮箱换绑、邮箱登录、修改用户名：均无 Action。
6. 会话可见性：无会话列表、无单设备撤销；logout 语义是全量撤销。
7. 登录历史：登录成功/失败只走 `TracingAuditHook`（tracing，不落库），无用户可查询的安全事件面。
8. MFA/TOTP：框架与应用均无。
9. 管理动作：无管理停用、管理签发重置凭证、用户列表（`password_reset_token.requested_by_user` 列表明设计已预留）。
10. 账号删除/匿名化：无（有利事实：`infrastructure/schema.rs` 全文无外键约束，删除路径不被 FK 阻塞）。
11. 密码策略：仅长度 10-128，无弱密码字典。

## 四、成熟方案共识与本系统取舍

调研 2026 年开源认证方案横评（Ory Kratos / ZITADEL / Keycloak / Authentik / Logto / better-auth / SuperTokens）后的共识对照：

| 共识做法 | 本系统现状 |
|---|---|
| 注册/找回防枚举统一响应 | ✅ 已有 |
| 验证码/token 只存摘要、原子单次消费 | ✅ 已有 |
| Refresh 轮换 + 凭据版本失效 | ✅ 且做到双版本分级 |
| 敏感操作 Step-up | ✅ 已有 |
| Argon2 + 并发收敛 | ✅ 框架能力 |
| 会话可见（设备列表 + 逐个踢出） | ❌ 缺失 |
| 自助密码找回 | ⚠️ 断链（只有消费端） |
| 账号中心（资料/改密/换绑/停用） | ❌ 前端缺失 |
| MFA（TOTP 底线，Passkey 进阶） | ❌ 缺失 |
| 安全事件用户可见 | ⚠️ 有审计表，无查询面 |

**结论：底层安全原语已达到甚至超过多数开源方案，缺的不是安全深度，而是闭环和可见性。方案重点不是换架构，而是补环。**

### 明确不做（与底座定位无关、无场景驱动的复杂度）

- **自建 OIDC Provider / 接入外部 IdP 产品**：与本项目定位根本冲突；未来 SSO 需求的正确姿势是作为 OIDC **Client** 对接外部 IdP（阶段 E 预留端口）。
- **Passkey/WebAuthn**：Rust 侧 webauthn-rs 虽成熟，但前端平台差异与恢复流程复杂度高，TOTP 落地后再评估。
- **邀请制/注册审批/图形验证码实现**：无场景；CAPTCHA 只预留端口（公开注册放量后再接）。
- **密码历史检查**：对齐 NIST 800-63B，收益低、体验差。

## 五、分阶段方案

### 阶段 A：闭环与账号中心

**A-0（前置，发布决策）**：确认并按 README 三阶段计划打开 `security.issue_refresh_credential_version`。
该开关经 `credential_mutations_enabled()`（`domain/context.rs:87-89`）门控改密/重置/停用/登出的**注册**，关闭时这些 Action 不存在，阶段 A 的账号中心将失去后端。示例配置默认为 `false`（`config/mod.rs:816`）。

**A-1 自助密码找回（修断链）**：
- `domain/password_reset/repository.rs` 新增签发函数（32 字节随机 token、SHA-256 摘要入库、`requested_by_user` 可空化——自助场景无请求者）。
- 新增 `POST /api/v1/users/request-password-reset`（public、202、邮箱存在才投递但响应无差别防枚举）；限流复用框架 `AuthOperation::PasswordResetCreate`（`rate_limit.rs:76`）；TTL 用既有 `security.password_reset_ttl_seconds`。
- 邮件含 `https://<host>/reset-password?token=...` 链接；前端 `ResetPasswordPage` 已支持 `?token=` 预填，零改动对接。
- 消费成功后的版本递增 + Outbox + 审计沿用现有 `reset_password.rs` 契约。

**A-2 登录等时校验（堵时序枚举）**：
用户 miss 时用固定 dummy PHC 哈希执行一次等时 Argon2 校验（`PasswordEngine` 加端口或应用侧内置常量），消除 `login.rs:49-62` 的响应时间差。限流维度不变。

**A-3 前端账号中心**：
- 新增 `features/account/`：`AccountSettingsPage`（资料展示、修改密码、修改用户名、停用账号入口）；路由 `/account` 挂 `RequireAuth`；`AppLayout` 侧边栏加「账号设置」入口。
- 改密成功后调用既有 `requireCredentialRelogin()` 传播；停用走 `SessionController.disableAccount()`（已实现 428→Step-up 重放）。
- 配套 Vitest（镜像 `tests/features/account/`）+ 1-2 条 Playwright。

**A-4 修改用户名 Action**：`POST /api/v1/users/change-username`（需登录，Step-up 保护），复用 `normalize_username` 策略与唯一约束，版本递增 + 审计；注意登录限流键随用户名变化。

**A-5 OpenAPI 快照修复**：让元数据导出路径能注册 step-up spec。实施前须先验证 `StepUpManager` 构造是否依赖 Redis（决定是构造临时 manager 还是拆分 spec/handler 注册）；修复后重跑 `python scripts/dump_openapi.py` 并提交两个生成物。

### 阶段 B：身份标识与凭据生命周期

- **B-1 邮箱换绑**：`POST /api/v1/users/change-email`（需登录 + Step-up），新邮箱验证码走框架验证码机制但**命名空间与注册验证码隔离**；事务内更新 email + 递增 `authz_version` + Outbox + 审计。前端账号中心加「更换邮箱」区块。
- **B-2 邮箱登录**：`UserRepository` 增加 `find_credentials_by_email`，`login.rs` 标识归一化后按 `@` 分派；限流键沿用归一化标识，防止经邮箱维度绕过用户名维度。
- **B-3 密码策略**：`domain/policy.rs` 增加常见弱密码字典（内置名单，不引外部服务）、禁止与用户名相同；不做密码历史。

### 阶段 C：会话可见性与登录安全

**C-1 会话持久化（本方案唯一的新架构决策点）**：

> ⚠️ **关键设计约束**：Refresh 是轮换制，jti 每次刷新都变。会话记录**不能以 jti 为键**，否则「踢出这个设备」在下一次轮换后失效。

- Token claims 增加跨轮换稳定的 `session_id`（登录时生成，refresh 时继承）；实施前必须验证框架 `RefreshClaimsResolver` 端口支持自定义 claims 透传。
- 新表 `user_session`（代码声明）：`session_id`（主键）、`user_id`、当前 `jti`（随轮换更新）、`created_at`、`last_seen_at`、`ip`、`user_agent`、`revoked_at`。
- **写放大控制**：`last_seen_at` 节流更新（同一 session 60s 内不重复写）或经 Worker 异步落；refresh 是热路径，受 `tests/refresh_load_benchmark.rs` 零错误基准守护。
- Action：`GET /api/v1/users/sessions`（列出活跃会话，标记当前设备）、`POST /api/v1/users/sessions/revoke`（按 `session_id` 撤销，Step-up + jti 黑名单 + 行标记 + 审计）。「退出全部设备」语义不变。
- 会话并发策略：默认「不限数量 + 可见可踢」，作为显式决策记录于此。
- 前端账号中心加「登录设备」区块。

**C-2 登录历史（独立表，不从 audit_event 投影）**：
登录事件不落审计库（保留期清理会牵连用户可见历史）。新建 `login_event` 表（user_id、occurred_at、ip、user_agent、result、failure_reason 粗粒度），自带保留策略；`GET /api/v1/users/security-events` 供用户自查。
同时接线框架 `record_failure`/`clear_failures` 失败计数（login 当前只用窗口计数）。

**C-3 异常登录通知**：新设备（会话表无历史指纹）登录成功后异步邮件提醒，best-effort，不阻塞登录路径。

### 阶段 D：管理动作（grants 化，无上帝账号）

- `POST /api/v1/users/{id}/disable` 与 `POST /api/v1/users/{id}/enable`（状态双向流转，复用单一 writer 端口递增版本 + Outbox）。
- `POST /api/v1/users/{id}/password-reset-tokens`：管理签发重置凭证（启用 `requested_by_user` 列的既定语义，凭证只回显一次）。
- `GET /api/v1/users`（分页列表，email 字段遵循既有 system 角色可见性约束）。
- 全部走既有 permission 校验 + Step-up + 审计；权限经 access grants 授予运营身份。

### 阶段 E：第二因子、删除与外部身份

**E-1 TOTP MFA**（阶段 A–C 之后最值得投入的一项，涉及 yang-base 扩展，注意跨仓库推送顺序：先 lib_yang 后 yang-system）：
- 选型：现成 crate（`totp-lite`/`oath`），禁止自写 HOTP/TOTP。
- 框架扩展：`LoginAction` 支持「部分认证 → 第二因子挑战」两阶段状态（最大工作量点）；`AuthOperation` 增加 TOTP 限流维度。
- 密钥存储：`users.totp_secret` 用 **AEAD 加密**——这是独立密钥域，不是 token keyring（HMAC 签名钥）的复用；新增配置项 + 启动密钥隔离校验 + 同步 `docs/contracts/CONFIGURATION.md`。
- 流程：`POST /users/mfa/totp/setup`（生成 secret + otpauth:// URI，未激活）→ `POST /users/mfa/totp/activate`（验码激活 + 版本递增 + 审计 + 签发一次性恢复码，恢复码摘要入库单次消费）。
- **验收条件（易漏）**：启用 TOTP 的账号执行 Step-up 时必须同时要求第二因子，否则高权限操作防护降级回单因子。

**E-2 账号删除**：匿名化路径——username 改写为 `deleted_<id>`（保唯一约束）、email 置 NULL（释放可再注册）、`status=deleted`、双版本递增全端失效；无外键阻塞（已核实 `schema.rs` 无 FK），审计外键完整性天然保持。第一个真实业务上线前落地。

**E-3 OIDC 端口抽象（降级项）**：只在 `domain/` 落 `ExternalIdentityProvider` 端口定义；`user_identity` 表（provider × external_sub × user_id）等第一个真实 SSO 需求出现时再声明——无消费者先建表不符合声明式 Schema 的演进纪律。

## 六、实施纪律

- 每个新 Action 用 `python scripts/new_action.py` 生成脚手架；改完 `actions/` 必跑 `python scripts/check_architecture.py`。
- 契约变更后重跑 `python scripts/dump_openapi.py` 并提交 `frontend/contracts/openapi.json` 与生成类型两个产物。
- 提交前 `python scripts/run_ci.py quick`；推送前 `python scripts/run_ci.py full`；真实依赖行为补 `run_ci.py integration` 对抗性测试（防枚举、单次消费、限流维度、时序无关响应）。
- 涉及 yang-base 的改动（E-1、可能的 A-2 框架端口）遵守跨仓库推送顺序：先推 lib_yang 再推 yang-system。
- 新增配置项（AEAD 密钥域等）必须同步 `config.example.toml` 与 `docs/contracts/CONFIGURATION.md`，且不得与既有 keyring 复用。

## 七、开放决策点

1. **管理面的形状**：阶段 D 按「grants 授权 + Step-up + 审计」实现、保持无超级管理员不变量；若未来有专职运营团队，再评估是否增加独立管理控制台视图（同套 Action，不同身份投影）。
2. **用户模型**：若未来业务出现「同一批人既是员工又是客户」或 B 端企业租户，E-3 的 `user_identity` 抽象需提前到阶段 C 之前；否则按现路线推进。

## 八、审查记录（方案自审结论）

本方案经一轮对照源码的自审，修正了以下问题（保留于此供后续追溯）：

| 发现 | 修正 |
|---|---|
| 会话表以 jti 为键会在轮换后失效 | 改为稳定 `session_id` 为键，jti 随轮换更新（C-1） |
| 阶段 A 依赖三阶段开关，开关关闭时 Action 不注册 | 增加 A-0 前置发布决策 |
| 密码找回断链比初判更深（repository 无签发函数） | A-1 工作量含 repository 签发函数 |
| 登录时序枚举旁路 | 增加 A-2 等时校验 |
| 「登录历史从 audit_event 投影」不成立（登录不落审计库 + 保留期耦合） | 改为独立 `login_event` 表（C-2） |
| TOTP 启用后 Step-up 降级风险 | 写入 E-1 验收条件 |
| TOTP secret「复用 keyring」表述错误 | 明确为独立 AEAD 密钥域（E-1） |
| OIDC 先建表有 YAGNI 风险 | 降级为仅端口抽象（E-3） |

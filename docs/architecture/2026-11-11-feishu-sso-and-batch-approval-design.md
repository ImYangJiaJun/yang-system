# 飞书登录与批量审批设计方案

> 状态：待实施（2026-11-11）
> 范围：飞书身份登录本系统（SSO）、首次登录选择绑定已有账号或新建账号、已有账号绑定飞书、批量审批（用户态 API）
> 关联文档：
> `docs/architecture/2026-10-13-feishu-user-identity-oauth-design.md`（现有单管理员飞书 OAuth）、
> `docs/architecture/account-system-roadmap.md`（账户体系路线图 E-3 外部身份端口）、
> `docs/architecture/multi-factor-login.md`（MFA 语义）、
> `docs/contracts/{AUTHZ_GRANTS,SCHEMA,CONFIGURATION,AUDIT,SESSION_AUTHORITY}.md`。
>
> 本文是**设计方案**，不表示相关能力已经实现。所有「待真机验证」条目在 Phase 0 逐条打勾后才能进入编码。

---

## 0. 摘要

本方案做两件事，共用一条飞书用户身份链：

1. **飞书登录（SSO）**：用户在 `/login` 点「飞书登录」，经飞书 OAuth 授权后登录本系统。首次登录且没有绑定关系时，进入**账号选择页**：绑定已有系统账号（账号密码 + MFA），或新建系统账号（用户自行输入用户名和密码，与正常注册一致）；已有用户也可以随时在账号设置里绑定飞书。系统会话仍是现有的 access/refresh JWT + `user_session`，不新增第二套会话。
2. **批量审批**：用户登录后进入 `/batch-approval`，按审批定义分组查看自己的飞书待办，批量执行同意 / 拒绝 / 转交 / 退回 / 加签。所有审批读取与写入都使用**用户自己的 `user_access_token`**，服务端不引入 tenant token 的审批数据权限。

关键新增：

| 层 | 新增 |
|---|---|
| 数据 | `user_identity`（账号 ↔ 外部身份）、`feishu_user_credential`（每用户飞书令牌）、`feishu_batch_job` / `feishu_batch_job_item`（批量任务与逐条结果） |
| 后端 | `/api/v1/auth/feishu/*`（登录/回调/账号选择页/绑定时用密码/MFA）、`/api/v1/users/me/identities/*`（绑定管理）、`/api/v1/feishu/batch-approval/*`（列表/详情/用户搜索/任务） |
| 前端 | `/login` 飞书按钮、`/login/feishu/choose`（首次登录选择绑定/新建）、`/login/feishu/mfa`（飞书登录的第二因子）、`/batch-approval`、账号设置「飞书账号」区块 |
| 权限 | 飞书用户 scope：`approval:task:read`、`approval:task:write`、`approval:instance:read`、`contact:user:search`、`contact:contact.base:readonly`、`offline_access`；不新增 tenant 审批权限 |

**模块落点**：SSO 放 feishu addon 新增的 `sso` module（它已持有飞书 OAuth 传输/换码/user_info）；account 域只导出一个窄端口（查找/建号/绑定/解绑/签发会话），避免 account → feishu 反向依赖。`src/app.rs` 当前先建 feishu、后建 account（`100-115` / `118-126`），实施时要调整为「先构造 account 端口，再注入 feishu」。

**三条不可退让的安全不变量**：

- 新建账号路径（选择页「新建」）**不得**触发首个账号的 `system_owner` 引导，也不得授予任何管理权限；
- 已激活 TOTP 的账号经飞书登录时**必须**继续要求第二因子，飞书身份只替换第一因子；
- 批量写操作**不自动重试**，超时标记 `unknown` 并以实例详情核对，避免重复审批。

---

## 1. 目标与非目标

### 1.1 目标

- 员工用飞书身份登录本系统，无需先由管理员创建账号。
- 首次飞书登录无绑定时，由用户选择绑定已有账号或新建账号；新建时用户自行设置用户名和密码（与正常注册一致），已有账号可显式绑定/解绑飞书身份。
- 登录后可使用批量审批：按审批定义分组、查看任务信息、批量同意/拒绝/转交/退回/加签。
- 批量审批只以用户本人身份操作；服务端不持有可越权的租户审批令牌。
- 复用现有系统会话、权限、审计、限流、MFA 与前端会话协议，不另造一套。

### 1.2 非目标

- 不做多 IdP 产品化（只接飞书；`ExternalIdentityProvider` 端口保留，若出现第二个 IdP 再抽公共层）。
- 不做多飞书租户（单租户；`tenant_key` 仅作校验与记录）。
- 不做飞书审批的发起、定义管理、模板配置（现有派发/回写功能不变）。
- 不做三方审批、条件表单的完整渲染；v1 对这类任务只展示并提示单独处理。
- 不做自动把飞书邮箱写进 `users.email` 并视为已验证（见 D5）。
- 不做无感自动建号：首次登录必须显式选择绑定或新建；新建时由用户自行输入用户名和密码。
- 不做移动端专门适配。

---

## 2. 事实基础

### 2.1 飞书审批有两套并存的 API

| 能力 | 用户态（本方案采用） | 经典租户态（不采用，仅作降级备选） |
|---|---|---|
| 待办列表 | `GET /open-apis/approval/v4/tasks?topic=1` | `POST /open-apis/approval/v4/tasks/search` |
| 实例详情 | `GET /open-apis/approval/v4/instances/detail` | `GET /open-apis/approval/v4/instances/:instance_id` |
| 同意 | `POST /open-apis/approval/v4/tasks/pass` | `POST /open-apis/approval/v4/tasks/approve` |
| 拒绝 | `POST /open-apis/approval/v4/tasks/refuse` | `POST /open-apis/approval/v4/tasks/reject` |
| 转交 | `POST /open-apis/approval/v4/tasks/forward` | `POST /open-apis/approval/v4/tasks/transfer` |
| 退回 | `POST /open-apis/approval/v4/tasks/rollback`（`node_ids`） | `POST /open-apis/approval/v4/instances/specified_rollback`（`task_def_key_list`） |
| 加签 | `POST /open-apis/approval/v4/tasks/add_sign` | `POST /open-apis/approval/v4/instances/add_sign` |
| Token | `user_access_token` | `tenant_access_token` + body `user_id` |
| 操作人 | 由 token 决定，飞书天然约束为本人 | 服务端传 `user_id`，飞书不校验自然人 |

用户态写接口的 body **不需要 `user_id`、不需要 `approval_code`**：

- `pass` / `refuse`：`instance_code`、`task_id`、`comment?`、`form?`
- `forward`：`instance_code`、`task_id`、`transfer_user_id`、`comment?`
- `rollback`：`instance_code`、`task_id`、`node_ids`、`comment?`
- `add_sign`：`instance_code`、`task_id`、`add_sign_user_ids`、`add_sign_type`、`approval_method?`、`comment?`

> 经典租户态的 `add_sign` 请求体没有「当前审批人」字段，服务端校验是唯一闸门；用户态由 token 约束操作人，这正是本方案选用户态的主要原因之一。

### 2.2 飞书 OAuth 事实

| 事实 | 出处/现状 |
|---|---|
| 授权页 `https://accounts.feishu.cn/open-apis/authen/v1/authorize`；令牌端点 `https://accounts.feishu.cn/oauth/v3/token` | 官方文档 + 现有 `user_token.rs` |
| `offline_access` 必须出现在授权 scope 里，否则没有 `refresh_token` | 现有设计 §2 |
| `refresh_token` 一次性，刷新即换新；`access_token` 约 2 小时 | 现有设计 §2 |
| 用户授权满 365 天必须重新授权；刷新**不**重置该时钟 | 现有设计 §2 |
| 授权页 scope 必须是开发者后台已开通权限的子集，否则 20027 | 现有设计 §2 |
| 本部署是裸 IP + 18654 端口（安全组只放行 18xxx） | 部署现状 |
| 现有 `feishu_user_token` 是**单管理员**凭证，只服务派发的姓名→open_id 解析 | 现有设计 D1 |
| 现有 OAuth state 是进程内内存表（10 分钟、一次性） | 现有设计 D5 |
| 现有 `fetch_user_info` 只反序列化 `open_id` 与 `name`，未取邮箱/头像/`tenant_key`/`union_id` | `src/addon/feishu/domain/user_token.rs:429-434` |
| 现有回调 `GET /api/v1/feishu/oauth/callback` 返回 JSON 信封，不签发系统会话 | `src/addon/feishu/approval/actions/oauth_callback.rs` |
| 现有 `oauth_config()` 额外要求 `can_pull()`（数据源出站开关） | `src/addon/feishu/domain/user_token.rs:117-139` |
| `oauth_redirect_uri` 在 `docs/contracts/CONFIGURATION.md` 没有条目，只有源码注释与设计文档 | 2026-11-11 源码核实 |

> SSO 需要 `user_info` 的邮箱/头像/`tenant_key`/`union_id`，但现有结构体只解析 `open_id`/`name`。Phase 0 必须先真机抓 `authen/v1/user_info` 的完整报文，再扩结构体；不要按文档猜字段。
>
> 登录配置应走独立的 `login_oauth_config()`：只要求 `enabled` + `app_id`/`app_secret` + `login_redirect_uri`，**不要求** `can_pull()`——数据源拉取开关不应决定用户能否登录。

### 2.3 本系统认证事实

| 事实 | 位置 |
|---|---|
| 密码登录 `POST /api/v1/users/login`，支持邮箱或用户名；登录成功返回 `access_token`，Refresh 走 HttpOnly Cookie | `src/addon/account/user/actions/login.rs`；`frontend/src/engine/session/lifecycle.ts` |
| Refresh Cookie：`yang_refresh`、Host-only、HttpOnly、`SameSite=Strict`、`Path=/api/v1/users` | `src/addon/account/domain/context.rs:41`；`frontend/e2e/support.ts` |
| 会话事实源 `user_session`，Access/Refresh 必须携带 `session_id` 且会话行未撤销 | `docs/contracts/SESSION_AUTHORITY.md` |
| 登录事件 `login_event`（user_id、时间、IP、UA、result、failure_reason） | `src/infrastructure/schema.rs:304` |
| 首个注册账号在同一事务内竞争 `system_owner` 哨兵并成为系统管理员 | `register.rs` + `Account::claim_system_owner` |
| 基础授权 `AuthorizationGrants::user()` = 角色 `user`，无任何权限 | `src/addon/account/domain/grants.rs:19` |
| 账号状态 active/disabled/deleted；`users.email` 可空且与 `email_verified_at` 成对 | `src/addon/account/user/table.rs` |
| `password_hash` 必填 | `src/addon/account/user/table.rs:107` |
| 用户名字符集 `^[A-Za-z0-9_-]+$`，3–64 字符，禁止 `deleted_` 前缀 | `src/addon/account/domain/policy.rs` |
| MFA 两段式：第一因子通过后返回 700012 `SecondFactorRequired`，第二段带 `mfa_code` | `docs/architecture/multi-factor-login.md` |
| 外部身份端口 `ExternalIdentityProvider` 已定义但**无表、无实现** | `src/addon/account/domain/oidc.rs` |
| 前端 `RequireAuth` 未登录时硬编码跳 `/login`，**不携带原始路径**；登录页已支持 `?redirect=` | `frontend/src/shell/auth-gate.tsx:39-45`；`LoginPage.tsx:41-42` |
| 生产 CSP `connect-src 'self'`、`img-src 'self' data: blob:` | `frontend/vite.config.ts:25-37` |
| 会话签发可复用：`Account::claims_for` → `TokenManager::generate_token_pair_with_refresh_claims` → `record_login_session` → `BrowserSession::token_response` | `src/addon/account/domain/context.rs:511-519`；`yang-base/src/token/manager.rs:493-501`；`src/addon/account/user/actions/login.rs:292-368`；`yang-base/src/action/auth/browser_session.rs:97-107` |
| `record_login_session` 当前是 `pub(super)`，只对 `account::user::actions` 可见 | `src/addon/account/user/actions/login.rs:292` |
| `LoginAction` + `CredentialVerifier` 也提供同构签发路径（邮箱验证码登录是现成例子） | `src/addon/account/user/actions/login_by_email_code.rs:163-238` |
| 用户插入 `UserRepository::insert_in_tx` 只写 `username`/`password_hash`/`status`；`authz_version=1`、`credential_version=0` 由表默认值 | `src/addon/account/domain/repository.rs:279-295` |
| 首个管理员引导在注册事务内：`claim_system_owner` | `src/addon/account/user/actions/register.rs:69-89`；`src/addon/access/domain/groups/owner.rs:32-70` |
| 运行支撑表固定数组当前 9 张（含 `user_session`/`login_event`/`user_avatar`），新增表要改数组长度与测试断言 | `src/infrastructure/schema.rs:106-122`、`358-375` |

### 2.4 前端事实

- 登录/注册/重置页面在 `frontend/src/features/auth/pages/`；账号页在 `frontend/src/features/account/AccountSettingsPage.tsx`。
- 已认证 REST 请求走 `requestWithTokenRefresh`（Bearer access token + `credentials: "include"`），见 `features/account/api.ts:45-80`。
- 登录页已有 `?redirect=` 安全目标解析 `safeRedirectTarget`，见 `features/auth/redirect.ts:8-12`。
- `features/` 各域之间禁止互相 import；账号域不能 import 飞书域（`frontend/AGENTS.md:47-51`）。
- 共享 UI 有 Button/Input/Dialog/Badge/Checkbox/Table/Skeleton/Select/Toast；**没有**通用第三方登录按钮，也没有飞书头像组件。
- Nginx 已有 SPA 回退 `try_files $uri $uri/ /index.html`，`/batch-approval` 深链接无需改 Nginx。

### 2.5 限速与文档冲突

| 接口 | 限制 |
|---|---|
| `pass` / `refuse` / `forward` | 100 次/分钟 |
| `rollback` / `add_sign` | 1000 次/分钟，50 次/秒 |
| `GET /tasks` / `GET /instances/detail` | 1000 次/分钟，50 次/秒 |
| `POST /open-apis/auth/v3/tenant_access_token/internal` | 特殊频控（现有 tenant token 缓存已处理） |

已知文档冲突（Phase 0 逐条真机确认）：

- 用户态 `add_sign` 的 `add_sign_type` 1/2/3 与 `approval_method` 1/2/3 的枚举含义在不同版本文档里不一致；
- 用户态退回用 `node_ids`（节点 ID 哈希，发起节点 `START`），经典 `specified_rollback` 用 `task_def_key_list`（`APPROVAL_x_y`），两套不能混用；
- 用户态接口在 `uAjLw4CM` 旧文档路径，`server-docs` 新路径 404；
- `GET /tasks` 的 `support_api_operate=false`、手写签名（1390018）、条件表单（1390001）等边界以真机为准。

---

## 3. 总体架构

### 3.1 两条身份链，不要混

```
┌─────────────────────────────── 本系统 ───────────────────────────────┐
│                                                                      │
│  浏览器 ──登录/刷新──► account 域 ──► user_session / access+refresh JWT │
│     │                     │                                          │
│     │                     └─ user_identity ── provider=feishu ──┐     │
│     │                                                           │     │
│     └──批量审批请求(Bearer)──► feishu 域 ──► feishu_user_credential │   │
│                                      │        （每用户 user token） │   │
│                                      └──────► 飞书用户态审批 API    │   │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
```

- **系统会话**：本系统的 access/refresh JWT，决定「你是谁、能不能进页面」。
- **飞书用户凭证**：每用户一条 `user_access_token`/`refresh_token`，只用于调用飞书用户态 API，不参与本系统鉴权。
- 两者通过 `user_identity` 关联：系统用户 ↔ 飞书 `open_id`。

### 3.2 登录时序（首次 / 已有绑定）

```
用户           前端 /login          后端 auth/feishu           飞书
 │ 点击飞书登录      │                       │                    │
 │───────────────►│  GET /authorize?redirect │                    │
 │                │──────────────────────►│ 生成 state+nonce+PKCE │
 │                │                       │ 种 nonce Cookie       │
 │                │◄────302───────────────│                    │
 │──────────────────────────────────────────────────────────────►│ 授权页
 │◄──────────────────────────────────────────────────────────────│ 回调 code+state
 │                │  GET /callback?code&state                    │
 │                │──────────────────────►│ 校验 state+nonce      │
 │                │                       │ 换 token + user_info  │
 │                │                       │ 查 user_identity       │
 │                │                       │  ├─ 已绑定 → 查用户状态/MFA → 签发会话
 │                │                       │  └─ 未绑定 → 302 /login/feishu/choose
 │                │◄────302 /batch-approval│（已绑定；或选择页绑定/新建完成）
 │  SPA 加载，restoreFromCookie → /users/refresh → 进入页面        │
```

> 选择页是公开页：`GET /auth/feishu/choose` 只展示飞书姓名/脱敏邮箱与两个选项，不消费 token；`POST /choose/bind` 走账号密码 + MFA，`POST /choose/create` 走显式新建。两者成功后才签发会话并消费 token。

### 3.3 已有账号绑定时序

```
已登录用户 /account     前端 account 域        后端 auth/feishu        飞书
 │ 点「绑定飞书」          │                       │                    │
 │──────────────────────►│ POST /users/me/identities/feishu/authorize
 │                       │──────────────────────►│ state{flow=Bind,user_id}
 │                       │◄────authorize_url─────│
 │ window.location.assign(authorize_url) ───────────────────────────►│
 │◄──────────────────────────────────────────────────────────────────│ 回调
 │                       │ GET /auth/feishu/callback?code&state      │
 │                       │──────────────────────►│ 校验 state+nonce
 │                       │                       │ 换 token + user_info
 │                       │                       │ 冲突检查 → 写 user_identity
 │                       │                       │ 写 feishu_user_credential
 │                       │◄────302 /account?feishu=bound─────────────│
 │ SPA 重新加载，restoreFromCookie 恢复会话，账号页显示已绑定          │
```

### 3.4 批量审批时序

```
/batch-approval          后端 feishu 域                  飞书用户态 API
 │ GET /tasks               │                              │
 │────────────────────────►│ 取 credential → 必要时刷新    │
 │                         │─────────────────────────────►│ GET /tasks?topic=1
 │◄── 分组后的待办 ─────────│◄─────────────────────────────│
 │ 勾选 + 同意/拒绝/...     │                              │
 │ POST /jobs              │ 校验任务仍在待办、参数合法     │
 │────────────────────────►│ 建 job + items（幂等键）       │
 │◄── job_id ──────────────│                              │
 │ GET /jobs/{id} 轮询      │ worker 领取                  │
 │────────────────────────►│ 逐条 POST tasks/pass|...     │
 │                         │─────────────────────────────►│
 │◄── 逐条结果 ─────────────│◄─────────────────────────────│
```

### 3.5 关键决策

| # | 决策 | 理由 |
|---|---|---|
| D1 | 飞书登录用授权码 + **PKCE + state + nonce Cookie**，后端回调 302 | 公开登录必须防登录 CSRF；后端回调避免前端承担 code 换 token；CSP `connect-src 'self'` 也要求换码在服务端 |
| D2 | `user_identity` 作为 account 域的**运行支撑表**（进 `infrastructure_definitions()`，不建 UI module）；`feishu_user_credential` 与批量 job/item 作为 feishu 域的纯表 module | 身份映射是账号体系的事实；它没有独立 UI，和 `user_session`/`login_event` 同层最贴切；token 形状是飞书特有的，不能污染通用表 |
| D3 | 首次飞书登录且无绑定时进入**账号选择页**：绑定已有账号 / 新建账号（用户输入用户名+密码）；不做自动建号；选择页新建绝不参与 `system_owner` 竞争 | 用户显式选择，避免「本来有账号却误建新号」；同时避免任意租户成员成为系统管理员 |
| D4 | 已有账号**显式绑定**：登录后账号设置绑定，或首次登录选择页用账号密码+MFA 绑定；**不按邮箱自动绑定** | 邮箱自动绑定是账号接管面；显式密码证明才安全 |
| D4b | 飞书邮箱命中 `users.email` 只作为选择页的**提示**（建议绑定），不作为自动路由/自动绑定条件 | 邮箱匹配可能有误（同名/换绑/未验证）；最终由用户选择并证明密码 |
| D5 | 飞书邮箱**不写入** `users.email`；只存 `user_identity.external_email` | `users.email` 是「已验证邮箱」，写入会触发成对 CHECK、影响密码找回与邮箱登录；飞书邮箱未走本系统验证 |
| D6 | 新建账号用户只拿基础角色 `user`，无任何权限；批量审批 Action 用 `authenticated` 门禁，不新增权限键 | 批量审批只操作自己的待办，权限由飞书 token 天然限定；避免「新用户没有权限」与「默认授权过宽」的两难 |
| D7 | 飞书登录只替换**第一因子**；已激活 TOTP 的账号必须继续过第二因子 | 与多因子设计的不变量一致，不能因新增 SSO 绕过 MFA |
| D8 | 批量写走**异步 job**（推荐）；同步执行仅作 MVP，单次上限 20 | 100 次/分钟 + Nginx 60s 超时，50 条同步必超时；job 还能落逐条结果与审计 |
| D9 | 批量审批只用 `user_access_token`，不引入 tenant token | 用户态天然约束操作人；经典 `add_sign` 没有当前审批人字段，服务端校验是唯一闸门 |
| D10 | 每用户凭证按 `identity_id` 行锁 + 进程内每用户互斥刷新；不做全员每日保活 | 刷新令牌一次性，必须串行；登录本身就是一次授权，按需刷新足够；过期就让用户重新授权 |
| D11 | 登录 state 存进程内内存（v1）；多实例需换 Redis | 与现有单管理员 OAuth 一致，单实例假设明确；重启丢 state 是失败闭合（重试即可） |
| D12 | `/batch-approval` 是根层 `RequireAuth` 路由，不套 `AppLayout` | 分享链接直接进入专注页面；不依赖 Catalog 导航；与 `/select-identity` 同形 |
| D13 | 飞书头像 v1 用姓名首字/占位，不直接加载飞书 URL | 生产 CSP `img-src 'self' data: blob:` 禁止外域图片；代理头像会引入 SSRF 与额外端点，v1 不做 |
| D14 | 上线前必须 TLS | OAuth code、会话 Cookie、飞书 token 走明文 HTTP 是 P0；当前裸 IP 部署只能作为内网/联调 |

---

## 4. 数据模型

> 所有表走声明式 Schema（`docs/contracts/SCHEMA.md`），不新增 SQL 迁移文件。一张表 = 一个 module；只有主表 module 挂 Action，绑定表 module 只声明表。

### 4.1 `user_identity`（account 域，运行支撑表）

> 它不是 UI module：注册进 `src/infrastructure/schema.rs::infrastructure_definitions()`（当前 9 张，需改数组长度与 `schema.rs:358-375` 测试断言），与 `user_session`/`login_event` 同层；表定义与 Repository 仍在 account 域。

| 列 | 类型 | 说明 |
|---|---|---|
| `id` | Key | 主键 |
| `user_id` | BigInt 必填、索引 | 指向 `users.id` |
| `provider` | Str(32) 必填 | `feishu`（未来可扩展） |
| `tenant_key` | Str(64) 必填、默认 `""` | 飞书租户；非租户型 IdP 为空串 |
| `external_id` | Str(128) 必填 | 飞书 `open_id`（当前应用内稳定） |
| `external_union_id` | Str(128) 可空 | 飞书 `union_id`（开发者维度稳定，留作应用重建后的迁移线索） |
| `external_name` | Str(128) 可空 | 飞书姓名（展示用） |
| `external_email` | Str(254) 可空、索引 | 飞书邮箱（trim + lowercase 归一化后存储/比较；仅用于「邮箱命中已有账号」判断与展示，不写 `users.email`） |
| `avatar_url` | Str(1024) 可空 | 飞书头像 URL；v1 不直接渲染 |
| `status` | Radio 必填、默认 `active` | v1 只用 `active`；`revoked` 预留给管理员冻结绑定。解绑走硬删除，避免唯一索引挡住重新绑定 |
| `bound_at` | Timestamp 必填 | 绑定时间 |
| `last_login_at` | Timestamp 可空 | 最近一次飞书登录成功 |
| `created_at` / `updated_at` | 标准列 | |

约束：

- 唯一索引 `uk_user_identity_external` (`provider`, `tenant_key`, `external_id`)：一个飞书身份只能绑定一个系统用户。
- 唯一索引 `uk_user_identity_user_provider` (`user_id`, `provider`, `tenant_key`)：一个系统用户在同一租户下只能绑定一个飞书身份。
- 索引 `idx_user_identity_email` (`external_email`)：首次登录的邮箱命中查询。
- 外键 `fk_user_identity_user` → `users.id`（RESTRICT；账号删除前必须先清理绑定，沿用 E-2 的前置清理纪律）。

### 4.2 `feishu_user_credential`（feishu 域，纯表 module）

> `feishu::build_user_credential_module()` 只声明表、不挂 Action；读写由 SSO/批量 Action 与 worker 经 `FeishuContext` 完成。`FeishuContext::new` 当前是 16 个 Repository 的位置参数，新增三张表后要同步扩展。

| 列 | 类型 | 说明 |
|---|---|---|
| `id` | Key | 主键 |
| `identity_id` | BigInt 必填、唯一 | 指向 `user_identity.id`；一行一份凭证 |
| `user_id` | BigInt 必填、索引 | 冗余系统用户 ID，worker 直接按用户查 |
| `open_id` | Str(64) 必填 | 飞书 `open_id`（冗余，调用飞书 API 用） |
| `access_token` | Text 必填、`secret`、仅 SYSTEM 可读写 | 用户访问令牌 |
| `refresh_token` | Text 必填、`secret`、仅 SYSTEM 可读写 | 一次性刷新令牌 |
| `access_token_expires_at` | Timestamp 必填 | 按 `expires_in` 实算 |
| `refresh_token_expires_at` | Timestamp 必填 | 按 `refresh_token_expires_in` 实算；响应缺失时用保守值 |
| `scope` | Str(1024) | 授权范围快照；99991672/99991679 时用于区分「授权没要」与「后台没开」 |
| `authorized_at` | Timestamp 必填 | 最近一次完整授权时间（365 天判据） |
| `status` | Radio 必填、默认 `active` | `active` / `reauth_required` / `revoked` |
| `last_refreshed_at` | Timestamp 可空 | 最近一次成功刷新 |
| `created_at` / `updated_at` | 标准列 | |

约束/不变量：

- `identity_id` 唯一：同一绑定只有一份凭证。优先声明外键到 `user_identity.id`；若 schema 同步器对跨 addon 表的外键创建顺序不支持，则退化为唯一索引 + 解绑/删除事务清理（Phase 1 验证）。
- `status=reauth_required` 时批量接口直接返回可行动的重新授权错误，不再尝试刷新。
- 解绑/账号删除时删除该行；不保留明文令牌。
- 不把该表与现有 `feishu_user_token`（单管理员）混用：前者是「用户本人批量审批」，后者是「派发解析申请人姓名」，生命周期与权限都不同。

### 4.3 `feishu_batch_job`

> `batch_approval` 主 module 的表；创建/查询/取消 Action 与表同模块。

| 列 | 类型 | 说明 |
|---|---|---|
| `id` | Key | 主键 |
| `user_id` | BigInt 必填、索引 | 发起人系统用户 |
| `identity_id` | BigInt 必填 | 使用的飞书身份 |
| `action` | Radio 必填 | `pass` / `refuse` / `forward` / `rollback` / `add_sign` |
| `status` | Radio 必填、默认 `pending` | `pending` / `running` / `succeeded` / `partial` / `failed` / `cancelled` |
| `idempotency_key` | Str(64) 必填 | 客户端生成的 UUID；唯一索引 `(user_id, idempotency_key)` 防重复提交 |
| `total` | Int 必填、默认 0 | 任务条数 |
| `succeeded` / `failed` / `unknown` | Int 必填、默认 0 | 汇总计数 |
| `comment` | Str(500) 可空 | 审批意见 |
| `transfer_open_id` | Str(64) 可空 | 转交目标（`forward`） |
| `add_sign_open_ids` | Str(2048) 可空 | 加签目标 open_id JSON 数组（去重、1–10 人） |
| `add_sign_type` | Int 可空 | 加签位置（1/2/3，含义以 Phase 0 确认为准） |
| `approval_method` | Int 可空 | 加签审批方式（1/2/3，含义以 Phase 0 确认为准） |
| `rollback_mode` | Str(16) 可空 | `initiator` / `previous`（批量退回只支持这两种；单条可选具体节点） |
| `created_at` / `updated_at` / `finished_at` | 标准列 | |

> `identity_id` 是历史引用，**不声明外键**：job 保留 90 天，若加 FK，解绑/删除身份会被历史 job 阻塞。解绑时取消未开始 job，但已完成的 job 作为审计记录保留。

### 4.4 `feishu_batch_job_item`

> 纯表 module；与 job 一一对应，worker 逐条更新。

| 列 | 类型 | 说明 |
|---|---|---|
| `id` | Key | 主键 |
| `job_id` | BigInt 必填、索引 | 指向 job |
| `task_id` | Str(64) 必填 | 飞书 task_id |
| `instance_code` | Str(128) 必填 | 飞书实例 Code |
| `definition_code` | Str(128) 可空 | 审批定义 Code |
| `definition_name` | Str(255) 可空 | 审批定义名称（快照） |
| `title` | Str(512) 可空 | 任务标题（快照） |
| `initiator_name` | Str(128) 可空 | 发起人姓名（快照） |
| `status` | Radio 必填、默认 `pending` | `pending` / `running` / `succeeded` / `failed` / `unknown` / `skipped` |
| `attempts` | Int 必填、默认 0 | 写入尝试次数（仅限流重试） |
| `result_code` | Int 可空 | 飞书业务码 |
| `message` | Str(1024) 可空 | 可行动错误文案 |
| `before_status` / `after_status` | Str(16) 可空 | 任务状态快照 |
| `started_at` / `finished_at` | Timestamp 可空 | |
| `created_at` / `updated_at` | 标准列 | |

约束：唯一索引 `uk_batch_item_job_task` (`job_id`, `task_id`)；任务去重后入库。`job_id` 优先声明外键到 `feishu_batch_job.id`（RESTRICT；保留期清理先删 item 再删 job）。

### 4.5 现有表的改动

| 表 | 改动 | 说明 |
|---|---|---|
| `users` | 无结构改动 | 新建账号写入用户输入的用户名、用户密码的 Argon2 摘要、`status=active`；`authz_version`/`credential_version` 由表默认值给出（1/0）；`email`/`email_verified_at` 保持 NULL |
| `login_event` | 可选：新增 `auth_method`（`password`/`email_code`/`feishu`，默认 `password`） | 用于安全事件区分登录方式；若枚举扩展风险大，v1 只写审计，不改表 |
| `user_session` | 无结构改动 | 飞书登录复用现有会话签发；会话清理 worker 自动覆盖 |
| `user_avatar` | 无结构改动 | 飞书头像不存入；v1 用姓名首字/占位 |
| E-2 账号删除 | 新增清理 | 匿名化事务内删除 `user_identity`、`feishu_user_credential`、该用户的 `feishu_batch_job/item`；先删子表再删主表 |

### 4.6 保留期

- `feishu_batch_job` / `feishu_batch_job_item`：建议保留 90 天，由现有清理 worker 或新增清理任务按 `created_at` 删除；它们是操作审计的明细，不能无限增长。
- `user_identity` / `feishu_user_credential`：绑定存续期间保留；解绑/删除账号即清理。
- `login_event`：沿用现有保留策略。

---

## 5. 飞书登录

### 5.0 模块归属与装配

- SSO 端点落 `src/addon/feishu/sso/`（新 module）：飞书 OAuth 传输、state、换码、`user_info` 都已经在 feishu addon，放这里不需要反向依赖。
- account 域导出窄端口 `ExternalLoginPort`：按外部身份查用户、按已验证邮箱查用户、新建账号、绑定/解绑、列绑定、签发系统会话、查 `system_owner` 是否存在。实现由 `Account` 承担；`src/app.rs` 先构造这个端口，再注入 feishu 的 `build_addon`。
- E-3 的 `ExternalIdentityProvider` 只有 `find_local_user`/`link_local_user`（`external_sub` 维度），不含 OAuth 换码、profile、建号与会话签发；本方案保留它作为「身份映射」语义锚点，新增 `ExternalLoginPort` 承担登录编排。若后续出现第二个 IdP，再把两者收敛为通用端口。
- 当前 `src/app.rs:100-115` 先建 feishu、`118-126` 才建 account；`account::build_addon` 现在在内部构造 `Arc<Account>` 并只返回 `AddonSpec`。实施时要像 feishu 的 `build_context` 那样把 account 上下文的构造拆出来，否则 feishu 拿不到 `Account`。
- `batch_approval` 主 module 声明 `feishu_batch_job` 表 + Action；`feishu_batch_job_item`、`feishu_user_credential` 各占一个纯表 module；`sso` module 只挂 Action（不声明表）。`FeishuContext` 的 Repository 从 16 个增到 19 个。
- 不复用 `/api/v1/feishu/oauth/callback`：它是单管理员 JSON 流；SSO 回调独立并返回 302。
- 框架侧待补：`BrowserSession` 目前只提供 `token_response`，`refresh_cookie` 是私有；「302 + Set-Cookie」需要新增公开方法（或由 account 域拼 Cookie）。这是 lib_yang 的纯新增，先推 lib_yang 再推 yang-system。

### 5.1 OAuth 参数与 scope

授权 URL（后端拼接，前端不拼）：

```
https://accounts.feishu.cn/open-apis/authen/v1/authorize
  ?client_id=<app_id>
  &redirect_uri=<feishu.login_redirect_uri>
  &response_type=code
  &state=<one-time-state>
  &scope=approval:task:read approval:task:write approval:instance:read
         contact:user:search contact:contact.base:readonly offline_access
  &code_challenge=<S256>
  &code_challenge_method=S256
  &prompt=consent
```

scope 说明：

| Scope | 用途 | 是否必需 |
|---|---|---|
| `approval:task:read` | `GET /tasks` 拉本人待办 | 必需 |
| `approval:task:write` | pass/refuse/forward/rollback/add_sign | 必需 |
| `approval:instance:read` | `GET /instances/detail` | 必需 |
| `contact:user:search` | 转交/加签选人 `GET /search/v1/user` | 必需 |
| `contact:contact.base:readonly` | 通讯录基础信息（现有管理员授权已在用） | 建议保留 |
| `offline_access` | 拿 `refresh_token`，批量审批在 access token 过期后继续 | 必需 |

> 登录与批量审批共用**同一次授权**：用户点一次「飞书登录」即授予批量审批所需 scope。若产品希望「只登录、不授权审批」，可拆成两次增量授权（登录最小 scope → 进入批量审批时再请求审批 scope）；v1 不做，避免两套 token 生命周期。

登录配置走独立的 `FeishuSettings::login_oauth_config()`：只要求 `enabled` + `app_id`/`app_secret` + `login_redirect_uri`，**不要求** `can_pull()`。现有 `oauth_config()` 的 `can_pull()` 约束只服务管理侧授权；若复用，数据源拉取开关会意外决定用户能否登录。

若真机确认 `authen/v1/user_info` 不返回 `tenant_key`：存空串，按「自建应用只属于一个租户」信任应用所属租户；不要伪造或从其他字段猜。`login_allowed_tenant_keys` 非空时，只有能拿到真实 `tenant_key` 才启用白名单校验，否则启动期拒绝该配置（fail-closed）。

### 5.2 端点

| 端点 | 方法 | 鉴权 | 用途 |
|---|---|---|---|
| `/api/v1/auth/feishu/status` | GET | public | 登录入口是否可用（`{enabled}`），供登录页决定是否显示飞书按钮；不泄露配置细节。**始终注册**，未启用时返回 `{enabled:false}`；其余端点仅在启用且凭据齐全时注册 |
| `/api/v1/auth/feishu/authorize` | GET | public | 开始登录：校验 `redirect` 白名单、生成 state+nonce+PKCE、302 到飞书 |
| `/api/v1/auth/feishu/callback` | GET | public | 登录/绑定共用回调：校验 state+nonce、换码；已绑定→登录/MFA；未绑定→302 选择页 |
| `/api/v1/auth/feishu/choose` | GET | public（pending token + nonce Cookie） | 选择页展示：飞书姓名、脱敏邮箱、是否命中已有邮箱；**不消费** token |
| `/api/v1/auth/feishu/choose/bind` | POST | public（pending token + nonce Cookie） | 绑定已有账号：账号标识 + 密码（+MFA）；成功才消费 token |
| `/api/v1/auth/feishu/choose/create` | POST | public（pending token + nonce Cookie） | 新建账号：用户输入用户名 + 密码（与正常注册一致）+ 显式确认；成功才消费 token |
| `/api/v1/auth/feishu/mfa` | POST | public（pending token + nonce Cookie） | 已绑定用户的第二因子完成，签发会话 |
| `/api/v1/users/me/identities` | GET | authenticated | 当前用户的绑定列表 |
| `/api/v1/users/me/identities/feishu/authorize` | POST | authenticated + Step-up（有密码/TOTP 的账号） | 已登录用户开始绑定/重新授权，返回 authorize URL；历史无密码账号降级为当前会话 + 二次确认 |
| `/api/v1/users/me/identities/feishu` | DELETE | authenticated + Step-up（有密码/TOTP） | 解绑飞书身份；无密码账号降级为当前会话 + 二次确认 |

回调错误不返回 JSON 给浏览器，统一 302 到 `/login?feishu_error=<code>`（或 `/account?feishu_error=<code>`），前端用 toast 展示可行动文案；详细原因只进日志/审计。

`pending_choice` token：一次性、10 分钟、绑定 Feishu `open_id` + `tenant_key` + 登录发起时的 nonce Cookie。`GET /choose` 只读展示，不消费；`bind`/`create` **成功才原子消费**（条件更新 `consumed=false -> true`，并发只有一个赢）；token 虽在 URL 里，单独拿到它不能建号/绑定——还必须持有同一个 nonce Cookie。

### 5.3 首次登录：账号选择页

回调发现 `user_identity` 中没有 `(provider=feishu, tenant_key, open_id)` 时，不自动建号，也不按邮箱自动路由，而是：

1. 生成一次性 `pending_choice`（10 分钟，绑定 `open_id`/`tenant_key`/`redirect`/nonce）；
2. 302 到 `/login/feishu/choose?token=...&redirect=...`；
3. 选择页展示飞书姓名、脱敏邮箱，以及（可选的）「该邮箱已对应系统账号，建议绑定」提示；
4. 用户选择：
   - **绑定已有账号**：进入 §5.4 B，账号密码 + MFA；
   - **新建账号**：仅当 `login_allow_create=true` 时显示；用户输入用户名/密码并显式确认后，按下面的表单规则创建。

新建账号的前置条件（全部满足）：

1. `user_identity` 中没有绑定；
2. `login_allow_create=true`；
3. 系统已经完成首个管理员初始化（`system_owner` 存在）。否则拒绝并提示先用账号密码注册首个管理员。

> 初始化检查走 account 端口的 `has_system_owner()`（内部读 `system_owner` 哨兵，不直接写/查 access 表）。`register.rs:69-89` 的 `claim_system_owner` 是唯一允许写哨兵的路径；选择页新建路径**不得调用**它，也不得加入 `system_admin` 组。

新建账号表单（用户自行输入用户名和密码，与正常注册一致）：

| 字段 | 规则 |
|---|---|
| `username` | 用户输入；复用 `normalize_username`（trim + lowercase、3–64 字符、`^[A-Za-z0-9_-]+$`、禁止 `deleted_` 前缀）；唯一约束冲突时返回字段错误，由用户改一个，不自动改名 |
| `password` | 用户输入；复用 `validate_password_field`（10–128 字符、弱口令字典、不能与用户名相同）；Argon2 哈希入库，明文不落库 |
| 确认密码 | 前端校验两次一致；后端只接收一个 `password` 字段 |
| `email` / `email_verified_at` | 均为 NULL（见 D5）；飞书邮箱存 `user_identity.external_email` |
| `status` | `active` |
| `authz_version` / `credential_version` | 由表默认值（1 / 0） |
| 授权 | 基础角色 `user`，无任何权限；**不调用** `claim_system_owner`，不写 `system_owner`，不加入 `system_admin` |
| 审计 | `feishu.user.created`（`source=choice`、open_id、username、IP/UA；不含密码/token） |

> 新建账号有用户自己设置的密码，创建后即可用「用户名 + 密码」或飞书登录。`users.email` 仍为 NULL；用户可稍后在账号设置里绑定并验证邮箱，之后才能用邮箱登录/自助找回密码。忘记密码且未绑定邮箱时，由管理员签发重置凭证兜底。

### 5.4 已有账号绑定

两条路径，最终都写同一张 `user_identity`：

**A. 已登录用户主动绑定（主路径）**

1. `/account` 的「飞书账号」区块点「绑定飞书」；有密码/TOTP 的账号先过 Step-up，历史无密码账号用当前会话 + 二次确认；
2. `POST /api/v1/users/me/identities/feishu/authorize` 生成 state（`flow=Bind`、`user_id`=当前会话用户、`redirect=/account`）；
3. 浏览器整页跳转飞书授权；回调校验 state+nonce 后，以 state 里的 `user_id` 为准（不接受回调参数里的 user_id）；
4. 冲突检查：该飞书身份已绑定其他用户 → 302 `/account?feishu_error=already_bound`；当前用户已有其他飞书身份 → 302 `/account?feishu_error=already_linked`；
5. 写 `user_identity` + `feishu_user_credential`，审计 `feishu.identity.bound`，302 `/account?feishu=bound`；
6. 前端整页重新加载，`restoreFromCookie` 恢复会话，账号页显示「已绑定：<飞书姓名>」。

**B. 首次飞书登录的选择页绑定已有账号**

回调发现无绑定时，进入选择页（§5.3），用户点「绑定已有账号」后：

> 飞书邮箱命中 `users.email` 时，选择页只显示**脱敏提示**（如 `z***@corp.com`，建议绑定），不自动路由、不自动绑定；用户仍可手动输入账号标识或选择新建。
>
> 若 `user_info` 不返回邮箱，提示不可用，但选择页与手输账号标识的绑定路径不受影响。

1. 绑定表单：账号标识（用户名或已验证邮箱）+ 密码；若目标账号已激活 TOTP，再要求第二因子（走现有 700012 两段式）；
2. `POST /api/v1/auth/feishu/choose/bind` 校验 pending token + nonce Cookie（token 单次消费，失败不消费，便于重试）、用与登录相同的等时密码校验与限流 `AuthOperation::Login`、校验目标账号 active；
3. 冲突检查：该飞书身份已绑定其他用户、或目标用户已绑定其他飞书身份 → 明确拒绝；唯一约束兜底并发；
4. 通过后写 `user_identity` + `feishu_user_credential`，签发系统会话，返回 `{access_token, redirect}`；前端 `beginSession` 后跳转；
5. 失败统一返回 `InvalidPassword`（不泄露账号是否存在/是否启用 MFA），审计 `feishu.identity.link_failed`。

> 不按邮箱自动绑定：邮箱命中只作为选择页提示，必须再用账号密码证明归属。这样既避免重复建号/误建号，也不引入账号接管面。
>
> 绑定已有账号要求目标账号有**可用密码**。通过选择页新建的账号都有用户设置的密码；若存在历史飞书-only 账号（无可用密码），先用其原飞书身份登录后在账号设置换绑/解绑，或由管理员签发重置凭证设置密码后再绑定。

**解绑**：

- `DELETE /api/v1/users/me/identities/feishu`：有密码/TOTP 的账号要求 Step-up，历史无密码账号用当前会话 + 二次确认；若解绑后没有任何登录方式，拒绝并提示先设置密码/绑定其他方式；
- 删除 `feishu_user_credential` 与 `user_identity`，取消该用户尚未开始的批量 job，审计 `feishu.identity.unbound`；
- 不删除系统用户本身。

### 5.5 第二因子（TOTP）语义

飞书身份是**第一因子**（持有飞书账号），不是第二因子。规则：

- 已绑定用户 `totp_activated_at IS NOT NULL`：回调校验飞书身份后**不签发会话**，生成一次性 `pending_login`（2 分钟；绑定 `user_id` + 飞书 `open_id` + `redirect`），302 到 `/login/feishu/mfa?token=...&redirect=...`；
- `POST /api/v1/auth/feishu/mfa` 用 `Account::verify_second_factor` 校验 TOTP / 恢复码 / MFA 备用邮箱验证码（第一因子是飞书，不是邮箱，`allow_backup_email=true` 与密码登录的备用通道语义一致，不构成「邮箱码第一因子 + 邮箱备用码」的同类复用）；
- 通过后签发会话；失败返回参数错误并计入 `AuthOperation::TotpVerify` 限流；
- 未激活 TOTP 的用户直接签发会话。

> 不允许「飞书登录跳过 TOTP」。这与 `multi-factor-login.md` 修复的邮箱验证码绕过是同一类缺口。

### 5.6 系统会话签发

飞书登录必须与密码登录**走同一条签发路径**，不能复制一份：

- 生成稳定的 `session_id`，签发 access/refresh JWT；
- 写 `user_session`（session_id、user_id、当前 refresh jti、IP/UA、last_seen_at）；
- 解析授权快照（基础角色 `user` + access 组/直授），写入 claims 的 `authz_version`；
- 写 `login_event`（成功/失败；可选 `auth_method=feishu`）；
- 新设备登录提醒（best-effort）；
- Set-Cookie `yang_refresh`（现有 HttpOnly/SameSite=Strict/Path 规则不变）。

实施方式（按现有代码的可复用点）：

1. `Account::claims_for(ctx, user_id)`（`context.rs:511-519`）生成 claims 与新的 `session_id`；
2. `ctx.tools().token()?.generate_token_pair_with_refresh_claims(...)`（`yang-base/src/token/manager.rs:493-501`）签发 access/refresh；
3. `record_login_session(...)`（`login.rs:292-368`）写 `user_session` + `login_event` + 新设备提醒；它当前是 `pub(super)`，需提升为 `pub(crate)` 或移进 `Account`；
4. `Account::browser_session().token_response(access, refresh, secure)`（`browser_session.rs:97-107`）返回 `data.access_token` + Set-Cookie。

也可以走 `LoginAction` + 自定义 `CredentialVerifier` 的同构路径（参考 `login_by_email_code.rs:163-238` 的 `VerifiedSubject::with_token_pair_claims`）；但 SSO 回调是 GET + 302，使用上面的显式四步更容易控制重定向与 Cookie。两条路都不得复制签发逻辑。

响应形状保持现有登录一致：`{"code":0,"message":"会话已建立","data":{"access_token":"..."}}` + `Set-Cookie: yang_refresh=...; Path=/api/v1/users; HttpOnly; SameSite=Strict[; Secure]`。Refresh token 不进入响应体。

### 5.7 失败文案（可行动是唯一标准）

| 场景 | 前端文案要点 |
|---|---|
| 用户取消授权（`access_denied`） | 「已取消飞书授权」 |
| state/nonce 失效/重放 | 「登录已过期，请重新点飞书登录」 |
| 应用未开通 scope（20027） | 「飞书应用未开通审批/通讯录权限，请联系管理员」 |
| 用户凭证缺 scope（99991679） | 「授权缺少审批权限，请重新授权并在授权页同意全部权限」 |
| 租户不在允许范围 | 「当前飞书租户不允许登录本系统」 |
| 账号停用/删除 | 「账号已停用，请联系管理员」（不泄露具体状态细节） |
| 未绑定且选择页不允许新建（`login_allow_create=false`） | 「该飞书账号尚未绑定系统账号，请用账号密码绑定；如需新建账号请联系管理员」 |
| 邮箱命中已有账号 | 选择页显示「检测到已有账号，建议绑定」；仍允许用户选择新建 |
| 选择页 token 失效/重放 | 「登录已过期，请重新点飞书登录」 |
| 绑定冲突 | 「该飞书账号已绑定其他系统账号」/「当前账号已绑定其他飞书账号」 |
| 系统未初始化管理员 | 「系统尚未初始化，请先用账号密码注册首个管理员」 |
| 刷新令牌失效 | 「飞书授权已过期，请重新授权」 |

### 5.8 状态与生命周期

- 飞书登录前检查 `users.status = active`；`disabled`/`deleted` 一律拒绝。
- 账号被停用/删除时，现有 `credential_version`/`authz_version` 失效传播会踢掉系统会话；飞书凭证在账号删除事务内清理，停用时保留但批量接口会因用户状态拒绝。
- 解绑后再次登录会进入账号选择页；用户可重新绑定或新建。若 `login_allow_create=false`，则只能绑定已有账号或联系管理员。

---

## 6. 批量审批

### 6.1 权限与身份

- 所有批量 Action 使用系统会话的 `authenticated` 门禁；不新增权限键。
- Action 从 `ActionContext` 取当前用户 ID，**不接受**请求体里的 user_id。
- 通过 `user_identity` + `feishu_user_credential` 找到当前用户的飞书 token；没有绑定/凭证失效 → 返回 `feishu_reauth_required`，前端引导重新授权。
- 飞书侧权限由用户 token 决定：用户只能看到/处理自己的待办；服务端不做跨用户查询。

### 6.2 接口映射

| 功能 | 本系统端点 | 飞书 API | 备注 |
|---|---|---|---|
| 待办列表 | `GET /api/v1/feishu/batch-approval/tasks` | `GET /open-apis/approval/v4/tasks?topic=1&page_size=100&user_id_type=open_id` | 服务端聚合分页，按 `definition_code/name` 分组 |
| 实例详情 | `GET /api/v1/feishu/batch-approval/tasks/{task_id}` | `GET /open-apis/approval/v4/instances/detail?instance_code=...` | 返回 `current_nodes`/`operation_records`/`tasks`/`form` |
| 用户搜索 | `GET /api/v1/feishu/batch-approval/users/search?q=` | `GET /open-apis/search/v1/user?query=...` | 转交/加签选人；前端防抖 |
| 创建任务 | `POST /api/v1/feishu/batch-approval/jobs` | — | 校验后建 job + items |
| 查询任务 | `GET /api/v1/feishu/batch-approval/jobs/{id}` | — | 进度 + 逐条结果 |
| 取消任务 | `POST /api/v1/feishu/batch-approval/jobs/{id}/cancel` | — | 只取消未开始项 |
| 同意 | worker | `POST /open-apis/approval/v4/tasks/pass` | `instance_code` + `task_id` + `comment?` |
| 拒绝 | worker | `POST .../tasks/refuse` | 同上 |
| 转交 | worker | `POST .../tasks/forward` | + `transfer_user_id` |
| 退回 | worker | `POST .../tasks/rollback` | + `node_ids` |
| 加签 | worker | `POST .../tasks/add_sign` | + `add_sign_user_ids`/`add_sign_type`/`approval_method?` |

### 6.3 列表与分组

- 服务端循环 `GET /tasks` 的 `page_token`，最多 10 页（1000 条）；超过则返回 `truncated=true`，前端提示「仅显示前 1000 条，请按定义筛选或分批处理」。
- 每条映射：`task_id`、`instance_code`、`definition_code`、`definition_name`、`title`、`initiator_name`、`summaries`、`support_api_operate`、`link`、`instance_external_id`、`task_external_id`、`status`。
- 分组：按 `definition_code` + `definition_name`；前端左侧定义列表显示数量，点击过滤。
- 可操作性：`support_api_operate=false`、`instance_external_id`/`task_external_id` 非空、`status` 不是 `PENDING` 的任务禁止勾选，展示原因。`support_api_operate` 字段缺失时按 `true` 处理，写失败再按错误码提示（Phase 0 确认字段是否恒在）。
- 列表不缓存（v1）；刷新按钮重新拉取。若后续发现频繁切换定义导致重复请求，再加 30 秒/用户的短缓存。

### 6.4 详情与退回节点

- 详情只用于展开的任务/批量确认前的抽查；批量列表本身不逐条拉详情。
- 服务端以用户 token 调 `/instances/detail`；拿到详情后再校验当前用户是该实例的参与者（`tasks`/`operation_records` 中出现的 `open_id` 包含当前绑定），这是用户 token 之外的纵深防御。
- 退回节点：
  - 单条退回：前端展示可选节点（发起人 `START` + 详情中的历史通过节点），用户选具体 `node_id`；
  - 批量退回：只支持 `initiator`（发起人）与 `previous`（上一节点）；worker 对每个实例拉详情，按 `current_nodes`/`operation_records` 计算目标 `node_id`；无法唯一确定时该条标 `failed`，文案提示「请单独处理」；
  - **不要**把经典 `task_def_key_list`（`APPROVAL_x_y`）传给用户态 `rollback` 的 `node_ids`；两套标识不通用。
- 条件表单（1390001）、手写签名（1390018）：列表/详情能识别的（`support_api_operate=false`、外部审批）在 job 预检直接跳过并给出原因；只有写时才暴露的，item 标 `failed` 并提示「请单独处理」，不把整批拖失败。

### 6.5 创建 job 的校验

`POST /jobs` 请求体（`items` 必须同时带 `task_id` 与 `instance_code`，不能只传 task_id）：

```json
{
  "action": "pass|refuse|forward|rollback|add_sign",
  "items": [{ "task_id": "…", "instance_code": "…" }],
  "comment": "…",
  "transfer_open_id": "ou_…",
  "add_sign_open_ids": ["ou_…"],
  "add_sign_type": 1,
  "approval_method": 1,
  "rollback_mode": "initiator|previous",
  "idempotency_key": "uuid"
}
```

在服务端做以下校验，全部通过才建 job：

1. `action` 合法；`items` 1–50 条、按 `task_id` 去重；`idempotency_key` 必填且唯一（重复键返回已有 job）。
2. 重新拉取当前用户的新鲜 `/tasks` 列表，按 `(task_id, instance_code)` 匹配；客户端提交的 title/definition 只作展示，不信任。
3. 每条任务：`support_api_operate=true`、非外部审批、`status=PENDING`、不属于自动通过/自动拒绝节点。
4. 参数：`refuse` 的 comment 必填；`forward` 的 `transfer_open_id` 必填且不是本人；`add_sign_open_ids` 1–10 人、去重、不是本人；`add_sign_type`/`approval_method` 在允许枚举内；`rollback_mode` 在 `initiator`/`previous` 内。
5. 目标用户：用 `search/v1/user` 的结果或服务端再校验同租户、未离职/未冻结（以飞书返回为准）；不信任前端只传来的姓名。
6. 建 job + items 在同一事务；审计 `feishu.batch.job_created`。

### 6.6 worker、幂等与错误处理

worker 放在 `src/infrastructure/`（与现有审批 worker 同层），随应用启动：

- 轮询 `pending` job；用条件更新把 job 置 `running`，逐条把 item 置 `running`；同一 job 串行，避免对同一用户并发写。
- 每个 job 开始前再拉一次 `/tasks`，任务已不在待办 → item `skipped`（「任务已被处理」），不发写请求。
- 写请求 `idempotent=false`，**不自动重试**；仅对限流（429/230020）等待后重试同一 item，最多 3 次。
- 网络超时/5xx：item 标 `unknown`；不重发。后续用 `/instances/detail` 核对：任务已不在待办 → 保持 `unknown` 并提示「请刷新确认」；仍待办 → 可让用户重试（新 job）。**不要**在没有证据时标 `succeeded`。
- 1395001（任务状态已变化/参数问题）：拉详情核对；任务已非 PENDING → `skipped`；仍 PENDING → `failed` 并带飞书 message。
- 99991672/99991679/20026/20037/20064/20073：整个 job 立即 `failed`，item 标 `failed`，文案引导重新授权/联系管理员；不再继续烧配额。
- 1390018/1390001：item `failed`，文案「该审批需要手写签名/条件表单，请单独处理」。
- 取消：job `cancelled`；`pending` item → `skipped`；`running` item 允许完成，避免半写状态。
- 重启恢复：启动时把超过 5 分钟仍 `running` 的 item 置 `unknown` 并核对详情；绝不盲目重放。
- 限速：全局令牌桶（pass/refuse/forward 100/min；rollback/add_sign 1000/min），单用户串行；先保证正确性，再考虑多用户公平调度。
- 审计：job 创建/完成/取消；逐条结果落 `feishu_batch_job_item`（含 result_code/message），不把 token 写日志。

状态机：

```
job:  pending ──► running ──► succeeded | partial | failed | cancelled
item: pending ──► running ──► succeeded | failed | unknown | skipped
```

### 6.7 同步 MVP（可选，排期紧时）

`POST /api/v1/feishu/batch-approval/execute`：同样的校验与写调用，单次最多 20 条，串行执行，返回逐条结果；不建 job 表。适合先上线验证，但超过 20 条或需要取消/进度/重启恢复时必须换 job。**生产推荐直接做 job**。

### 6.8 前端页面

| 页面/组件 | 位置 | 要点 |
|---|---|---|
| 登录页飞书按钮 | `features/auth/pages/LoginPage.tsx` | 挂载时取 `GET /api/v1/auth/feishu/status`，`enabled=false` 时隐藏；`<Button asChild><a href="/api/v1/auth/feishu/authorize?redirect=...">飞书登录</a></Button>`；不调飞书 API，整页跳转 |
| 首次登录选择页 | `features/auth/pages/FeishuChoosePage.tsx` | 公开路由 `/login/feishu/choose`；读 `token`/`redirect`；展示飞书姓名/脱敏邮箱；两个动作：绑定已有账号（账号密码 + MFA 对话框）、新建账号（二次确认）；成功后 `controller.beginSession` + 跳转 |
| 飞书登录 MFA 页 | `features/auth/pages/FeishuMfaPage.tsx` | 公开路由 `/login/feishu/mfa`；读 `token`；TOTP/恢复码/备用邮箱验证码；成功后 `beginSession` + 跳转 |
| 批量审批页 | `features/feishu/views/BatchApprovalPage.tsx` | 根层 `RequireAuth`，不套 `AppLayout`；左侧定义分组，右侧任务表；工具栏同意/拒绝/转交/退回/加签；详情用 Dialog（不用 Drawer/Tabs） |
| 批量审批 API | `features/feishu/batch-approval-api.ts` | `requestWithTokenRefresh` + `credentials:"include"`；错误码映射；不 import 其他 features |
| 飞书选人器 | `features/feishu/components/FeishuUserPicker.tsx` | 防抖调用 `/users/search`；显示姓名/部门；不能复用账号域的 `UserPicker`（那是系统账号数字 id） |
| 任务进度/结果 | `features/feishu/components/BatchJobDialog.tsx` | 轮询 `GET /jobs/{id}`；逐条成功/失败/未知；「重试失败项」建新 job |
| 账号设置绑定区块 | `features/account/AccountSettingsPage.tsx` + `features/account/feishu-identity-api.ts` | 显示已绑定姓名/时间；`/auth/feishu/status` 关闭时隐藏绑定入口；绑定→Step-up（有密码/TOTP）→取 authorize URL→整页跳转；解绑→Step-up；账号域不能 import 飞书域 |
| 登录重定向 | `frontend/src/shell/auth-gate.tsx` | `RequireAuth` 跳 `/login?redirect=<当前路径>`；登录页已有 `safeRedirectTarget` |

约束：

- `features/account` 与 `features/feishu` 不能互相 import；绑定 API 放账号域，批量 API 放飞书域。
- 生产 CSP 禁止外域图片：飞书头像用姓名首字/占位，不直接 `<img src="https://...feishu...">`。
- 批量页的所有写操作都要二次确认（动作、条数、目标人）；加签/退回默认逐条确认或更强提示。
- 任务列表为空、凭证过期、scope 缺失、部分失败、job 取消都要有明确空态/错误态。
- 批量页是独立页（无 AppLayout），需要「当前用户 + 退出登录 / 返回系统」入口；退出必须调现有 `/api/v1/users/logout`，不能只清前端状态。

---

## 7. 配置、权限与部署

### 7.1 新增配置

`FeishuSettings` 目前 `deny_unknown_fields`，新增键必须同步 `src/config/mod.rs`、`config.show.toml`、`deploy/README.md` 的合法键清单。

| 键 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `feishu.login_redirect_uri` | String 可选 | 无 | 飞书登录/绑定回调地址，必须与开发者后台注册值**逐字一致**；例如 `https://<host>/api/v1/auth/feishu/callback`。未配置则登录端点不注册 |
| `feishu.login_enabled` | bool | `false` | 总开关；关闭时 `authorize/callback/choose*/mfa` 不注册，`status` 仍注册并返回 `false`（与现有 `oauth_redirect_uri` 独立） |
| `feishu.login_allow_create` | bool | `true` | 选择页是否显示「新建账号」；`false` 时只能绑定已有账号或联系管理员 |
| `feishu.login_allowed_tenant_keys` | Vec<String> | `[]` | 非空时只允许这些 `tenant_key` 登录；空表示信任应用所属租户 |
| `feishu.login_allowed_redirects` | Vec<String> | `["/", "/batch-approval", "/account"]` | 登录后回跳的相对路径白名单；不接受绝对 URL/`//` |
| `feishu.batch_max_tasks` | Int | `50` | 单 job 最大任务数；有效范围 1..=100 |
| `feishu.batch_worker_enabled` | bool | `true` | 批量 worker 启动开关；关闭后 job 保留 `pending`，不执行 |
| `feishu.batch_worker_interval_seconds` | Int | `2` | worker 轮询间隔；有效范围 1..=60 |

> `app_id`/`app_secret` 复用现有配置；`app_secret` 继续只走 secret 文件，不新增环境变量入口。
>
> 文档缺口：现有 `oauth_redirect_uri` 在 `docs/contracts/CONFIGURATION.md` 没有条目（只有源码注释与设计文档），本次一并补上，并新增 `login_redirect_uri`/`login_enabled`/`login_allow_create` 等条目。`config.example.toml` 因精确长度断言刻意不含 `[feishu]`，不要往那里加；改 `config.show.toml` 与 `deploy/README.md` 的合法键清单。

### 7.2 飞书开发者后台

1. **权限管理**：开通 `approval:task:read`、`approval:task:write`、`approval:instance:read`；确认 `contact:user:search`、`contact:contact.base:readonly`、`offline_access` 已开通。创建版本 → 发布 → 租户管理员审批。
2. **安全设置 → 重定向 URL**：新增 `feishu.login_redirect_uri` 的完整值（含 scheme/host/port），与配置逐字一致。现有管理侧 `oauth_redirect_uri` 保留，不删。
3. **应用可用范围/数据权限**：覆盖需要使用批量审批的员工；通讯录范围覆盖转交/加签可能选到的人。
4. **不需要**新增事件订阅；现有事件订阅/派发/回写不受影响。

> 20027 = 授权页 scope 不在后台已开通集合；99991679 = 用户 token 缺 scope（授权时没请求或旧 token 不含）。两者处置不同：前者找管理员开权限，后者重新授权。

### 7.3 权限清单汇总

| 类别 | 权限 | 用途 |
|---|---|---|
| 飞书用户 scope | `approval:task:read` | 拉本人待办 |
| 飞书用户 scope | `approval:task:write` | 同意/拒绝/转交/退回/加签 |
| 飞书用户 scope | `approval:instance:read` | 实例详情 |
| 飞书用户 scope | `contact:user:search` | 转交/加签选人 |
| 飞书用户 scope | `contact:contact.base:readonly` | 通讯录基础信息 |
| 飞书用户 scope | `offline_access` | refresh_token |
| 系统 | 无新权限键 | 批量 Action `authenticated`；账号绑定 Action `authenticated`；解绑 Step-up |
| 系统管理员 | 现有权限不变 | 首个管理员仍需账号密码注册；不给新建账号用户任何管理权限 |

### 7.4 部署与 TLS

- **TLS 是 P0 前置**：OAuth code、`yang_refresh` Cookie、飞书 token 不能走明文 HTTP。当前裸 IP + 18654 只能作为内网/联调；对公网开放前必须上域名 + HTTPS，并同步修改 `feishu.login_redirect_uri`、管理侧 `oauth_redirect_uri` 与开发者后台注册值。
- Nginx：现有 SPA 回退已覆盖 `/batch-approval`、`/login/feishu/choose`、`/login/feishu/mfa`；`/api/v1/auth/feishu/*` 与 `/api/v1/feishu/batch-approval/*` 走现有反代。
- 反向代理的 `proxy_read_timeout` 对批量 job 不敏感（job 是异步轮询）；同步 MVP 仍受 60s 限制。
- 回滚：关闭 `feishu.login_enabled` 即停止新登录/绑定；新增表是 additive，不影响现有派发/回写；批量 worker 可通过配置停止。

---

## 8. 安全设计与威胁模型

### 8.1 P0（上线阻断）

| 风险 | 处置 |
|---|---|
| 明文 HTTP 暴露 OAuth code / 会话 Cookie / 飞书 token | 上 TLS；未上 TLS 不对外网开放 |
| 选择页新建触发 `system_owner`，任意租户成员成为系统管理员 | 新建路径不调用 `claim_system_owner`；登录前检查 `system_owner` 已存在；加单测钉死 |
| 飞书登录绕过 TOTP | 已激活 TOTP 必须走 `/auth/feishu/mfa` 第二因子；集成测试覆盖 |
| 绑定/解绑被会话劫持利用 | 账号设置绑定要求已登录 + state 绑定 user_id + nonce，有密码/TOTP 的账号还要 Step-up（无密码账号降级为当前会话 + 二次确认）；首次登录选择页绑定要求 pending token + nonce + 账号密码（+MFA）；解绑要求 Step-up；冲突明确拒绝 |
| 选择页新建被 CSRF/重放 | pending token 一次性 + nonce Cookie；GET 只读不消费，POST 成功才原子消费；新建按钮二次确认；仅凭 URL 不能建号 |
| 批量写重复审批 | 写请求不自动重试；超时 `unknown`；job item 唯一键；worker 重启恢复不重放 |

### 8.2 登录 CSRF / 授权码安全

- **state**：一次性、10 分钟、服务端内存；包含 `flow`、`user_id`（绑定）、`redirect`、`nonce_hash`、`code_verifier`。
- **nonce Cookie**：登录开始时种 `HttpOnly; SameSite=Lax; Path=/api/v1/auth/feishu; Max-Age=600`（HTTPS 加 `Secure`）；回调比对 state 里的 `nonce_hash`。SameSite=Lax 是必须的：回调是跨站顶层导航，Strict 不会带上。
- **PKCE S256**：`code_verifier` 只存 state 记录，不经过浏览器；回调换码时携带。
- **state 存储上限**：内存表加最大条目数（如 10k）与定期清理，防止攻击者反复打 `/authorize` 撑爆内存；`/authorize` 按 IP 限流。
- **回跳白名单**：只接受配置中的相对路径；拒绝 `//evil.com`、`http(s)://...`、`\` 等变体。
- **不要对回调调用 `BrowserSession::validate_same_origin`**：飞书回调是跨站顶层导航，`Sec-Fetch-Site: cross-site` 会被现有同源校验拒绝；回调的 CSRF 防护由 state + nonce 承担。登录发起端点同理，不依赖同源校验。
- **回调不回显入参**：错误只给短码，code/token 不写日志、不进 URL（链接页的 pending token 是短时一次性，单列处理）。

### 8.3 账号接管

- 不按邮箱自动绑定；邮箱命中只作为选择页提示，必须再验账号密码（+MFA）。
- 选择页绑定的密码校验复用 `PasswordEngine::verify_or_dummy` 与 `AuthOperation::Login` 限流，保持等时与防枚举；目标账号由用户输入账号标识确定，密码证明归属，不接受客户端直接指定 user_id。
- 选择页新建必须显式点击 + 二次确认；pending token 绑定 Feishu 身份 + nonce Cookie，单独拿到 URL 不能建号。
- 绑定冲突（飞书身份已绑他人 / 用户已绑其他飞书）明确拒绝，不覆盖。
- 新建账号的用户名由用户输入，复用 `normalize_username`；唯一冲突返回字段错误，不自动改名。
- `users.email` 不写飞书邮箱，避免未验证邮箱被用于密码找回/邮箱登录。

### 8.4 IDOR 与 job 授权

- 所有 job 查询/取消按 `user_id = 当前会话用户` 过滤；`job_id` 不是授权凭据。
- 创建 job 时服务端重新拉 `/tasks` 匹配 `(task_id, instance_code)`，不接受任意 task_id。
- 详情接口用用户 token 调飞书，并校验详情参与者包含当前 open_id。
- worker 每条写前确认用户 active、identity active、credential active；解绑/停用后未开始项 `skipped`。

### 8.5 令牌存储与刷新

- `feishu_user_credential` 的 token 列为 `secret`，仅 SYSTEM 可读写；不返回前端、不写日志、不进 URL。
- 刷新令牌一次性：每 `identity_id` 行锁 + 进程内每用户互斥，锁后复查有效期；并发刷新会撞 20073，必须串行。
- 刷新请求 `idempotent=false`；响应丢失不重发，标 `reauth_required`。
- 365 天硬红线：`authorized_at + 365d` 后刷新报 20037；提示重新授权。刷新不重置该时钟。
- 多实例：进程内锁/state 只保证单实例；多实例前换 Redis 锁与 Redis state（与现有单管理员 OAuth 的已知缺口一致）。

### 8.6 限流、审计与日志

- 登录/绑定/回调按 IP + open_id 限流；`AuthOperation::Login` 预算复用；MFA 走 `TotpVerify`。
- 批量 job 按用户限流；飞书侧 100/min/1000/min 由全局令牌桶兜底。
- 审计：登录成功/失败、新建账号、绑定/解绑、重新授权、job 创建/完成/取消、逐条结果（job item 表）；不记录 token/code/密码。
- 日志：Feishu 错误码与 request_id 可记录；授权码、access/refresh token、Cookie 一律脱敏。
- 前端：form 值按纯文本渲染；外链只允许 http(s)；不拼接 HTML。

### 8.7 已知风险与接受项

| 风险 | 接受/处置 |
|---|---|
| 选择页新建 = 任意租户成员可创建系统账号 | 这是需求本身；用租户校验 + `login_allow_create` 开关 + 限流收口；如需更严，后续加部门/邮箱域白名单或管理员审批 |
| 新建账号不写 `users.email`，忘记密码不能自助找回 | 用户可在账号设置绑定并验证邮箱后自助找回；否则管理员签发重置凭证兜底 |
| 飞书文档与真实响应不一致 | Phase 0 真机逐条验证；以真机为准，不按文档猜 |
| 用户态 API 可能在租户侧不可用 | Phase 0 验证；失败则降级经典租户态（需额外 tenant 权限与 add_sign 特殊防护），见 §10 |
| 进程重启丢 state/pending token | 失败闭合：用户重新点登录/绑定；不持久化短时状态 |
| 多实例部署 | v1 单实例假设；多实例前换 Redis |
| 飞书头像无法直接展示（CSP） | v1 姓名首字/占位；后续做同源代理再评估 |
| 批量退回「上一节点」语义因实例而异 | 无法唯一确定时该条失败并提示单独处理；不猜节点 |
| 飞书应用重建导致 `open_id` 变化 | 绑定表同时存 `union_id` 作为迁移线索；v1 不做自动迁移，重建前导出映射或让用户重新绑定 |

---

## 9. 实施计划与验收

### Phase 0：真机验证（阻塞编码）

用真实租户 + 真实待办逐条打，记录请求/响应：

1. `GET /approval/v4/tasks?topic=1`（user token）返回 `instance_code`、`task_id`、`definition_code/name`、`support_api_operate`、`summaries`、`link`。
2. `GET /approval/v4/instances/detail?instance_code=...`（user token）返回 `current_nodes`/`operation_records`/`tasks`/`form`；确认只有参与者能看。
3. `POST /tasks/pass` / `refuse` / `forward` 用 user token 成功；确认 body 不需要 `user_id`/`approval_code`。
4. `POST /tasks/rollback` 的 `node_ids` 从详情取，验证 `initiator`/`previous` 两种模式。
5. `POST /tasks/add_sign` 用 user token 成功；确认 `add_sign_type` 1/2/3 与 `approval_method` 1/2/3 的真实含义。
6. `GET /search/v1/user` 返回 `open_id/name/avatar/department`；确认转交/加签目标可解析。
7. 确认 `approval:task:read`/`approval:task:write`/`approval:instance:read` 在控制台可开通、授权页不报 20027。
8. 验证 `authen/v1/user_info` 返回 `tenant_key`、`email`、`avatar_url`、`union_id`；确认登录 scope 不需要额外邮箱权限。若返回 99991679 且点名邮箱权限，补 `contact:user.email:readonly` 并重新授权，同时更新 §5.1 的 scope 表。
9. 若用户态任一接口不可用 → 记录失败码，评估降级经典租户态（§10）。

**验收**：上述 1–8 全部有真实响应样本；不确定项写成「已知缺口」，不进入编码。

### Phase 1：账号身份底座

- `user_identity` 表 + Repository + 迁移（声明式）；注册进 `src/infrastructure/schema.rs::infrastructure_definitions()`（当前 9 张，改数组长度与 `schema.rs:358-375` 测试断言），**不建 UI module**。
- `feishu_user_credential` 表 + Repository + 刷新器（复用现有 OAuth HTTP 函数）；`FeishuContext` 增 Repository。
- account 域暴露 `ExternalLoginPort`：按外部身份查找、按已验证邮箱查找、新建账号、绑定、解绑、会话签发复用、`has_system_owner`。
- `src/app.rs` 调整装配顺序：先构造 account 端口，再注入 feishu `build_addon`；`account::build_addon` 拆出 `Arc<Account>` 构造。
- 提升 `record_login_session` 可见性（或移进 `Account`）；lib_yang 补「302 + Set-Cookie」公开方法。
- 单测：用户名/密码校验、唯一冲突、邮箱命中提示、绑定冲突、选择页新建不触发 `system_owner`、刷新串行。

**验收**：集成测试用 ScriptedTransport 模拟飞书，首次登录进入选择页、选择页新建、选择页绑定、二次登录复用、绑定/解绑、冲突拒绝全部通过；`system_owner` 不变。

### Phase 2：飞书登录与绑定

- `/api/v1/auth/feishu/authorize|callback|choose|choose/bind|choose/create|mfa`；state/nonce/PKCE；302 错误页。
- `/api/v1/users/me/identities*`；解绑 Step-up。
- 登录页飞书按钮、账号选择页、MFA 页、账号设置区块；`RequireAuth` 带 `?redirect=`。
- 登录事件/审计；失败文案。

**验收**：首次飞书登录无绑定进入选择页；选择页绑定已有账号成功；选择页新建账号成功；邮箱命中只做提示、不自动绑定；TOTP 账号必须过第二因子；选择页 token 重放/无 nonce/换 Feishu 身份全部拒绝；state 重放/nonce 不匹配/租户拒绝/停用账号全部拒绝；e2e 用路由 mock 覆盖。

### Phase 3：批量审批只读

- `/batch-approval/tasks` 列表 + 分组 + 详情 + 用户搜索；凭证刷新/失效文案。
- 前端页面：定义分组、任务表、详情 Dialog、空态/错误态。
- 列表分页聚合上限与 `truncated`。

**验收**：真实租户能看到自己的待办并按定义分组；无绑定用户看到重新授权引导；scope 缺失文案正确；列表不串用户。

### Phase 4：批量写与 job

- job/item 表；创建/查询/取消；worker；限流/幂等/unknown/取消/重启恢复。
- 同意/拒绝/转交/退回/加签五种动作；前端确认对话框/进度/逐条结果/重试失败项。
- 审计与保留期清理。

**验收**：50 条 job 不超时；逐条结果可追溯；重复提交幂等；超时标 `unknown` 不重发；取消只停未开始项；worker 重启不重复审批；五种动作在真实租户各跑通一次（测试单据）。

### Phase 5：加固与部署

- TLS、域名、重定向 URL、scope 发布、secret 检查。
- 安全测试：登录 CSRF、开放重定向、账号接管、IDOR、token 泄漏、限流、MFA 绕过。
- 文档/运维手册：`deploy/README.md` 合法键、`docs/contracts/CONFIGURATION.md`（并补上现有 `oauth_redirect_uri` 缺口）、手动操作清单。
- 同步 `AGENTS.md` 的 feishu addon module 列表（新增 `sso`、`batch_approval` 与三张纯表 module）；同步 `account-system-roadmap.md` E-3 状态（从端口预留到已实施）。

**验收**：HTTPS 下全流程通过；安全用例全绿；回滚开关演练。

### 测试矩阵（摘要）

| 层 | 用例 |
|---|---|
| 后端单测 | 用户名/密码校验、state/nonce/PKCE、profile 映射、job 参数校验、错误码映射、刷新串行 |
| 后端集成 | 选择页新建、选择页绑定、二次登录、邮箱命中提示、绑定冲突、解绑、TOTP 两段、停用/删除、state 重放、租户拒绝、job 成功/失败/unknown/取消/重启、IDOR |
| 前端单测 | 登录页飞书按钮、账号选择页（绑定/新建/提示）、MFA 页、账号设置绑定区块、批量页选择/对话框/结果、路由重定向 |
| e2e | `page.route` 模拟 `/api/v1/auth/feishu/*` 与批量端点；不跑真实飞书 OAuth；断言 token 不进 Web Storage |
| 真机 | Phase 0 探针 + Phase 4 五种动作各一次（测试单据） |

---

## 10. 降级：经典租户态（仅当用户态 API 不可用）

如果 Phase 0 发现用户态接口在租户侧不可用/被下线，降级方案：

- 列表：优先 `GET /tasks`（user token，`approval:task:read`）；若该接口也不可用，则退回 `POST /tasks/search`（tenant token + 相应权限）——这条降级路径必须重新确权，不能假设与用户态响应同形。
- 新增 tenant 权限：`approval:approval:readonly`（详情）、`approval:task`（approve/reject/transfer/specified_rollback；已有 `approval:approval` 可覆盖）、`approval:approval:readonly`（add_sign）。
- 服务端必须做**所有权校验**：写前拉详情，确认 `task_list` 中存在 `(task.id=task_id, user_id=当前用户)` 且 `status=PENDING`；add_sign 尤其关键，因为经典请求体没有当前审批人字段。
- 批量加签默认**禁用**，或强制逐条确认 + 同锁所有权校验 + 审计；这是降级方案的已知弱化。
- 退回节点用 `timeline[].node_key`（`type=PASS`，发起人 `START`），不要用 `task_list[].node_id`。

降级方案的权限清单与风险必须在实施前重新评审；它不是本方案的主路径。

---

## 11. 开放决策（附建议）

| # | 问题 | 建议 |
|---|---|---|
| 1 | 选择页「新建账号」是否默认开启 | 默认开启（`login_allow_create=true`）；受限部署可关闭，只允许绑定已有账号 |
| 2 | 是否允许任意租户成员新建账号 | 默认允许（`login_allow_create=true`）；后续按部门/邮箱域白名单或管理员审批收紧 |
| 3 | 飞书登录是否必须过 TOTP | 必须；不能绕过 |
| 4 | 邮箱命中已有账号时是否自动绑定 | 不自动；选择页提示建议绑定，用户显式选择；密码+MFA |
| 5 | 新建账号是否写飞书邮箱到 `users.email` | 新建账号时不写；用户后续在账号设置绑定并验证邮箱后可写入 |
| 6 | 绑定/解绑是否要求 Step-up | 有密码/TOTP 的账号绑定与解绑都要求 Step-up；历史无密码账号降级为当前会话 + 二次确认并记录风险 |
| 7 | 批量审批是否需要权限键 | v1 不需要（authenticated-only）；若需灰度再加 `feishu.batch_approval.use`，并给新建账号用户默认授予 |
| 8 | 批量写同步还是异步 | 生产异步 job；同步 20 条仅 MVP |
| 9 | 批量退回是否支持「上一节点」 | 支持，但按实例解析；无法唯一确定时该条失败，不猜 |
| 10 | 是否为全员做飞书 token 每日保活 | v1 不做；登录即授权，按需刷新；过期引导重新授权 |
| 11 | 多实例部署 | v1 单实例；多实例前把 state/锁换 Redis |
| 12 | 飞书头像 | v1 姓名首字/占位；后续同源代理再评估 |

---

## 12. 附录

### A. 用户态 API 请求/响应要点（Phase 0 校准）

```text
GET /open-apis/approval/v4/tasks?topic=1&page_size=100&user_id_type=open_id
→ data.tasks[]: task_id, instance_code, definition_code, definition_name,
  title, initiator_name, summaries[], support_api_operate, link,
  instance_external_id, task_external_id, status
  data.has_more, data.page_token

GET /open-apis/approval/v4/instances/detail?instance_code=...&user_id_type=open_id
→ data: instance_code, approval_code/definition_code, status, form,
  current_nodes[], operation_records[], tasks[]

POST /open-apis/approval/v4/tasks/pass
{ "instance_code": "...", "task_id": "...", "comment": "...", "form": [...]? }

POST /open-apis/approval/v4/tasks/refuse
{ "instance_code": "...", "task_id": "...", "comment": "..." }

POST /open-apis/approval/v4/tasks/forward
{ "instance_code": "...", "task_id": "...", "transfer_user_id": "ou_...", "comment": "..." }

POST /open-apis/approval/v4/tasks/rollback
{ "instance_code": "...", "task_id": "...", "node_ids": ["..."], "comment": "..." }

POST /open-apis/approval/v4/tasks/add_sign
{ "instance_code": "...", "task_id": "...", "add_sign_user_ids": ["ou_..."],
  "add_sign_type": 1, "approval_method": 1, "comment": "..." }
```

> 以上字段以 Phase 0 真实响应为准；`add_sign_type`/`approval_method` 枚举含义、`node_ids` 形态、`form` 结构是重点校准项。

### B. 错误码处置

| 码 | 含义 | 处置 |
|---|---|---|
| 0 | 成功 | `succeeded` |
| 1395001 | 任务状态已变化/参数问题（不一定是「已被处理」） | 拉详情核对；非 PENDING → `skipped`；仍 PENDING → `failed` |
| 1390018 | 需要手写签名 | `failed`，提示单独处理 |
| 1390001 | 需要条件表单/参数 | `failed`，提示单独处理 |
| 99991672 | 应用未开通 scope | job `failed`，提示管理员开通 |
| 99991679 | 用户 token 缺 scope | job `failed`，提示重新授权 |
| 20026/20037/20064/20073 | refresh token 失效/并发刷新 | job `failed`，标 `reauth_required`，提示重新授权 |
| 99991663 | token 类型不支持 | 代码 bug/接口选错，告警 |
| 230020 / 429 | 限流 | 等待后重试同一 item，最多 3 次 |
| 5xx/超时 | 未知是否写入 | `unknown`，不重发，详情核对 |

### C. 文件落点（预估）

| 层 | 文件 |
|---|---|
| 后端 account | `src/addon/account/domain/identity.rs`（`user_identity` 表定义，进 `infrastructure_definitions()`）、`src/addon/account/domain/external_login.rs`（`ExternalLoginPort` 实现：查找/建号/绑定/解绑/会话签发） |
| 后端 feishu | `src/addon/feishu/sso/`（登录/绑定/回调/MFA actions + state）、`src/addon/feishu/domain/user_credential.rs`（每用户 token/刷新）、`src/addon/feishu/batch_approval/`（module/actions/domain/tables）、`src/infrastructure/feishu_batch_worker.rs` |
| 后端装配 | `src/app.rs`（先 account 端口、后 feishu）、`src/addon/feishu/mod.rs`（`FeishuContext` 增 3 个 Repository）、`src/infrastructure/schema.rs`（`user_identity` 进运行支撑表数组） |
| 前端 auth | `frontend/src/features/auth/pages/FeishuChoosePage.tsx`、`FeishuMfaPage.tsx`、`features/auth/api.ts` |
| 前端 account | `features/account/feishu-identity-api.ts`、`AccountSettingsPage.tsx` 新区块 |
| 前端 feishu | `features/feishu/views/BatchApprovalPage.tsx`、`features/feishu/batch-approval-api.ts`、`features/feishu/components/FeishuUserPicker.tsx`、`BatchJobDialog.tsx` |
| 前端 shell | `shell/routes.tsx`、`shell/auth-gate.tsx`（redirect） |
| 测试 | `tests/feishu_sso_integration.rs`、`tests/feishu_batch_approval_integration.rs`、`frontend/tests/features/auth/feishu-*.test.tsx`、`frontend/tests/features/feishu/batch-approval-page.test.tsx` |

### D. 部署手工清单

1. 飞书后台开通三个审批 scope + 确认通讯录/offline scope；发布应用版本；租户管理员审批。
2. 飞书后台「安全设置 → 重定向 URL」新增 `feishu.login_redirect_uri` 的完整值（逐字一致）。
3. 配置 `feishu.login_redirect_uri`、`feishu.login_enabled=true`；按需设置 `login_allow_create`、租户白名单、回跳白名单。
4. 确认 `feishu.app_id`/`app_secret` 已配置（secret 文件）；不要写入 Git。
5. 上 TLS + 域名；同步修改所有重定向 URL 与配置。
6. 确认系统已有首个管理员（`system_owner` 存在）；否则先注册首个账号。
7. 部署后用测试账号走一遍：首次飞书登录 → 选择页 → 绑定已有账号/新建账号 → `/batch-approval` → 列表 → 单条同意 → 批量 job → 结果。
8. 回滚：`feishu.login_enabled=false`；新增表保留，不影响现有派发/回写。

---

## 13. 自审记录

| 发现 | 修正 |
|---|---|
| 最初想把批量审批做成独立 Cookie 会话 | 接入飞书 SSO 后复用系统会话，删掉独立 session 表/CSRF 方案，减少一套认证面 |
| 最初想「无绑定时按邮箱命中自动路由，否则自动建号」 | 改为统一账号选择页：绑定已有/新建由用户显式选择；邮箱命中只做提示，避免误建号和重复账号 |
| 新建账号最初用生成用户名 + 随机密码 | 改为用户自行输入用户名/密码，与正常注册一致；新账号即有可用密码，不再需要 `password_set`/设置密码流程 |
| 选择页 token 只放 URL 不够 | 绑定 nonce Cookie + Feishu `open_id`/`tenant_key`；GET 只读，POST 成功才消费，单独拿 URL 不能建号 |
| 把飞书邮箱写进 `users.email` 会破坏成对 CHECK 并放大密码找回面 | 改为只存 `user_identity.external_email` |
| 飞书登录若直接签发会话会绕过 TOTP | 增加 `pending_login` + `/auth/feishu/mfa` 第二因子完成端点 |
| 经典租户态 add_sign 没有当前审批人字段 | 主路径坚持用户态；降级方案默认禁用批量加签 |
| 批量同步 50 条会撞 100/min 与 60s 超时 | 生产走异步 job；同步仅 20 条 MVP |
| 前端 CSP 禁止外域图片 | v1 不直接渲染飞书头像 |
| `RequireAuth` 不保留原始路径 | 批量页分享链接会回不到目标；实施时给 `RequireAuth` 加 `?redirect=` |
| SSO 端点最初想放 account 域 | 会形成 account → feishu 反向依赖；改为 feishu `sso` module + account 窄端口，`src/app.rs` 装配顺序调整 |
| 回调最初可能复用 `validate_same_origin` | 飞书跨站 302 的 `Sec-Fetch-Site: cross-site` 会被拒；改为 state+nonce，回调不调用同源校验 |
| 现有 `fetch_user_info` 只解析 `open_id`/`name` | SSO 扩字段前必须真机抓完整 `user_info` 报文，不按文档猜 |
| `user_identity` 最初按 UI module 设计 | 它是运行支撑表，进 `infrastructure_definitions()`，不建 module，避免为一张无 UI 的表造 Action |

# yang-system 全项目解析报告与修复方案

| 项目 | 内容 |
|---|---|
| 审查日期 | 2026-10-20 |
| 仓库 | `project/yang-system`（独立 Git/Cargo 项目） |
| 分支 / 基线 | `main` / `9f68346` |
| 审查范围 | 738 个受控文件（Rust 265、TS/TSX 232），覆盖 `src/`、`frontend/`、`tests/`、`scripts/`、`deploy/`、`docker/`、`ops/`、CI 与关键契约文档 |
| 审查方式 | 7 个领域并行静态审查 → 42 条高危/高置信中危对抗性核验 → 人工抽读关键实现复核 |
| 结果 | 66 条发现：11 高、38 中、17 低；40 confirmed、2 refuted、24 未送核验 |
| 代码改动 | 审查过程未修改任何代码；本文件为报告落盘，是唯一新增产物 |
| 修复决策 | 2026-10-20：P0-2（Step-up）、P0-9（Encrypt Key）、阶段 0 的 0.1（凭据轮换）暂不修复，保留为已知风险 |

> 安全边界：审查未读取 `config.toml`、`deploy/config.cloud*.toml`、`.env` 或任何密钥文件；未运行构建、测试、部署脚本或联网命令。报告中不复述任何密钥/Token/个人信息的实际值。

> **2026-10-20 决策**：P0-2（Step-up 接线）、P0-9（测试源码真实 Encrypt Key）与阶段 0 的 0.1（凭据轮换）暂时不修复；文档保留问题与提案，修复计划中标注为暂缓项。

---

## 一、结论摘要

项目整体工程质量高于平均：事务边界、`authz_version + Outbox`、append-only 审计、Schema 预检、xlsx 防御、部署契约门禁、前端分层都有明确设计。但当前存在 **4 组可串成完整攻击链的高危问题**，以及 3 个会直接破坏生产承诺的缺陷：

1. **组所有者提权链**：任何自助注册用户建组后，可把 `access.grants.write` 等非管理员等价权限装进组并拉同伙入组，随后相互授权，最终取得 `account.users.manage`、`feishu.approval.write` 等权限。与「应用内不存在自提权路径」直接矛盾。
2. **Step-up 完全未接线**：`change_email`、`totp_deactivate`、`delete_account`、`revoke_session`、管理员停用/签发重置凭证等只凭 Access Token 即可执行。窃取一次会话即可改绑邮箱 → 重置密码 → 再用同一邮箱接收 MFA 备用码，绕过 MFA 完成接管。（2026-10-20 决策：本轮暂不修复。）
3. **单设备撤销失效**：refresh 从不校验 `user_session.revoked_at`；撤销依赖 best-effort 的 jti 快照，并发轮换或会话行缺失时被撤销设备仍可无限续期。
4. **飞书公开面鉴权缺口**：事件接收端点 `verification_token` 只告警不拦截、配置了 Encrypt Key 仍接受明文；`portal/suggest` 绕过 `access_mode` 与限速；限速键用原始 slug，大小写变体可绕过口令爆破限速；派发 Token 明文落请求记录并随只读列表回显；测试源码提交了真实 Encrypt Key 与生产密文。（其中真实 Encrypt Key 项按 2026-10-20 决策暂缓修复。）
5. **财务生产默认明文公网**：`deploy-finance-prod.ps1` 的 `EdgeBindAddr` 默认 `0.0.0.0`，配合无 TLS 终止，登录密码、会话 Cookie、重置令牌明文传输。
6. **前端刷新 Token 未回写**：`acceptRefreshedTokenPair` 无生产调用方，刷新后请求仍带旧 token，请求翻倍，第二个 access TTL 到期即强制登出（refresh cookie 仍有效）。

**建议**：先做 P0 止血（公开端点 fail-closed、组授权闸门、会话撤销、部署默认值、前端会话回写），再按 P1 收敛数据一致性与前端/运维问题，最后处理低危与历史文档漂移。**按 2026-10-20 决策，P0-2（Step-up）、P0-9（Encrypt Key）与阶段 0 的 0.1（凭据轮换）暂不修复，作为已知风险跟踪。**

### 发现统计

| 等级 | 原始 | 核验后 |
|---|---:|---|
| 高 | 11 | 11 confirmed |
| 中 | 38 | 27 confirmed；7 未核验；2 refuted；2 降为低危（confirmed） |
| 低 | 17 | 17 未核验；另加 2 条由中危降级（confirmed） |
| 合计 | 66 | 40 confirmed；24 未核验；2 refuted |

---

## 二、项目全景

### 2.1 后端

| 层 | 目录 | 职责 |
|---|---|---|
| 组合根 | `src/app.rs`、`src/bootstrap.rs` | Addon 装配、中间件、Catalog 冻结、Schema 同步、Worker 启停 |
| 业务 Addon | `src/addon/account`（46 文件） | 注册/登录/会话/刷新/MFA/密码重置/邮箱换绑/匿名化删除/管理动作 |
| 授权管理面 | `src/addon/access`（40 文件） | 权限目录、直授、一层权限组、首账号引导、管理员等价闸门 |
| 飞书集成 | `src/addon/feishu`（114 文件） | datasource（push/pull/xlsx）、option（外部选项）、portal（公开查询门户）、event（审批事件）、approval（派发/回填/通知/OAuth） |
| 基础设施 | `src/infrastructure` | 授权版本 + Outbox、审计、声明式 Schema、四个 worker |
| 配置 | `src/config` | 配置合成、密钥 keyring、启动校验 |

**核心请求链**：`auth → authz_version → permission → Step-up guard（设计上）→ Action → 业务事实 + 版本 + Outbox/audit`。实际装配中 Step-up 环节缺失（见 P0-2）。

**数据一致性设计**：授权写入必须同事务更新业务事实、单调递增 `authz_version`、追加 Outbox；Redis 只做短 TTL 加速；审计 append-only；Schema 启动时先只读预检再增量同步。这部分实现整体扎实，核验未发现高危绕过。

### 2.2 前端

- `engine/`：会话状态机、HTTP 协议、zod/Ajv 契约、Catalog 投影、table/form/action 渲染器。
- `features/`：auth、account、access、feishu 四个业务域；`shell/` 负责路由、认证门控、应用外壳。
- 依赖方向基本成立（engine 不 import features/shell，`main.tsx` 唯一 `createRoot`），未发现 XSS sink 或凭据落盘。
- 主要问题在会话层（见 P0-11）与通用表格选择状态（P1）。

### 2.3 运维与门禁

- `scripts/run_ci.py` 统一 quick/full/integration；`check_architecture.py` 有单 Action、租户边界、裸 SQL、授权 writer、前端边界、lock-before-gate 等门禁。
- 部署为 PowerShell 打包上传 + 远端蓝绿 bash；前端 Nginx 与后端共享网络命名空间；CI 有 Nginx/Prometheus 只读校验。
- 门禁整体细致，但存在两个可静默失效的点：`--lib` 测试按名称过滤、lock-before-gate 在原始文本上匹配（P1）。

**整体优势**：事务/版本/Outbox 契约清晰；审计与 Schema 预检 fail-closed；飞书数据面防御意识强（字段绑定、跨源预检、空快照守卫、出站失败分类）；前端契约与部署门禁投入大。

**整体风险**：授权语义的组合性缺口、Step-up 契约漂移、飞书公开面鉴权不完整、单实例假设遍布 worker/OAuth state/导入互斥、部分关键路径缺端到端测试。

---

## 三、P0 高危发现（11 条，全部 confirmed）

### P0-1 组所有者可把任意非管理员等价权限装进自建组并转授

**ID**：`access-infra#9`
**位置**：`src/addon/access/groups/actions/add_group_item.rs:139`、`add_group_member.rs:212`、`create_group.rs:138-149`

**代码事实**：

- `create_group` 是 authenticated-only，任何登录用户可建组并成为 owner（`create_group.rs:139-149`）。
- `add_group_item` 的 G2 闸门只拦管理员等价权限；防自提权子集校验只在 `members.contains(&operator_id)` 时执行（`add_group_item.rs:139`）。owner 通常不是成员，因此可把任意已声明权限（如 `access.grants.write`）写进组。
- `add_group_member` 的子集校验只在 `input.user_id == operator_id` 时执行（`add_group_member.rs:212`）。owner 可把第二个账号加入组而不触发校验。
- `grant_permission` 只校验目录声明与管理员等价闸门，不要求操作者持有被授权限（`grant_permission.rs:54-91`）。

**利用链**：注册 A/B → A 建组 → A 加 `access.grants.write` → A 加 B → B 刷新后凭该权限给 A 授任意非管理员等价权限（含 `account.users.manage`、`feishu.approval.write`）。两个自助注册账号即可完成。

**修复**：

1. `add_group_item` 无条件执行委派子集校验：非 `system_admin` 组成员时，`input.permission` 必须已在该操作者的 `effective_permissions_of_in_tx` 集合内。
2. `add_group_member` 要求操作者有效权限覆盖目标组全部条目（或操作者为 `system_admin` 组成员）。
3. 补双账号端到端集成测试：owner 非成员 + 同伙入组 + 同伙授非管理员等价权限，必须在第二步被 403。

**验收**：删除任一新增校验，测试必须变红；`python scripts/run_ci.py integration` 通过。

---

### P0-2 高危账号操作完全没有 Step-up/二次认证

> **状态：暂不修复（2026-10-20 决策）。** 保留问题描述与修复提案，待产品决策后实施。

**ID**：`account#1`
**位置**：`src/addon/account/user/actions/change_email.rs:31`、`totp_deactivate.rs:25`、`src/app.rs:107-115`、`src/addon/account/user/mod.rs:62-69`

**代码事实**：

- `change_email` 只要求登录 + 新邮箱验证码，不要求当前密码、TOTP 或 Step-up（`change_email.rs:31-53`）。
- `totp_deactivate` 输入为空结构体，handler 不校验任何第二因子（`totp_deactivate.rs:25-56`），与其文件头「须出示第二因子」直接矛盾。
- 全仓 `src` 无 `StepUpMiddleware`/`StepUpCompleteAction` 构造或注册；`StepUpSettings::build_manager` 只在配置校验路径被调用后丢弃。
- `app.rs:107-115` 只给 account addon 挂 `ActionLogMiddleware`；`account/user/mod.rs:62-69` 只有 `TrustedClientIp + TokenAuth`。

**利用链**：窃取会话 → `change_email` 绑定攻击者邮箱 → 公开的 `request_password_reset` 向新邮箱发链接并重置口令 → 登录需要 TOTP 时，`request_mfa_email_code` 把备用码发到同一新邮箱 → 完成接管。`totp_deactivate` 更可直接关闭 MFA。

**待恢复时的候选修复**：

1. 在组合根构造 `StepUpManager`，注册 `StepUpCompleteAction` + `StepUpMiddleware`，为 `change_email`、`delete_account`、`revoke_session`、`totp_setup/activate/deactivate`、`admin_disable/enable_user`、`admin_issue_password_reset` 配置目标解析；proof 走 Redis 单次消费。
2. 若团队已决定系统级移除 Step-up（commit `807dc91`），则至少在这些 handler 内要求当前密码或 TOTP/恢复码，并同步删除文档与 `[step_up]` 配置声明，避免契约继续骗人。
3. `totp_deactivate` 必须校验 TOTP/恢复码。
4. 补集成测试：仅持 Access Token 无法完成 `change_email`/`totp_deactivate`。

**验收**：上述攻击链在第二步被拒；文档与运行期行为一致。

---

### P0-3 管理员等价清单漏掉 `feishu.approval.write`

**ID**：`access-infra#10`
**位置**：`src/addon/access/domain/sensitive_permissions.rs:50-67`

**代码事实**：清单只有 4 条（`account.users.reset_credentials`、`feishu.datasource.secret`、`feishu.datasource.write`、`feishu.portal.write`），不含 `feishu.approval.write`。但该权限可调用 `reveal_config_token` 回显派发明文、`rotate_config_token` 重置 Token；派发端点是 public，仅按 `token_hash` 验机器凭据。这与清单自身判据「获得或夺取其他主体的凭据/身份，或绕过其他一切授权检查」完全吻合。

**影响**：持 `access.grants.write`/`access.groups.write` 的委派者（或经 P0-1 提权后的任意用户）可不受 G2 闸门约束取得审批派发机器凭据。

**修复**：把 `feishu.approval.write` 加入 `ADMIN_EQUIVALENT_PERMISSIONS`；或把 reveal/rotate 拆到独立的 `feishu.approval.secret` 键并列入清单。同步 `docs/contracts/AUTHZ_GRANTS.md`、目录投影测试与 `the_permission_catalog_marks_exactly_the_admin_equivalent_permissions` 期望集；补「非 system_admin 不能授予/组内携带该键」的集成测试。

---

### P0-4 被撤销的单台会话仍可继续 refresh

**ID**：`account#2`
**位置**：`src/addon/account/domain/session/repository.rs:114-154`、`src/addon/account/user/actions/refresh.rs:65-82`、`src/addon/account/user/actions/revoke_session.rs:37-89`

**代码事实**：

- `claims_for_refresh` 只校验用户状态与 `credential_version`，从不读取 `user_session.revoked_at`（`context.rs:516-529`）。
- `touch_on_refresh` 在会话行缺失时直接 `return Ok(())`，UPDATE 也不带 `revoked_at` 条件（`session/repository.rs:114-154`）。
- `revoke_session` 先在无事务读里取 jti，再事务标记撤销，提交后才拉黑 jti；攻击者在窗口内并发轮换得到的新 refresh jti 永远不会入黑名单，之后可无限续期；行已 revoked 后再次撤销又因 `where_null(revoked_at)` 返回 401，用户无法补救。
- 超长 User-Agent 使 `user_agent VARCHAR(512)` 写入失败，而 `login.rs:218-229` 仅 warn 吞掉，形成确定性的「无会话行」路径；refresh 因缺行降级仍成功。

**影响**：「踢出该设备」在网络抖动、DB 写失败、超长 UA 或并发轮换下静默失效，被撤销设备可继续换取 Access Token，直到全局登出/凭据版本变更。

**修复**：

1. refresh 以 claims 里的 `session_id` 查询 `user_session`；行缺失或 `revoked_at` 非空即 fail-closed 拒绝。对无 `session_id` 的老 Token 做一次性兼容窗口并明确截止版本。
2. 把撤销状态写进随轮换延续的 Redis 键或会话版本，使新 jti 也失效；撤销操作可重入（已撤销时仍允许补拉黑）。
3. `record_login_session` 失败应重试或让登录失败；`user_agent` 入库前按列宽截断。
4. 补并发测试：撤销与刷新并发、会话行缺失、超长 UA。

---

### P0-5 事件接收端点 `verification_token` 只记日志不拦截

**ID**：`feishu-data#18`
**位置**：`src/addon/feishu/event/actions/receive_approval_event.rs:141-156`、`:95-139`、`event/mod.rs:17-28`

**代码事实**：token 不匹配只 `tracing::warn!` 后继续处理；请求体没有 `encrypt` 字段时直接采信明文，即使配置了 `event_encrypt_key`；两个端点都是 `public()`、无认证中间件。事件行由 `feishu_event_worker` 消费，会按 `instance_code` 反查 task、调用 `get_instance` 并回写 `instance_status`。

**影响**：任何能访问 `/api/v1/feishu/approval/event` 的匿名请求都可注入任意事件行，驱动 worker 出站调用并更新任务状态/回填记录；也可反复写入垃圾事件占满事件表与重试队列。配置错误时飞书合法推送也不会被拒。

**修复**：验签 fail-closed：先解密（若有 encrypt），再取 token 做常数时间比较，不匹配返回 401/403；配置了 `event_encrypt_key` 时拒绝未加密请求；补伪造 token 与明文注入的端到端拒绝测试。注意保留现有 5xx 重推语义，不要把可重试的解密故障误判成 4xx。

---

### P0-6 门户 `suggest` 绕过 `access_mode` 与限速

**ID**：`feishu-data#19`
**位置**：`src/addon/feishu/portal/actions/portal_suggest.rs:92-136`、`portal_query.rs:194-236`

**代码事实**：`portal_suggest` 只检查门户 `enabled` 与 `fuzzy_enabled`，随后直接按 `query_field_source_key` 做 LIKE 查询并返回 label；没有调用 `portal_query` 里的 `rate_limit::check_query` 与 `authorize`。端点 `.public()`，`portal/mod.rs:40-58` 在没有 Authorization 头时匿名放行。

**影响**：口令或登录模式的门户，其查询键（如卡号）可被匿名枚举：`GET /api/v1/feishu/portal/{slug}/suggest?query=4514` 返回匹配 label 列表；全表 `%...%` LIKE 且无限速，可放大数据库负载。

**修复**：把 `portal_query` 的判权与限速抽成共享函数，`suggest` 复用（先 `rate_limit::check_query`，再按 `access_mode` 判权；passphrase 门户要求随请求带口令，或仅对 public 门户开放 suggest）。补集成测试：passphrase/login 门户无凭证时 suggest 返回 403。

---

### P0-7 门户限速键使用原始 slug，大小写变体可绕过口令爆破限速

**ID**：`feishu-data#20`
**位置**：`src/addon/feishu/domain/rate_limit.rs:41-43`、`portal_query.rs:155-160,199,311-327`

**代码事实**：`query_key` 直接拼 `portal:query:{slug}:{client_ip}`；`find_portal` 用 `where_eq` 查询，而建表 DDL 固定 `COLLATE=utf8mb4_unicode_ci`，比较大小写不敏感——同一门户可用大小写变体命中，却落入不同计数键。判权（含 argon2 校验）在限速之后。

**影响**：单 IP 的猜测/CPU 预算可被放大到与变体数成正比；32 位十六进制 slug 也有大量变体，口令爆破保护失效。

**修复**：限速键改用已加载门户的主键 `portal:query:{portal_id}:{client_ip}`；若必须保留 slug，先做 ASCII 小写归一。补单测：`ABC` 与 `abc` 生成同一计数键。

---

### P0-8 派发 Token 明文写入请求记录并随只读列表回显

**ID**：`feishu-approval#30`
**位置**：`src/addon/feishu/approval/actions/dispatch.rs:128-150,939-947`、`request_log_table.rs:45-46`、`list_requests.rs:74,125`

**代码事实**：HTTP `DispatchInput` 含 `token: String` 且派生 `Serialize`；`write_request_log` 把整个输入序列化进 `request_body`；该列是普通 `Text`，未标 secret；`list_requests` 在 `feishu.approval.read` 权限下原样返回，前端 `ApprovalRequestsPage.tsx:322` 直接渲染。对照 `reveal_config_token` 的回显要求 `feishu.approval.write`，只读接口绕过了这道门。

**影响**：任何拿到 `feishu.approval.read` 的账号（以及能读库/备份的人）都能从请求记录取得当前有效的派发 Token，进而调用 public 的 dispatch 端点创建审批实例、回写多维表格。每点一次按钮就多一份明文，且该表没有保留期。

**修复**：落库前脱敏（序列化一份 token 替换为占位符的副本，或只存非敏感字段）；历史行中的明文与 Token 轮换按 0.1 暂缓项跟踪；补断言 `request_body` 不含 token 的回归测试。

---

### P0-9 测试源码提交了真实飞书 Encrypt Key 与生产密文

> **状态：暂不修复（2026-10-20 决策）。** 密钥泄露风险保留为已知风险；轮换与代码清理待后续决策。

**ID**：`feishu-data#21`
**位置**：`src/addon/feishu/domain/crypto.rs:484-508`

**代码事实**：测试硬编码一段 32 字符密钥与一段密文，注释明确写「真实飞书加密 challenge（从生产日志截取）」，并用 `derive_key(SHA-256)+AES-256-CBC` 成功解密。密钥进入版本库与 git 历史即应视为泄露。本报告不复述密钥值；核验未读取生产配置，无法确认它是否仍是当前生产值。

**待恢复时的候选修复**：删除真实密钥与密文，改为测试内随机生成 key + `encrypt_bytes` 现造密文做往返；若该值曾用于生产，立即在飞书开发者后台轮换 `event_encrypt_key`，并评估用 BFG/filter-repo 清理 git 历史（轮换优先，历史清理是破坏性操作，需单独确认）。

---

### P0-10 财务生产部署默认将边缘绑定 `0.0.0.0`，明文 HTTP 暴露公网

**ID**：`tests-ops#56`
**位置**：`deploy/deploy-finance-prod.ps1:143-154`、`deploy/deploy.ps1:115-124`、`deploy/README.md:280-288`

**代码事实**：两个入口的 `EdgeBindAddr` 默认值都是 `0.0.0.0`；每次把 `BIND_ADDR` 传给远端，覆盖 `deploy-blue-green.sh:82-90` 的 `127.0.0.1` 安全默认值；README 明确写当前是 `http://<公网IP>:18654` 明文直连；仓库没有 TLS 终止配置。

**影响**：公网路径上的中间人可直接读取登录密码、会话 Cookie（HTTP 下 Secure 不生效）和密码重置令牌。财务正式服务器使用该默认值，且不传参即暴露。

**修复**：默认值改为 `127.0.0.1`；公网访问改为显式 opt-in（如 `-ExposePublicly` 开关）；宿主上用受信 TLS 反代终止 HTTPS，只转发飞书外部选项所需路径。若已按 `0.0.0.0` 明文运行过，凭据轮换按 0.1 暂缓项处理，待后续决策。

---

### P0-11 刷新得到的 Access Token 未回写 SessionController

**ID**：`frontend-engine#41`
**位置**：`frontend/src/engine/session/auth-session.ts:82-95,112-138`、`session-controller.ts:71-73`、`frontend/src/shell/session-bridge.tsx:22-40`

**代码事实**：`refreshTokenPair` 拿到新 token 后只写模块级 `currentAccessToken` 并派发 `SESSION_REFRESHED_EVENT`；`acceptRefreshedTokenPair` 在 `src` 内没有调用者（仅测试命中）；`SessionBridge` 只监听 expired/relogin。所有请求的 Bearer 来自 `useSessionCredentials` 快照，即登录/恢复时的 T0：

- 第一次刷新后每个请求仍带 T0 → 401 → 刷新返回缓存的 T1 → 重试成功，请求量翻倍。
- T1 到期后同一路径返回已过期的 T1（不再刷新），重试 401 触发 `expireSession`，即使 refresh cookie 仍有效（30 天）也被强制登出。

**影响**：任何活跃超过约 2 小时的会话会被强制重新登录；刷新后所有 API 调用多一次 401 往返；授权/目录变更不会随刷新重新拉取。

**修复**：

1. 在 `SessionBridge`（或 `SessionController` 自身）订阅 `SESSION_REFRESHED_EVENT` 并调用 `controller.acceptRefreshedTokenPair(event.detail)`。
2. 更稳妥的是让 `useSessionCredentials`/请求路径使用 `activeAccessToken()`，避免快照 token 永远停在 T0。
3. 补两条回归测试：401→刷新后快照 token 必须变为新值；新 token 再次过期时仍能再次刷新而不是登出。

---

## 四、P1 中危发现（27 条 confirmed）

> 以下均已通过对抗核验；按领域分组。修复建议为核验后收敛的动作。

| ID | 位置 | 问题 | 修复要点 |
|---|---|---|---|
| account#3 | `user/actions/logout.rs:50` | 会话行永不清理，全设备登出不标撤销；设备列表长期展示幽灵设备并无限累积 IP/UA | 全量失效路径同事务写 `revoked_at`；增加 refresh 过期列与清理 worker；`active_count` 只统计可用会话 |
| account#4 | `user/actions/request_registration_email.rs:30` | 公开注册验证码端点无消费方，可对任意邮箱真实发信 | 删除 Action/ACTIONS 注册/默认 email.verification 槽/前端死函数；或真正接入注册流程并取消任意地址投递 |
| account#6 | `user/actions/lookup.rs:84` | 任意登录用户可批量获取全量邮箱，绕过 email 字段的 system-only 可读约束 | 声明 `account.users.read`；无权限调用只返回 id+username+status；按 email 搜索在服务端完成而不回显 |
| account#7 | `user/actions/totp_activate.rs:107` | 发布开关关闭时 TOTP/管理员启停仍递增 `credential_version`，导致后续 refresh 永久被拒 | 所有递增 `credential_version` 的 Action 统一受 `credential_mutations_enabled` 门控；或始终在 refresh claims 写该字段 |
| access-infra#13 | `access/domain/repository.rs:46` | 直授 `expires_at` 只在签发时过滤，已签发 Token 中的过期权限最长再存活一个 access TTL | 周期扫描过期直授并递增 authz_version+Outbox；或在 claims 携带权限级过期时间 |
| access-infra#14 | `config/mod.rs:853` | 默认 `logging.filter` 没有默认指令，`yang_base/yang_db/yang_runtime` 的 ERROR 也被丢弃 | 默认追加全局 `error`（或分 target warn）；同步示例配置；补 EnvFilter 单测 |
| feishu-data#22 | `portal/actions/portal_query.rs:471` | 子行取数硬截断 100 行，多命中时静默丢列值 | 按 `matched.len()*columns.len()` 分页取全并带稳定排序；或返回明确截断标记 |
| feishu-data#24 | `event/actions/receive_approval_event.rs:147` | 验签失败时把配置的 `verification_token` 明文写日志 | 只记布尔结果与出示值摘要前缀；绝不打印期望值；加日志内容断言 |
| feishu-data#25 | `datasource/actions/update_datasource_table.rs:163` | 更新接口允许改成 pull 却不校验坐标，源静默永不同步 | update 按合并后状态校验：pull 时 `base_token`/`table_id` 必须非空；与 create 共用校验函数 |
| feishu-approval#31 | `infrastructure/feishu_approval_worker.rs:701` | 任务表以全局唯一 `record_id` 作键，删配置重建后重新派发永久卡住播种 | 唯一键改 `(config_id, record_id)`；所有查询/更新带 config_id；补删配置→重建→重发测试 |
| feishu-approval#32 | `approval/actions/dispatch.rs:1602` | 显式 `notification_recipient` 绕过终态门，waiting/受理结论重复或超额发送 | 显式接收人也只允许终态发送；中间态统一交 worker；去重键纳入 state+record_id+config_id |
| feishu-approval#33 | `approval/domain/task_applicant_table.rs:45` | 读-改-插无唯一约束，并发写为同一任务留下两行 | `task_id` 唯一索引 + 原子 upsert；补并发测试 |
| feishu-approval#34 | `domain/approval_provision.rs:428` | 单配置时区未在配置期校验：同步派发报错、worker 静默回退默认值 | build_plan/create/update 统一校验 IANA 时区并拒绝非法值 |
| feishu-approval#35 | `domain/approval_provision.rs:473` | 六个扩展回填列与回填/申请人列不查重，后写覆盖先写 | `check_backfill_targets` 对全部目标按 field_id 判重，重复即配置期报错 |
| feishu-approval#37 | `infrastructure/feishu_approval_worker.rs:1232` | 中间态通知发送失败不重试，`last_error` 先落库导致去重永久跳过 | 仅在通知成功时记去重键；或加 `notified_at/notify_attempts` 并在失败后下一轮重试 |
| frontend-engine#42 | `engine/session/auth-session.ts:136` | 刷新成功后的业务 401 被当成会话失效，管理员操作失败会被登出 | 重试仍 401 时抛 `ApiError`，不 `expireSession`；会话失效只由 refresh 端点失败判定 |
| frontend-engine#43 | `engine/session/session-controller.ts:105` | Access Token 过期后退出登录/停用账号必然失败，本地会话与 refresh cookie 都未清 | 登出/停用走 `requestWithTokenRefresh`；`finally` 无条件 `clearSession + publishSessionEnd` |
| frontend-engine#44 | `engine/renderers/table/DataGrid.tsx:70` | 批量选择跨分页不清空且行键复用，批量操作可能命中错误记录 | 分页/查询变化时清空选择；行键改用记录主键；提交时按当前 rows 重映射 |
| frontend-features#49 | `features/access/components/PermissionPickerDialog.tsx:140` | 取消「永久有效」但不选日期仍提交永久授权；日期按 UTC 末日解析 | 非永久模式 `expiresOn` 必填；用本地日末 `new Date(y,m-1,d,23,59,59)` 换算 |
| frontend-features#52 | `features/feishu/components/FieldBindingsTable.tsx:53` | 字段树是不可聚焦的 div，键盘用户无法切换字段 | 行改 `<button>` 或补 `role/tabIndex/Enter/Space` 与 `aria-selected`；补键盘 e2e |
| frontend-features#53 | `e2e/table-view.spec.ts:54` | E2E 依赖共享内存后端执行顺序，CI 重试放大瞬时失败 | 每用例可重建独立种子数据/唯一名称；提供 demo reset 或重试前重建 webServer |
| tests-ops#57 | `ops/prometheus/yang-system.rules.yml:79` | readiness 告警依赖只在 `/health/ready` 被调用时更新的 gauge，拓扑没有周期探针 | 加 blackbox/systemd timer 周期探针，或后台任务周期写 gauge；补 absent/up==0 告警 |
| tests-ops#58 | `ops/prometheus/yang-system.rules.yml:9` | 可用性 SLI 分母包含 business_error，违背 SLO 口径 | 分母改为 `result=~"success|error"`；补「只有 business_error 不触发」用例 |
| tests-ops#59 | `deploy/deploy-blue-green.sh:462` | 清库失败路径回显含明文 DB 密码的 SQL，密码进入 `docker exec` argv | 用 defaults-extra-file/stdin 传密码；错误输出脱敏；URL 只打印 scheme/host |
| tests-ops#61 | `scripts/check_architecture.py:1305` | lock-before-gate 在 handle 原始文本上匹配，注释/字符串可伪造顺序 | 在等长 skeleton 上搜索 lock/gate；自测 fixture 补「注释/字符串伪装」形态 |
| tests-ops#62 | `scripts/run_ci.py:112` | 五个 `--lib` 测试按名称过滤，改名后 cargo 以 0 测试成功退出 | self_test 断言 filter 仍出现在源文件；或先 `--list` 断言匹配数 > 0；统一前缀 + `--exact` |
| tests-ops#63 | `deploy/README.md:218` | README 仍称清库后无管理员引导、需手工插授权 | 改为「首个成功注册账号自动成为 system_admin」；手工 SQL 仅灾难恢复 |

---

## 五、未核验 / 低危 / 已排除

### 5.1 未核验的中危（7 条，需人工确认）

| ID | 位置 | 问题 | 建议 |
|---|---|---|---|
| account#5 | `request_password_reset.rs:53` | 同步 SMTP 分支造成时序枚举 | 邮件投递移出请求路径，或补固定延迟 |
| access-infra#11 | `access/domain/groups/admin.rs:519` | 生命周期路径先锁目标用户再升序锁全组成员，可能 ABBA 死锁 | 事务外探查成员，事务内升序整批加锁；补并发测试 |
| feishu-data#23 | `domain/xlsx.rs:164` | xlsx 无解压后大小上限，可膨胀到 GB 级 | 检查 zip entry uncompressed_size 总和 + 行/单元格硬上限 |
| feishu-data#26 | `datasource/actions/import_xlsx.rs:267` | 表头解析在 async handler 同步执行，阻塞 tokio worker | 移入 `spawn_blocking` |
| feishu-approval#36 | `feishu_approval_worker.rs:786` | HTTP 同步派发与 worker 可同时处理同一任务，回写无状态 CAS | 统一 state 条件更新 + CAS 回写 |
| frontend-engine#45 | `auth-session.ts:84` | 刷新/恢复无超时或取消，挂起会无限等待 | 加 `AbortSignal.timeout`，超时按可重试处理 |
| frontend-features#51 | `UserPermissionView.tsx:144` | 组成员详情加载中/失败时仍断言「不属于任何组」「无有效权限」 | 对 pending/error 单独渲染，不把未加载当事实 |

### 5.2 低危（19 条，含 2 条核验后降级）

| ID | 状态 | 位置 | 问题 |
|---|---|---|---|
| account#8 | 未核验 | `login.rs:169` | User-Agent 未截断，超长头使会话/登录事件写入静默失败 |
| access-infra#15 | 未核验 | `batch_grant_permissions.rs:145` | 批量授予吞掉 `ensure_declared` 非 ParamInvalid 错误 |
| access-infra#16 | 未核验 | `add_group_item.rs:102` | 不锁组行，并发重复添加/删组泄漏 5xx |
| access-infra#17 | 未核验 | `revoke_permission.rs:93` | 单条撤销用 `revoke_by_subject` 杀掉目标全部 refresh token |
| feishu-data#27 | 未核验 | `create_portal.rs:351` | `query_field_ids` 可配置入库但无读取方 |
| feishu-data#28 | 未核验 | `delete_datasource_table.rs:109` | 删除数据源不处理门户，留下失效公开门户 |
| feishu-data#29 | 未核验 | `portal_query.rs:241` | 门户配置 JSON 解析失败被静默吞成空值 |
| feishu-approval#38 | 未核验 | `feishu_event_worker.rs:260` | 事件连续失败 10 轮即永久丢弃 |
| feishu-approval#39 | 未核验 | `dispatch.rs:1346` | 同步派发不经创建令牌桶，可破 100/分钟上界 |
| feishu-approval#40 | 未核验 | `user_token.rs:556` | `user_token` 单行不变式无数据库约束 |
| frontend-engine#46 | 未核验 | `business-cell-model.ts:30` | Timestamp 是 Unix 秒，date_time 渲染失败显示原始秒数 |
| frontend-engine#47 | 未核验 | `shell/density.ts:21` | Storage getter 抛错时应用启动崩溃 |
| frontend-features#50 | **confirmed→low** | `ApprovalConfigDetails.tsx:145` | Token 轮换/回显在剪贴板失败时明文无处可拿 |
| frontend-features#54 | 未核验 | `AccountSettingsPage.tsx:425` | 日期隐式 locale，违背 zh-CN 单语言合同 |
| frontend-features#55 | 未核验 | `DatasourceDetailPage.tsx:1423` | Token 轮换成功后「最近轮换」仍显示旧值 |
| tests-ops#60 | **confirmed→low** | `docker/app/Dockerfile:7` | 构建上下文未排除 deploy 生产配置与 SSH 口令文件 |
| tests-ops#64 | 未核验 | `ops/prometheus/yang-system.rules.test.yml:285` | p99 反例缺 `+Inf` 桶，断言空转 |
| tests-ops#65 | 未核验 | `deploy/verify-feishu-user-identity.sh:40` | MySQL root 密码出现在 `docker exec` argv |
| tests-ops#66 | 未核验 | `.github/workflows/ci.yml:71` | CI 不校验部署脚本语法 |

### 5.3 已排除的误报（2 条）

- `access-infra#12`：Outbox 积压指标并非全表扫描。`state` 是 ENUM，`idx_authorization_outbox_dispatch(state, available_at, id)` 的前缀可做 range；`published` 行不会被扫描。**无需修改。**
- `frontend-features#48`：门户从 public 切 passphrase 且口令留空不会锁死。服务端 `update_portal.rs:264-269` 会拒绝「没有存量口令却切 passphrase」，编辑器进入 catch 后不会执行 setPassphrase。**无需修改**，仅剩 UX 提示可优化。

---

## 六、修复方案（分阶段）

### 阶段 0：止血（建议 0–3 天，P0；不含暂缓项）

**目标**：关闭其余可直接利用的攻击链。**P0-2（Step-up）、P0-9（Encrypt Key）与 0.1（凭据轮换）按 2026-10-20 决策暂不执行**，保留为已知风险。

| 任务 | 涉及文件 | 验收标准 |
|---|---|---|
| 0.1 轮换泄露凭据 **（暂不修复）** | 飞书后台、部署配置（不读值） | 暂不执行；`event_encrypt_key`、审批配置 Token 与可能进入明文链路的凭据保持现状，作为已知风险跟踪 |
| 0.2 事件端点 fail-closed | `receive_approval_event.rs`、`event_challenge.rs` | 伪造 token/未加密请求返回 401/403；合法事件仍 200；解密故障仍 5xx 触发重推 |
| 0.3 门户 suggest 鉴权 + 限速 | `portal_suggest.rs`、`portal_query.rs`、`rate_limit.rs` | passphrase/login 无凭证 suggest 返回 403；限速键按 portal_id；大小写变体共享同一计数键 |
| 0.4 派发记录脱敏 | `dispatch.rs`、`request_log_writer.rs`、`list_requests.rs` | 新记录 `request_body` 不含 token；只读列表不泄露；历史明文与 Token 轮换按 0.1 暂缓项跟踪；回归测试断言 |
| 0.5 组授权闸门 | `add_group_item.rs`、`add_group_member.rs`、`sensitive_permissions.rs` | 双账号提权链在第二步 403；`feishu.approval.write` 标记 admin-equivalent；集成测试红/绿验证 |
| 0.6 Step-up 接线或替代 **（暂不修复）** | `app.rs`、`account/user/mod.rs`、相关 Action | 暂不执行；仅持 Access Token 仍可改邮箱/停用 TOTP/删号，作为已知风险跟踪 |
| 0.7 会话撤销 fail-closed | `refresh.rs`、`context.rs`、`session/repository.rs`、`revoke_session.rs` | 撤销后 refresh 401；并发轮换不可洗白；缺会话行不可降级；超长 UA 不破坏登录 |
| 0.8 部署默认回环 | `deploy.ps1`、`deploy-finance-prod.ps1`、`deploy/README.md` | 默认 `127.0.0.1`；公网需显式开关；TLS 反代就绪；部署契约测试更新 |
| 0.9 前端刷新回写 | `session-bridge.tsx`、`auth-session.ts`、`use-session.ts` | 刷新后请求携带新 token；连续两次过期仍可刷新；业务 401 不登出 |
| 0.10 前端登出/选择状态 | `session-controller.ts`、`DataGrid.tsx` | 过期后登出仍清本地；翻页清空批量选择；回归测试 |

**建议顺序**：0.2/0.3/0.4（公开面止血）→ 0.5/0.7（授权与账号）→ 0.8 → 0.9/0.10。**0.1 与 0.6 暂缓，不在本轮执行**；0.5 可与其他任务并行。若未来恢复 0.6，需先确认 Step-up 产品决策并同步文档。

### 阶段 1：数据一致性与正确性（建议 1–2 周，P1）

- **账号**：`account#3` 会话清理/全设备撤销；`account#4` 删除无消费方端点；`account#6` lookup 权限；`account#7` credential_version 门控。
- **授权**：`access#13` 过期直授传播；`access#14` 日志过滤默认值。
- **飞书审批**：`#31` 任务唯一键；`#32` 通知终态门；`#33` applicant 唯一约束；`#34` 时区校验；`#35` 回填列查重；`#37` 通知失败重试。
- **飞书数据**：`#22` 门户子行分页；`#24` 日志脱敏；`#25` pull 坐标校验。
- **前端**：`#42` 业务 401；`#43` 过期登出；`#44` 批量选择；`#49` 授权日期；`#52` 键盘可达；`#53` E2E 隔离。
- **运维/门禁**：`#57` readiness 探针；`#58` SLI 分母；`#59` 清库密码脱敏；`#61` 架构门禁；`#62` 测试过滤；`#63` 文档漂移；`#60` Docker 上下文。

**验收**：每条新增/修改的守卫至少做一次「摘除式」变异（删掉判断 → 对应测试变红 → 恢复），按 `docs/guides/TESTING_SILENT_FAILURES.md` 执行。

### 阶段 2：加固与长期治理（建议 2–4 周，P2）

- 7 条未核验中危逐条确认：死锁、SMTP 时序、xlsx 解压上限、async 阻塞、同步/worker 竞态、刷新超时、权限页加载态。
- 19 条低危按影响排序处理；优先 `account#8`（UA 截断）、`feishu-data#28`（失效门户）、`tests-ops#65`（探针密码）。
- 架构级加固：
  - 飞书 worker/OAuth state/导入互斥的单实例假设与蓝绿重叠：引入 Redis 分布式锁或明确单活部署约束。
  - 通知/告警缺持久 Outbox：评估是否引入 outbox 表或至少补偿任务。
  - `audit_event`/`login_event`/`request_log`/`event` 保留期与清理 worker。
  - 管理员等价清单的反向检测：新增「签发/回显凭据」语义的权限时，用测试或脚本强制登记。
  - 前端 `features/shell` 直连 engine 内部路径：补齐公共出口并落地机器禁令。
  - OIDC/SSO 空端口：接入前补 IdP 吊销联动与账户合并语义。

---

## 七、回归验证计划

### 7.1 命令

```bash
# 提交前
python scripts/run_ci.py quick
python scripts/check_architecture.py
python scripts/check_architecture.py --self-test

# 推送前（注意：本机 full 的 audit 步需要 VPN 或 registry 覆盖）
python scripts/run_ci.py full

# 真实依赖集成（MySQL 8 / Redis 7；单线程；库名以 _test 结尾，Redis DB 15）
python scripts/run_ci.py integration

# 定向
cargo test --test permission_groups_integration -- --test-threads=1
cargo test --test session_revocation_integration -- --test-threads=1
cargo test --test feishu_approval_dispatch_integration -- --test-threads=1
pnpm --dir frontend check
```

### 7.2 新增测试清单

- 双账号组提权链（必须 403）。
- Step-up（暂缓）：仅 Access Token 不能 `change_email`/`totp_deactivate`；待恢复 0.6 后实施。
- 会话撤销：撤销后 refresh 401；并发轮换不可洗白；缺行 fail-closed。
- 事件端点：伪造 token / 明文注入被拒；合法事件 200。
- 门户：suggest 在 passphrase/login 下 403；大小写 slug 共用限速键。
- 派发记录：`request_body` 无 token。
- 前端：刷新后快照 token 更新；第二次过期仍可刷新；业务 401 不登出；翻页清空选择。

### 7.3 运维验证

- （暂缓）飞书后台轮换 Encrypt Key 与审批 Token；待恢复 0.1 后执行。
- 检查日志管道不再出现 `request_body.token`、`verification_token` 期望值、清库 SQL 密码。
- 财务机确认当前 `BIND_ADDR` 与 TLS 终止形态；若曾公网明文，凭据轮换按 0.1 暂缓项处理。
- 检查 `deploy/config.cloud*.toml` 是否曾被构建上下文打包；如是，评估镜像/制品泄露范围。

---

## 八、覆盖与局限

- **已覆盖**：`src/addon/account`、`access`、`feishu` 全部源码；`src/infrastructure`、`config`、`bootstrap/app`；`frontend/src` 全部；`tests/`、`scripts/`、`deploy/`、`docker/`、`ops/`、CI；关键契约文档（AUTHZ_GRANTS、SCHEMA、AUDIT、CONFIGURATION、OBSERVABILITY、SLO）。
- **未运行**：`cargo build/test`、`pnpm check`、Playwright、部署脚本、真实 SMTP/Redis/MySQL 并发；所有结论来自静态阅读与对抗核验。
- **未读取**：`config.toml`、`deploy/config.cloud*.toml`、`.env`、任何密钥文件；因此无法确认 `event_verification_token`/`event_encrypt_key`/`log_inbound_requests` 的生产取值，也无法确认财务机当前是否真的以 `0.0.0.0` 运行。
- **未核验的 24 条**：7 条中危 + 17 条低危，已在第五节标注；它们不是「已证伪」，而是未进入核验批次或置信度不足。
- **2 条误报**已排除，未计入修复计划。
- **框架侧**：仅按调用语义阅读 `yang-base` 相关实现；本次发现均定位在应用仓库，未对基础库做全面审查。

---

## 九、修复优先级速查

> 标注「暂缓」的条目按 2026-10-20 决策不在本轮修复。

| 优先级 | 条目 | 一句话理由 |
|---|---|---|
| P0 | `access-infra#9`、`access-infra#10` | 任意注册用户可提权到委派权限/机器凭据 |
| P0（暂缓） | `account#1` | 会话窃取可绕过 MFA 完成账号接管；按 2026-10-20 决策暂不修复 |
| P0 | `account#2` | 「踢出设备」对外承诺失效 |
| P0 | `feishu-data#18`、`#19`、`#20` | 公开端点可伪造事件/枚举受保护数据/爆破口令 |
| P0 | `feishu-approval#30` | 只读权限可取得派发 Token 明文 |
| P0（暂缓） | `feishu-data#21` | 真实 Encrypt Key 进入版本库；按 2026-10-20 决策暂不修复 |
| P0 | `tests-ops#56` | 财务生产凭据明文公网传输 |
| P0 | `frontend-engine#41` | 用户约 2 小时被强制登出、请求翻倍 |
| P1 | 第四节 27 条 | 数据一致性、错误处理、前端交互、运维告警 |
| P2 | 第五节 26 条 | 低危加固、未核验项、长期治理 |

---

*本报告由 7 个领域并行审查 + 3 组对抗核验生成；所有代码位置以 `main@9f68346` 为准。修复时按阶段 0 的未暂缓项执行；P0-2、P0-9 与 0.1 按 2026-10-20 决策暂缓，待后续决策。*

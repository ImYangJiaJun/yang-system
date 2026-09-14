# yang-system 账户系统完整性对抗性评审（第一性原理·多视角证伪）

> - 评估对象：`yang-system` 的 `account` Addon（注册/认证/会话/生命周期/第二因子）、`access` Addon（授权端口）、`infrastructure/authorization`（授权失效传播）、`infrastructure/audit`（审计），以及前端 `features/auth`、`features/account`、`engine/session`
> - 当前代码快照（嵌套仓库）：`831a3c564188dc5158ed733aa375e20029f5c3d4`
> - 当前框架快照（根仓库 lib_yang）：`cc9756dcf7918f7ee172d4219bec29193c660445`
> - 复核日期：2026-09-14
> - 文档性质：基于当前源码的完整性评审；结论经逐条对抗性证伪得出，不代表下述缺口已经修复
> - 方法：5 个独立第一性原理视角推导 68 条完整性维度，8 个对抗性视角猎取缺口，46 条候选缺口逐条到源码证伪（最终 43 confirmed / 1 uncertain / 2 refuted），并对三条关键结论做了人工源码复核；2026-09-14 补检：39 代理对框架内部 / CSRF-Origin / OIDC 三块再证伪（32 候选 → 29 confirmed / 3 refuted），见第七节

## 一、结论（裁决）

判定标准沿用 `docs/architecture/account-system-roadmap.md` 第二节的口径——账户系统只回答四个问题，外加一条横切约束，且每条都要**闭环**（有创建端必有消费端、有写入必有失效传播、有敏感操作必有 Step-up + 审计）：

1. **你是谁**（身份建立）
2. **怎么证明是你**（认证与凭据）
3. **证明的有效期**（会话）
4. **如何体面结束**（生命周期终结）
5. 横切：**对抗滥用** + 全程审计

按此标准，裁决为：

> **单账号的「注册→认证→会话→终止」闭环已基本闭合；但存在一条致命缺陷（会话精确撤销失效），以及一批高/中/低缺口，因此不能认定「完整」。**

同时澄清一处评审口径（见第八节）：`access` 权限管理域当前是**刻意未交付、只留端口**的状态，本评审不将其视为缺陷，只记录由此产生的文档漂移。

## 二、评估方法与证据边界

### 2.1 多视角第一性原理（维度推导）

用 5 个相互独立的视角分别推导「账户系统完整性」应覆盖的维度，避免单一视角自说自话：

| 视角 | 要点 |
|---|---|
| OWASP ASVS 4.0 | V2 认证 / V3 会话管理 / V7 错误与日志 / V4 访问控制 |
| NIST SP 800-63B | 身份证明、认证器管理、会话生命周期、联邦身份 |
| STRIDE 威胁模型 | 枚举、撞库、爆破、会话劫持/固定、重放、越权、隐私泄露 |
| 成熟 IdP 功能矩阵 | Ory Kratos / ZITADEL / Keycloak / Logto / better-auth / SuperTokens 共识能力 |
| 生命周期 + 隐私/合规 | GDPR 删除权/可携带、数据最小化、保留期、运营管理动作 |

合计 68 条完整性维度。

### 2.2 对抗性缺口猎取与逐条证伪

- 8 个对抗性视角（身份/凭据/会话/授权失效/生命周期/反滥用/审计/前端契约）各自带完整实现地图猎取缺口，产出 46 条候选。
- 每条候选缺口由独立的「证伪者」打开真实源码验证：只有从代码找到确凿证据才判 confirmed，纯臆测或已被覆盖的判 refuted。
- 最终 43 confirmed / 1 uncertain / 2 refuted；本评审的三条关键结论（逐台撤销、refresh 丢失 session_id、无最终管理员声明器）另经人工打开 `revoke_session.rs`、`claims.rs`、`system_owner.rs` 复核，与证伪结论一致。

### 2.3 证据边界

第一遍未覆盖的「框架内部 / CSRF / OIDC」三块，已由第二遍审计补齐（见第七节），结论含对它们的裁决。仍属证据边界之外的是运维/部署面与集成测试覆盖（见 7.5）。

## 三、致命缺陷：逐台撤销不吊销 refresh token

`POST /api/v1/users/sessions/revoke`（`revoke_session.rs`）按 `session_id` 撤销单台设备，但实现只让 access token 失效，**被踢设备的 refresh token 不受影响，可在黑名单窗口内无限轮换出新 access token**。

链路逐段核实：

1. `user_session.current_jti` 存的是 **access token 的 jti**：登录 `login.rs:265` 用 `verify_token(access_token)` 取 claims、`:289` 把 `claims.jti` 写入 `current_jti`；refresh 时 `refresh.rs:84` 同样验 access token、`:98` 以新 access token 的 jti 更新 `current_jti`。
2. access 与 refresh 的 jti 各自独立生成：`crates/yang-base/src/token/manager.rs:537`（access）与 `:562`（refresh）分别 `uuid::Uuid::new_v4()`，永不相等。
3. 撤销只拉黑 access jti：`revoke_session.rs:57-60` 调 `revoke_by_jti_with_ttl(&current_jti, …)`，写入 `token:blacklist:{access_jti}`。
4. refresh 轮换校验的是 refresh jti：`verify_token_checked(refresh)` 查黑名单用的是 refresh token 自己的 jti，永远命中不了上面那条。
5. 头注释 `revoke_session.rs:21-22` 声称「被踢设备的旧 refresh 会被 verify_token_checked 拒绝」——**与实现不符**。

叠加第二个断链：`claims.rs:55-59` 显示 refresh token 的 claims 只带 `credential_version`/`authz_version`、**不带 `session_id`**，而 refresh 继承路径 `claims_for_refresh` 从旧 refresh claims 里读 `session_id` 恒为 None（`session_id_from_claims` 读不到该键）。因此 session_id 从未真正跨轮换继承。

后果：「登录设备管理 / 逐台踢人」这条对用户承诺的安全能力整体失效——被踢设备只是从列表里消失，实际仍在线且无时间上限。修复方向：把 refresh jti 一并拉黑，或让 `user_session` 记录 refresh jti，或增加按 `session_id` + 签发时间的作用域撤销。

## 四、高严重度缺口

| 缺口 | 关键代码 |
|---|---|
| 成功业务写入用 `append_independent` 独立提交审计，破坏「成功审计与业务同事务原子提交」不变量（`revoke_session`、`admin_issue_password_reset`），中间崩溃会丢审计 | `revoke_session.rs:73`、`admin_issue_password_reset.rs:67`、`audit/repository.rs:144` |
| 登录成功/失败不入 append-only `audit_event` 事实源，失败记录 best-effort 静默丢弃，`login_event` 无任何保留/清理代码 | `login.rs:173-251`、`login_event.rs:1-5`、`schema.rs:232` |
| 「匿名化删除」保留 `password_hash / totp_secret(AEAD 密文) / totp_recovery_digest`——伪匿名而非删除，GDPR Art 17 未真正满足 | `authz_version.rs:206-239` 仅 UPDATE 六列，凭据三列原样保留 |
| 删除账号不清 `user_session` / `login_event`（`ip` + `user_agent` 属个人数据，无外键、无删除路径） | `delete_account.rs:44-81` 全文无 session/login_event 清理 |
| 用户可见「安全事件」只读 `login_event`；`audit_event` 全仓无任何 SELECT/导出读路径，高危变更与 Step-up 审计对受影响用户不可见 | `security_events.rs:55-58` |
| TOTP 校验无重放防护：同一码在 ±1 窗口(~90s)内可重放，头注释「一次校验只成功消费一个窗口」在代码中未实现 | `crates/yang-base/src/action/auth/mfa.rs:70-110` |
| `totp_setup / totp_activate` 未挂 Step-up（仅 `deactivate` 挂了）：会话劫持者可自行绑定 TOTP 并夺走 8 枚恢复码锁定合法用户 | `totp_setup.rs:31-34`、`totp_activate.rs:40-43`、`user/mod.rs:116-140` |
| 删除账号后 `authz_grant` 授权事实残留（无外键、无级联、无删除路径） | `access/grants/table.rs`、`delete_account.rs` |

## 五、中低严重度缺口（按主题）

**防枚举 / 限流仍可绕过**
- 三个公开发码端点 `deliver=false` 分支无人工延时拉齐，响应体一致但耗时可区分「已注册且启用」邮箱（时序枚举）。
- 注册先 `username_exists` 再烧验证码，任意用户名可被免费枚举（`register.rs:54-66`）。
- 邮箱 `+tag` 子地址与 `.` 不折叠：同一收件箱可建无限身份，且按邮箱维度的限流/发码被按 tag 拆分绕过（`email_verification.rs:321-354`）。
- 身份限流按 username/邮箱拆成两个独立桶，同一账号可获双倍尝试预算（`login.rs:51-60`）。

**凭据策略不对称**
- 「密码不能与用户名相同」只在注册生效，改密/重置绕过（`policy.rs:87-95`）。
- `change_password / reset_password` 不调用 `converge_revocation`，旧 access token 存在传播窗口；其余 8 个动作都调了（`change_password.rs:79`、`reset_password.rs:78`）。
- 改密前端声明 Step-up 但后端 `step_up_targets` 不含 `change_password`，TOTP 账号改密退化为单因子。

**MFA**
- 恢复码消费存在 TOCTOU：普通 SELECT（无 FOR UPDATE）+ 按 USER_ID 无条件回写，并发下同一恢复码可双重消费（`repository.rs:412-453`）。

**授权模型（端口层，见第八节口径）**
- `revoke_permission` 只靠 Outbox 异步传播、无即时收敛，被撤销权限在传播窗口内仍生效。
- 无「最后一名持 `access.grants.write` 者不可被停用/撤销」守卫。

**生命周期 / 隐私 / 合规**
- `deleted_{id}` 用户名无保留命名空间、无存在性预检，可被预占导致删除事务回滚。
- 无隔离期/墓碑，被删身份可即时抢注冒充；停用账号邮箱永久滞留（`email_exists` 无状态过滤）。
- 无数据导出端点，GDPR Art 20 数据可携带权缺失。
- `audit_event` / `authorization_outbox` 无保留期执行机制：只有 `idx_audit_event_retention` 索引、无清理 worker，`mark_published` 只改状态不删除。

**审计卫生**
- `list_users`（纯只读 GET）与 `totp_setup`（仅内存生成密钥）也写 succeeded 审计事件，且经 `append_independent`，产生无界审计噪声并让只读请求耦合审计写失败。

**前端 / 契约**
- 安全事件后端与契约类型已存在，但前端零调用、无路由、账号中心无区块（README/roadmap C-2 却标已完成）。
- `subscribeSessionEnd` 跨标签接收侧未挂载到组件树，跨标签即时收敛不生效。
- 账号中心 API 绕过 `requestWithTokenRefresh` 客户端，access token 过期即硬失败且不传播会话失效。
- 前端账号中心不感知 `credential_mutations_enabled` 发布开关，开关关闭时改密/改用户名/换邮箱/停用区块呈非功能态。

**会话 / 开关一致性**
- `revoke_session` 未自检 `credential_mutations_enabled`，开关关闭时接口仍注册、无 Step-up 保护（与 `logout.rs:37-41` 不对称）。

**可观测性**
- `authz_propagation_seconds` 以秒粒度采样，无法度量 SLO「outbox 到 Redis p99 < 2s」；worker 指标未加 `yang_system_` 前缀、未进 `YANG_SYSTEM_METRIC_NAMES` 清单、未受 `metrics` feature 门控。

## 六、已证伪 / 无缺口

2 条候选缺口被证伪。另经对抗性审查确认以下维度**实际无缺口**：

- 密码重置 token：32 字节 `OsRng` + SHA-256 摘要入库，`consume_in_tx` 用 `FOR UPDATE` + 条件 UPDATE + `affected==1` 判定，无恢复码那种 TOCTOU。
- 头像上传：MIME 白名单 + magic bytes + 宽高 ≤1024 + ≤40KiB + SVG 拒绝。
- demo addon 对象级授权：`list_notes` 强制 owner 作用域、`delete/update_note` 按 owner 过滤，无 IDOR。
- 前端 token 存储：access token 仅存内存并主动清除 legacy sessionStorage；`nginx.conf` 有 CSP/COOP/HSTS/nosniff/X-Frame-Options。
- 密钥隔离：启动校验强制 token/step-up/邮箱验证码四套密钥域交叉隔离、≥32 字节、TTL 边界。

## 七、第二遍检验：框架内部 / CSRF-Origin / OIDC 风险（2026-09-14 补检）

对第一遍标记为「尚未检验」的三块做了第二轮对抗性审计（39 代理、逐条证伪，32 候选缺口 → 29 confirmed / 3 refuted）。

### 7.1 总评

框架安全原语整体扎实，与 roadmap「底层安全原语已达到甚至超过多数开源方案」的判断一致；**CSRF/Origin 无缺口**。确认的缺口集中在「参数未显式固定、无再哈希/升级路径、验证码键控未归一化、Cookie Secure 无 TLS 兜底、同源校验对全缺头 fail-open、限流固定窗口」等 LOW/MEDIUM 项。OIDC 是纯未来风险面（当前仅 trait、无实现），不作为当前缺陷。

### 7.2 已确认缺口（框架层）

**密码哈希（Argon2id）**
- [LOW] `verify_or_dummy` 等时性依赖「存量哈希参数与 dummy 一致」的隐含不变量；无 rehash-on-login / 参数迁移 / 存量哈希版本校验，历史弱哈希或参数漂移会重新暴露用户名枚举（`password.rs:47/62`）。
- [LOW] Argon2 参数取 OWASP/RFC 下限（m=19456, t=2, p=1）且完全依赖库默认值，未显式固定、无升级/再哈希路径。

**邮箱验证码**
- [MEDIUM] `deliver=false` 防枚举路径仍消耗发送额度并写入冷却（`email_verification.rs:435-454`）。
- [MEDIUM] 引擎按原始邮箱字符串键控，大小写 / `+` 别名可绕过单身份限流（`email_verification.rs:606-608`）。
- [LOW] 发送计数/冷却先于真实投递，投递失败不返还额度。
- [LOW] 校验侧（consume/verify_only）无内置限流，缺失 key 为免费 GET。
- [LOW] `from_config` 仅校验位数 1..=9、无安全下限（应用侧已硬编码 6 位并封顶 max_attempts，风险已收敛）。
- [LOW] 重发原子覆盖旧码并重置尝试计数，暴力预算被放宽。

**限流器**
- [LOW] 固定窗口而非滑动窗口，窗口边界可 2× 突发放大。
- [LOW] `record_failure` 未接入主密码登录失败路径，失败计数语义不一致。
- [LOW] `clear_failures` 连带删除共享 IP 失败桶，造成跨用户干扰。
- [LOW] `AuthRateLimitConfig` 公共 API 无内部校验，`window_seconds=0` 会静默关闭限流（应用侧 `validate_rate_limit` 已兜底）。

**JWT / Token**
- [MEDIUM] `new_symmetric` 不强制 HMAC 密钥 ≥32 字节（仅 `new_symmetric_keyring` 校验；应用侧 `validate_token_secret` 已兜底）。
- [LOW] `new_asymmetric` 无 RSA 密钥长度校验，且非对称模式无 keyring 轮换（当前非活跃路径）。
- [LOW] `token_type` 未下沉到 verify 层，仅调用点校验（middleware/refresh）。
- [LOW] refresh 无复用检测（reuse detection），被盗轮换无告警 / 家族撤销。
- [LOW] logout 走 `revoke_by_subject` 全量撤销；token 层虽有 `revoke_by_jti_with_ttl` 但唯一调用点 `revoke_session` 用错 jti（见第三节）。

**浏览器会话 Cookie / 同源**
- [MEDIUM] `Secure` 属性由客户端 Origin/Referer 推导，缺头即降级不带 Secure，且无服务端 TLS 兜底（`browser_session.rs:72-93`）。
- [LOW] `validate_same_origin` 对 Sec-Fetch-Site / Origin / Referer 三信号同时缺失的请求静默放行 `Ok(false)`（fail-open，无 fail-closed 兜底）。
- [LOW] Cookie 属性经 `format!` 拼接未转义；当前构造点是编译期常量、JWT base64url 无 `;`，不可利用，但公共构造器可注入。

### 7.3 CSRF / Origin 边界：无缺口

双层防御：唯一 Cookie 凭据 `yang_refresh` 为 HttpOnly + SameSite=Strict + host-only（跨站请求根本不携带）；且全部 9 个触碰 Cookie 的 action（login / login_by_email_code / refresh / logout / change_password / change_username / disable_self / delete_account / reset_password）以及 step_up / request_mfa_email_code / request_login_email_code 均显式调用 `validate_same_origin`。access token 仅存响应体（内存承载），以 Bearer 认证的端点天然免 CSRF。候选缺口「6 个 access-token-only 端点缺 Origin 校验」已证伪——它们无实时 CSRF 暴露面。

### 7.4 OIDC / SSO 未来风险（纯风险清单，非当前缺陷）

`domain/oidc.rs` 当前仅 `ExternalIdentityProvider` trait、无实现、无 `user_identity` 表、无配置面。未来接入外部 IdP 时的风险项（按严重度）：

- [HIGH] **IdP 侧吊销无联动**：本地吊销完全靠 `authz_version/credential_version/user_session.revoked_at/authorization_outbox`，无 OIDC backchannel logout / session management，IdP 侧注销/吊销不会传导到本地会话。
- [MEDIUM] `external_sub` 绑定后缺账户合并/链接语义与安全门（trait 仅 find_local_user/link_local_user，无 merge/unlink/provision）。
- [MEDIUM] `username` 强约束（`^[A-Za-z0-9_-]+$`、3-64、unique）与 IdP `preferred_username` 冲突，且无 `display_name` 分离字段。
- [MEDIUM] `password_hash` 强制非空（`require(true)`），无密码的 SSO-only 用户无法落库，需先解耦该列。
- [MEDIUM] 端口签名以 `&MySqlPool` 而非 `&mut Transaction`，无法兑现其文档声明的「与 authz_version writer 同事务」原子性。
- [MEDIUM] 无 IdP 信任/校验面（issuer / audience / claims / PKCE / JWKS / discovery 全缺）。
- [LOW] 邮箱子地址不归一化，同一 IdP 邮箱的 `alice@x` 与 `alice+work@x` 会形成两个本地身份。

### 7.5 仍未检验（第三类边界）

- 运维/部署面：备份恢复、日志脱敏与保留仅 doc 级，无代码级验证。
- 集成测试覆盖：现有 6 个 `tests/` 入口无 access/permission/step-up 端到端用例。
- 账号级锁定/指数退避、空闲超时：属未评估的设计取舍，非缺口。

## 八、澄清：权限管理刻意未交付（非缺陷）

评审初期把「权限管理面无冷启动引导」列为致命根因二，经与维护者确认口径后修正：

- `src/addon/account/domain/system_owner.rs` 的 `NoSystemOwnerClaimer::claim` 恒返回 `AlreadyClaimed`、`OwnerClaimOutcome::Claimed` 被 `#[allow(dead_code)]`，是**「无最终管理员」不变量的刻意实现**，不是 bug。
- `access` Addon（`grant_permission / revoke_permission / list_permissions / list_user_grants`）当前是**预留端口/脚手架**，权限管理功能尚未交付，因此「grant_permission 需 `access.grants.write` 而无人能获得首条授权」的鸡生蛋状态是**预期中的未接线状态**，不属于当前要修的缺陷。

由此保留的合法结论只有两条**文档漂移**（非代码缺陷），已于 2026-09-14 修复：

1. ~~`README.md` 顶层仍写「骨架只保留 account 一个业务 Addon」、能力表只列 `account.user`、目录树写「当前只有 account 一个」~~ → 已修正概览句、能力表（补 `access.grants` / `demo.notes` 两行）与目录树（三 Addon）。
2. ~~`docs/architecture/account-system-roadmap.md` 进度表把「D 管理动作」「E-4 多因子任选登录阶段 1」标「✅ 已完成」~~ → 已在进度表前新增「完成状态口径」注：`✅ 已完成` = Action/契约落地，不等于生产可交付；`access` 授权端口无冷启动引导、属预留端口。

另同步修正 `AGENTS.md` 的同一处漂移（概览句与目录树）。

## 九、优先级建议

1. **修 `revoke_session` 的 refresh 吊销**（第三节）——「设备管理 / 逐台踢人」当前是空话，属对外承诺的安全能力失效。
2. **审计原子性**：把成功写入从 `append_independent` 改回 `append_in_tx`（第四节第一条）。
3. **TOTP 重放防护** + **`totp_setup/activate` 挂 Step-up**（第四节）。
4. **GDPR 删除/可携带/保留期**：删除时清理 `user_session`/`login_event`/`authz_grant`，清空 `password_hash`/`totp_*` 凭据列，补数据导出端点与保留期执行机制。
5. **文档漂移**：修正 README 的 Addon 概览与目录树，为 roadmap 进度表补充「端口未接线」说明。

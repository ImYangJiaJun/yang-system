# 多因子任选登录（Google 式）设计方案

> - 文档性质：面向实施的设计方案，**当前未实施**；状态变更同步 `docs/architecture/account-system-roadmap.md` 进度表
> - 依据：2026-11 对 `src/addon/account/`、`frontend/src/features/auth/`、`crates/yang-base/src/action/auth/` 的源码核实
> - 关联文档：`docs/architecture/account-system-roadmap.md`（总路线图）、`docs/contracts/AUDIT.md`、`docs/contracts/CONFIGURATION.md`
> - 目标语义：对标 Google 账户登录——多种验证方式，**同一因子类别内任选**；未开双因子任选一种第一因子即可登录，开启双因子后需再任选一种第二因子

## 一、第一性原理：因子分类模型（设计的出发点）

认证强度 = 独立因子类别的组合：知识（password）、持有（possession）、固有（inherence）。"任选" 只能发生在**同一因子类别内部**，跨类别的「任选一种」在安全性上不成立：

- **TOTP 不能单独当第一因子**：6 位数字、30s 窗口，不具备标识身份的能力，脱离「你是谁」的前提可被暴力枚举；它是持有证明，不是身份证明。
- **同类两因子不算双因子**：密码 + 邮箱验证码看似两样，但多数用户的邮箱受同一个密码保护（或邮箱可重置密码），两者不构成独立性。Google 也从不把邮箱验证码计入 2SV。

据此对本系统现有/候选验证方式分类：

| 方式 | 因子类别 | 可作第一因子 | 可作第二因子 | 现状 |
|---|---|---|---|---|
| 密码 | 知识 | ✅ | — | 已有（`user/actions/login.rs`） |
| 邮箱验证码 | 持有（邮箱，弱持有） | ✅ | ⚠️ 仅作第二因子**备用** | 已有（`login_by_email_code.rs`） |
| TOTP | 持有（认证器） | ❌ | ✅ | 已有 |
| 恢复码 | 持有（离线） | ❌ | ✅ | 已有（单次消费） |
| MFA 备用邮箱验证码 | 持有（邮箱） | ❌ | ✅（备用通道） | 已有（`[email.mfa]` 独立 key 域） |
| Passkey/WebAuthn | 持有 + 固有 | ✅ | ✅ | 未做（全仓无 webauthn 依赖） |

**目标登录语义**（可执行化后的用户需求）：

1. 第一因子：从 {密码、邮箱验证码} 任选一种（未来 +Passkey）。
2. 若账号已激活 TOTP（`users.totp_activated_at IS NOT NULL`），必须再出示第二因子：从 {TOTP、恢复码、备用邮箱验证码} 任选一种。第二因子三选一**已由 `domain/context.rs::verify_second_factor` 实现**（依次尝试 TOTP→恢复码→备用邮箱验证码）。
3. 未激活 TOTP 的账号：第一因子通过即登录，语义不变。

## 二、现状差距（源码核实）

- **G-1（bug 级缺口）邮箱验证码登录绕过双因子**：`user/actions/login_by_email_code.rs` 的 `EmailCodeCredentialVerifier` 不检查 `totp_activated_at`，前端 `LoginPage.tsx` 验证码模式无 MFA 分支（文件内注释明示）。后果：**已激活 TOTP 的账号可用邮箱验证码直接登录，双因子形同虚设**。无论是否做完整改造，此缺口必须先修。
- **G-2 框架单步模型，无登录挑战协议**：`yang-base` `CredentialVerifier::verify`（`action/auth/login.rs:24`）是「验证→返回身份」单步模型；现有双因子是**无状态两段式**——第一段密码通过后返回 `SecondFactorRequired`（不签发任何凭据），第二段**重发完整密码** + `extra.mfa_code`。该形态在「任选组合」下不可扩展：每多一个可选第一因子/第二因子，客户端都要重放前面所有凭据，服务端也无法表达「已通过因子集合」。
- **G-3 会话/claims 无认证方式记录**：`AppClaims`（`domain/claims.rs:14`）无 `amr` 类声明（有 `version: u8` 演进字段可用）；`user_session` 表（`infrastructure/schema.rs:210-227`）无认证级别列。
- **G-4 前端结构**：`features/auth/pages/LoginPage.tsx` 单文件承载两种模式（`LoginMode` 联合类型 + 每模式独立 state/提交分支），加方式会继续膨胀；SessionController 无「部分认证」中间态。

**已核实的框架事实（影响方案选型）**：验证码引擎 `RegistrationEmailVerification` 只暴露 `request` / `request_via` / `consume`（`yang-base/.../email_verification.rs:354/368/469`），**没有「只验不消费」接口**；`consume` 是 Redis 原子单次消费 + 错误上限销毁。

## 三、安全不变量（任何一期改造不得退化）

1. **防枚举**：第一因子通过前，任何响应（含错误类型、时序）不得泄露账号是否存在、是否停用、是否启用 MFA（现有边界见 `login.rs:92-95` 与 `login_by_email_code.rs` 的统一无效验证码错误）。
2. **统一限流预算**：所有登录第一因子与发码端点共用 `AuthOperation::Login`（IP + 身份双维度）；TOTP 验码走 `AuthOperation::TotpVerify`。
3. **keyring 隔离**：四类邮箱验证码密钥域互不复用、不复用 token/step-up 密钥（启动校验强制）。
4. **审计与登录事件**：成功/失败均落 `login_event` + `AuthAuditHook`，不含凭据明文。
5. **第二因子独立性**：密码不得作第二因子；第二因子集合内任选不改变「与第一因子不同类」的要求（已决：第一因子为邮箱验证码时，`[email.mfa]` 备用邮箱验证码通道禁用，只接受 TOTP / 恢复码）。

## 四、分期方案

### 阶段 1（必修）：邮箱验证码登录对齐 MFA 语义（框架引擎小幅扩展 + 应用侧编排）

**目标**：消灭 G-1 缺口；让「第一因子任选 {密码、邮箱验证码} + 第二因子任选三选一」在现有无状态两段式下完整可用。

**设计要点**：

- 沿用密码登录的两段式形态：第一段提交邮箱 + 验证码；若账号已激活 TOTP，返回 `SecondFactorRequired`（不签发凭据）；第二段重发邮箱 + **同一个验证码** + `extra.mfa_code`。
- **关键设计决策（验证码单次消费与两段式的矛盾）**：验证码是单次消费的，第一段若直接 `consume`，第二段重发时验证码已失效。**已决：采用方案 A**——框架验证码引擎新增**只验不消费**接口（`verify_only`：比对 HMAC 摘要，但不销毁）。第一段只验不消费 → 发现需 MFA → `SecondFactorRequired`；第二段重新提交时先 `consume`（原子消费）再验第二因子。语义等价于「验证码在 TTL 内先用于探测、后正式消费」，不扩大攻击面（TTL 内持有验证码本身就是持有证明）。
  - **实现红线**：`verify_only` 的失败路径必须与 `consume` 共享错误计数与上限销毁语义——错 N 次照常销毁，否则等于开了不计次的暴力枚举旁路。框架侧需针对性测试（错 N 次销毁、peek 成功后 consume 仍原子）。
  - 需要在 lib_yang 落地（纯新增方法，不改 `consume` 既有行为），注意跨仓库推送顺序：先 lib_yang 后 yang-system。
  - 被否的候选：方案 B（第一段直接 consume + 签发限定路径 pending token 补因子）——零框架改动但引入一次性安全构件（新密钥域/防重放/配置同步），阶段 2 落地时整体报废，且中断后必须重新发码；仅在「必须单仓库紧急堵洞」时可作临时补丁。
- **第一因子约束（已决：严格禁用）**：邮箱验证码登录的第二因子**不接受** `[email.mfa]` 备用邮箱验证码——第一因子已是邮箱持有，同类不构成双因子。`verify_second_factor` 需在调用点按第一因子种类过滤备用通道（第一因子为邮箱验证码时只接受 TOTP / 恢复码）；密码登录的备用邮箱通道不受影响。
- 前端：`LoginPage.tsx` 验证码模式接入与密码模式相同的 `SecondFactorRequiredError` → `MfaChallengeDialog` 编排（第二段重发邮箱+验证码+mfa_code）。
- 防枚举细化：第一段的 `SecondFactorRequired` 只有在验证码**验证通过**后才可能返回，不泄露 MFA 状态；验证码错误仍统一 `ParamInvalid(email_code, ...)`。

**验收条件**：

1. 激活 TOTP 的账号走邮箱验证码登录，无第二因子必被拒；TOTP / 恢复码可完成，**备用邮箱验证码必被拒**（因子独立性）；密码登录的备用邮箱通道回归不受影响。
2. 未激活 TOTP 的账号行为与现状完全一致（回归）。
3. 验证码第一段只验不消费后，TTL 内仍可完成第二段；第二段验证码错误计数与销毁语义不变（集成测试覆盖原子消费、错误上限、防枚举）。
4. 登录事件/审计记录两种第一因子的成功与失败，粗粒度一致。

### 阶段 2（框架侧，正式编排）：登录挑战协议（pending-auth）

**目标**：把两段式从「重放凭据」升级为服务端挑战模型，为任意第一因子/第二因子组合（含未来 Passkey）提供统一协议；前端登录页重构为「方式选择 + 统一步骤编排」。

**设计要点**：

- 框架新增登录挑战原语，形状对齐既有 step-up（`yang-base/src/action/step_up.rs` 的 `StepUpChallenge` → proof）：
  - 任一第一因子通过且需第二因子时，签发 **pending-auth token**：HMAC JWT（独立密钥域，不复用 token/step-up/验证码密钥）、TTL ≤120s、claims 含 subject + 已通过因子清单（amr）+ purpose 标记；**不可当会话用**（无 access/refresh 能力，签发路径与 `LoginAction` 分离）。
  - 客户端持 pending token + 第二因子凭据提交统一完成端点 → 校验 token 有效性（建议 Redis 单次消费防重放，对齐 step-up proof 语义）→ `verify_second_factor` → `LoginAction` 签发正式会话。
  - `CredentialVerifier` 单步模型不变；挑战编排在 `LoginAction` 之上新增一层，密码路径同步切换到挑战协议（淘汰重发密码形态）。
- 会话承载：`AppClaims` 增加 `amr`（认证方式列表）声明并 bump `version` 演进规则，`claims_for_refresh` 继承路径同步；`user_session` 表加认证级别列**为可选项**（仅当设备管理要展示因子级别时再做，见六.2）。
- 前端：`LoginPage` 拆分为方式选择器 + 统一第二步骤编排；`SessionController` 承载「部分认证」中间态；MFA 弹窗复用现有 `MfaChallengeDialog`。
- 限流/防枚举边界按第三节不变量逐项过：pending token 不泄露 MFA 配置细节（只含已验证事实）；challenge 完成与失败均计 `AuthOperation::Login` 预算。
- **跨仓库**：框架改 lib_yang，推送顺序先 lib_yang 后 yang-system。

**验收条件**：

1. 密码/邮箱验证码两种第一因子 × TOTP/恢复码/备用邮箱码三种第二因子的全部合法组合可登录（集成测试矩阵）。
2. pending token 过期/重放/串 subject 均被拒；不得兑换任何会话能力。
3. 现有无状态两段式客户端行为迁移后，e2e（dev-server + production-build）全绿。
4. access token claims 携带 `amr`；登录事件可区分第一因子种类。

### 阶段 3（独立排期）：Passkey/WebAuthn 评估

- 从零引入 `webauthn-rs`、新增凭证表（声明式 schema）、前端平台差异与恢复流程。
- 定位：既可作第一因子（免密）也可作第二因子；落地后接入阶段 2 的挑战协议而非单独通道。
- 按路线图既定纪律「TOTP 落地后再评估」，本方案不展开，单独立项。

## 五、实施纪律（沿用路线图第六节）

- 新 Action 用 `scripts/new_action.py` 脚手架，改完跑 `check_architecture.py`；契约变更重跑 `dump_openapi.py` 并提交两个生成物。
- 提交前 `run_ci.py quick`，推送前 `run_ci.py full`；两阶段/挑战协议行为补 `run_ci.py integration` 对抗性测试（单次消费、防枚举、限流维度、重放）。
- 新增密钥域（pending token 签名钥）必须同步 `config.example.toml` 与 `docs/contracts/CONFIGURATION.md`，启动校验禁止复用。

## 六、已决决策与开放决策点

**已决（2026-11 确认）**：

- **D-1 阶段 1 选型 = 方案 A**（引擎加 `verify_only` 只验不消费接口）。理由：语义干净、零新增配置、与阶段 2 正交不报废；唯一实质风险（peek 必须共享错误计数/销毁语义）已写入阶段 1 实现红线。方案 B 仅在「必须单仓库紧急堵洞」时作临时补丁。
- **D-2 邮箱验证码作第二因子 = 严格禁用**：第一因子为邮箱验证码时，`[email.mfa]` 备用通道不可用，第二因子只接受 TOTP / 恢复码。密码登录的备用邮箱通道保留。

**仍开放**：

1. **`amr` 声明与 `user_session` 认证级别列的必要性**：阶段 2 是否必须，还是等设备管理展示需求出现再加。
2. **pending token 防重放**：纯 HMAC JWT（无状态，TTL 内可重放）还是 Redis 单次消费（对齐 step-up proof）。倾向 Redis 单次消费。
3. **阶段 2 是否保留旧两段式兼容期**：双形态并存一个发布窗口，还是直接切换（影响前端 SessionController 迁移成本）。

## 七、审查记录（方案自审结论）

| 发现 | 修正 |
|---|---|
| 「任选一种通过即可登录」跨类别不成立（TOTP 不能当第一因子） | 改为「同一因子类别内任选」的分类模型（第一节） |
| 邮箱验证码登录绕过已激活的 TOTP | 列为 G-1 bug 级缺口，阶段 1 必修，独立于完整改造 |
| 两段式与验证码单次消费矛盾 | 阶段 1 拆出方案 A/B 两个候选，记录引擎无 peek 接口的核实事实；已决 A（D-1） |
| 密码 + 邮箱码同类不算双因子 | 写入不变量 5；已决严格禁用备用邮箱通道（D-2） |
| pending token 若复用既有密钥域会违反 keyring 隔离 | 明确独立密钥域 + 启动校验 |

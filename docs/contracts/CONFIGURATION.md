# 启动配置契约

yang-system 使用单一启动期配置合成器，优先级固定为：

```text
config.toml < YANG_SYSTEM_* 环境变量 < 目录型 secret provider
```

合成、反序列化和安全校验只发生一次；应用运行期只持有不可变的强类型
`Settings`，不轮询配置文件，也不存在第二套动态配置注册表。

## 环境变量

配置字段按 `YANG_SYSTEM_{SECTION}_{FIELD}` 映射为大写环境变量。例如：

| TOML 字段 | 环境变量 |
|---|---|
| `app.environment` | `YANG_SYSTEM_APP_ENVIRONMENT` |
| `http.max_concurrency` | `YANG_SYSTEM_HTTP_MAX_CONCURRENCY` |
| `mysql.url` | `YANG_SYSTEM_MYSQL_URL` |
| `email.password_reset.link_base_url` | `YANG_SYSTEM_EMAIL_PASSWORD_RESET_LINK_BASE_URL` |
| `token.active_secret` | `YANG_SYSTEM_TOKEN_ACTIVE_SECRET` |
| `security.trusted_proxy_cidrs` | `YANG_SYSTEM_SECURITY_TRUSTED_PROXY_CIDRS` |
| `shutdown.total_timeout_seconds` | `YANG_SYSTEM_SHUTDOWN_TOTAL_TIMEOUT_SECONDS` |
| `observability.metrics_enabled` | `YANG_SYSTEM_OBSERVABILITY_METRICS_ENABLED` |
| `observability.metrics_bind` | `YANG_SYSTEM_OBSERVABILITY_METRICS_BIND` |
| `observability.traces_enabled` | `YANG_SYSTEM_OBSERVABILITY_TRACES_ENABLED` |
| `observability.traces_otlp_endpoint` | `YANG_SYSTEM_OBSERVABILITY_TRACES_OTLP_ENDPOINT` |
| `observability.traces_sample_ratio` | `YANG_SYSTEM_OBSERVABILITY_TRACES_SAMPLE_RATIO` |
| `observability.traces_export_timeout_seconds` | `YANG_SYSTEM_OBSERVABILITY_TRACES_EXPORT_TIMEOUT_SECONDS` |
| `observability.readiness_budget_ms` | `YANG_SYSTEM_OBSERVABILITY_READINESS_BUDGET_MS` |

所有配置字段均支持该映射，包括未出现在 `config.example.toml` 中、由内置
默认值承接的调优项（见下节）。整数使用非负十进制，
`traces_sample_ratio` 使用有限浮点数，
布尔值只接受小写 `true`/`false`，字符串列表使用逗号分隔。可选的
`max_lifetime_seconds` 可用空字符串或 `none` 清除。不认识的
`YANG_SYSTEM_*` 变量会让启动失败，避免拼写错误被静默忽略；
`YANG_SYSTEM_TEST_*` 保留给测试门禁。

## 内置默认值

`config.example.toml` 只保留必须填写的环境事实与部署决策（URL、密钥、
`app.environment`、`authorization.deployment`、`issue_refresh_credential_version`
等）。下列调优项全部有内置默认值，需要偏离时按原字段名在 `config.toml`
或环境变量中显式覆盖即可；省略对应配置段同样生效。逐项注释版的全量参考见
仓库根目录 `config.show.toml`（由 `config::tests` 的同步测试保证与代码一致）。

| 字段 | 默认值 |
|---|---|
| `app.name` | `yang-system` |
| `http.bind` | `127.0.0.1:8080` |
| `http.max_body_bytes` | `1048576`（1 MiB，允许至 16 MiB） |
| `http.request_timeout_seconds` | `30` |
| `http.max_concurrency` | `256` |
| `mysql.max_connections` / `min_connections` | `20` / `2` |
| `mysql.connect_timeout_seconds` / `idle_timeout_seconds` / `max_lifetime_seconds` | `10` / `600` / `1800` |
| `mysql.test_before_acquire` | `true` |
| `redis.max_connections` / `min_connections` | `20` / `2` |
| `redis.connect_timeout_seconds` / `wait_timeout_seconds` / `idle_timeout_seconds` / `max_lifetime_seconds` | `5` / `10` / `300` / `1800` |
| `redis.test_before_acquire` | `true` |
| `authorization.outbox_poll_interval_ms` | `250`（传播 p99 ≤ 2s 契约上限，不允许调大） |
| `authorization.outbox_batch_size` / `outbox_lease_seconds` / `outbox_max_retry_seconds` | `100` / `10` / `60` |
| `token.issuer` / `token.audience` | `yang-system` / `yang-system-api` |
| `token.access_ttl_seconds` / `refresh_ttl_seconds` | `3600` / `2592000`（30 天） |
| `step_up.issuer` / `step_up.audience` | `yang-system-step-up` / `yang-system-sensitive-actions` |
| `step_up.challenge_ttl_seconds` / `proof_ttl_seconds` | `120` / `300` |
| `email.smtp.port` / `timeout_seconds` | `587`（强制 STARTTLS）/ `10` |
| `email.{verification,change,mfa}.namespace` | 继承 `authorization.deployment` |
| `email.{verification,change,mfa}.ttl_seconds` / `resend_cooldown_seconds` / `max_attempts` | `600` / `60` / `5` |
| `email.{verification,change,mfa}.send_window_seconds` | `3600` |
| `email.{verification,change,mfa}.send_ip_attempts` / `send_email_attempts` / `send_global_attempts` | `20` / `5` / `1000` |
| `security.argon2_max_concurrency` | `4` |
| `security.auth_rate_limit_window_seconds` / `ip_attempts` / `username_attempts` | `60` / `30` / `10` |
| `security.password_reset_ttl_seconds` | `900` |
| `security.trusted_proxy_cidrs` | `[]`（完全忽略 Forwarded/X-Forwarded-For） |
| `security.totp.digits` | `6` |
| `observability.metrics_bind` | `127.0.0.1:9090` |
| `observability.traces_*` | 关闭；端点 `http://127.0.0.1:4317`，采样 `0.1`，导出超时 `5s` |
| `observability.readiness_budget_ms` | `2000`（允许 50..=10000） |
| `shutdown.total_timeout_seconds` | `30` |
| `logging.filter` | `yang_system=info,tower_http=info` |

`app.environment=production` 时必须启用 `observability.metrics_enabled`，以保证
独立管理面 `/metrics` 与预算化 `/health/ready` 一定存在；开发与测试环境可以显式
关闭。

`token.retiring_keys` 是对象数组，环境变量使用显式的
`YANG_SYSTEM_TOKEN_RETIRING_KEYS_JSON`，例如：

```json
[{"key_id":"2026-06","secret":"at-least-32-bytes-retiring-secret"}]
```

## Secret provider

设置 `YANG_SYSTEM_SECRET_DIR` 后，加载器会从该目录读取下列可选 UTF-8
单行文件，并在环境变量之后覆盖对应敏感字段：

| 文件名 | 目标字段 |
|---|---|
| `mysql_url` | `mysql.url` |
| `redis_url` | `redis.url` |
| `token_active_secret` | `token.active_secret` |
| `token_retiring_keys_json` | `token.retiring_keys`（JSON 对象数组） |
| `step_up_active_secret` | `step_up.active_secret` |
| `step_up_retiring_keys_json` | `step_up.retiring_keys`（JSON 对象数组） |
| `email_smtp_password` | `email.smtp.password` |
| `email_verification_secret` | `email.verification.secret` |
| `feishu_management_api_token` | `feishu.management_api_token` |
| `feishu_encryption_key` | `feishu.encryption_key` |

每个文件上限 64 KiB；允许一个结尾换行，拒绝空值、内嵌换行、NUL 和非
UTF-8 内容。目录一旦显式配置却不可访问，进程会失败关闭；单个文件缺失则
回退到环境变量或配置文件。文件名固定，不能由外部输入拼接路径。

生产环境建议让 Kubernetes/Docker secret、systemd credentials 或同类设施
把 secret 只读挂载到独立目录。不要把原始 token secret 或数据库密码提交到 Git。

## Token keyring 轮换

`token.active_key_id` 与 `token.active_secret` 只负责签发新 Token；
`token.retiring_keys` 只负责验证存量 Token。轮换顺序固定为：

1. 把旧 active key 移入 retiring，同时部署新 active key；
2. 等待至少一个 `refresh_ttl_seconds`，确保旧 Refresh Token 全部自然过期；
3. 从 retiring 移除旧 key。

keyring 最多 8 把密钥，`key_id` 必须唯一。生产 Token 强制携带 `kid`，
缺失或未知 `kid` 均失败关闭。首次从旧单密钥版本升级时，既有无 `kid`
会话会失效并要求重新登录；系统不保留隐式逐密钥试签名的兼容回退链。

## 服务凭据轮换（MySQL / Redis / SMTP）

Token 与 Step-up keyring 之外的凭据（`mysql.url`、`redis.url`、
`email.smtp.username` / `email.smtp.password`）没有 keyring 机制：新值只在
**进程启动期**合成一次，不支持热更新。轮换的通用步骤固定为：

1. 在依赖侧先让新凭据生效（不要先吊销旧凭据）；
2. 更新 secret 目录文件或 `YANG_SYSTEM_*` 环境变量；
3. **滚动重启**应用实例：逐个实例替换，依赖管理面 `/health/ready` 确认新实例
   就绪后再下线旧实例，保证零停机；
4. 确认全部实例已用新凭据运行后，回依赖侧吊销旧凭据。

连接池内已建立的旧连接最长存活 `max_lifetime_seconds`（MySQL/Redis 默认
1800 秒），吊销旧凭据前预留至少一个该周期，避免池内连接被集中踢断。

### MySQL

- 推荐为应用使用专用账号（非 root）。轮换时优先「新建账号 → 切换 → 删除旧账号」；
  若必须原地 `ALTER USER ... IDENTIFIED BY ...`，先执行改密再立即滚动重启，
  因为旧连接不受影响但新连接会立刻要求新密码。
- 连接串经 secret 文件 `mysql_url` 或 `YANG_SYSTEM_MYSQL_URL` 注入；
  不要把新密码写进 `config.toml`。

### Redis

- 凭据内嵌在 `redis.url`（`redis://[:password@]host:port/db`，或 ACL 用户
  `redis://user:password@...`），经 secret 文件 `redis_url` 或
  `YANG_SYSTEM_REDIS_URL` 注入，轮换流程同上。
- Redis 只是加速层（授权最终事实在 MySQL，含 Outbox）：轮换窗口内 Redis 短暂
  不可用只影响限流计数、Step-up proof 与缓存命中率，不丢业务数据；但应用对
  Redis 连接失败按失败关闭处理，重启期间保持 Redis 可达。

### SMTP

- `email.smtp` 凭据承载全部事务性邮件投递（注册验证码、密码重置链接、新设备登录提醒、
  MFA 备用验证码、免密登录验证码）。先在 relay 侧添加新凭据，再按
  通用步骤滚动重启，最后吊销旧凭据。
- 轮换失误（旧凭据提前失效）不会导致启动失败，但会让邮件投递失败；通过
  `yang_system_registration_email_total{result}` 指标观察 `error` 结果突增
  即可发现，修复方式是部署正确凭据并再次滚动重启。
- `email.password_reset.link_base_url` 是密码重置邮件中链接指向的前端控制台入口
  （scheme + host[:port]，无路径与查询串），生产环境强制 `https`。该地址只从配置
  注入，不得从请求 `Host` 头推导——Host 头可被攻击者伪造，伪造链接会把一次性
  重置凭证导向攻击者控制的站点。

### 邮箱换绑验证码（`email.change`）

- `email.change` 是邮箱换绑验证码的独立配置段（与 `email.verification` 注册验证码
  完全隔离）：独立 `namespace`（Redis key 前缀 `yang-system:<ns>:change-email`）、
  独立 `secret`。**启动校验拒绝** `email.change.secret` 复用注册验证码、Token 或
  Step-up 密钥——否则换绑验证码可被注册验证码的 key 域重放，或跨密钥域混淆。
- 该段可省略（`#[serde(default)]`）：省略时 `change_email`/`request_change_email`
  Action 不注册，换绑能力不可用；显式配置后需重启生效。
- 字段语义与 `email.verification` 一致（TTL/冷却/尝试上限/发送额度），且全部
  字段有内置默认值：`namespace` 缺省继承 `authorization.deployment`，其余取
  「内置默认值」表中的验证码默认值；最小配置只需填写 `secret`。

### 登录 MFA 备用邮箱验证码（`email.mfa`）

- `email.mfa` 是登录 MFA 备用邮箱验证码的独立配置段（与注册/换绑验证码完全隔离）：
  独立 `namespace`（Redis key 前缀 `yang-system:<ns>:mfa-email`）、独立 `secret`。
  **启动校验拒绝** `email.mfa.secret` 复用注册验证码、换绑验证码、Token、Step-up
  或 `security.totp.aead_key` 密钥——否则验证码可跨场景重放。
- 用途：账号已激活 TOTP 但认证器不可用时，登录第二因子可改用注册邮箱接收的
  一次性验证码（`POST /api/v1/users/mfa/email-code` 请求发码，等时密码校验防枚举，
  失败计数与登录共用同一限流预算）。**这是弱于 TOTP 的降级通道**：邮箱失守 +
  密码泄露即等于账号失守，定位与恢复码同级；验证码单次消费、短 TTL、尝试
  次数用尽即销毁。
- 该段可省略（`#[serde(default)]`）：省略时发码端点返回「未启用」错误，
  登录第二因子仅接受认证器动态码与恢复码；显式配置后需重启生效。
- 字段语义与 `email.verification` 一致（TTL/冷却/尝试上限/发送额度），且全部
  字段有内置默认值：`namespace` 缺省继承 `authorization.deployment`，其余取
  「内置默认值」表中的验证码默认值；最小配置只需填写 `secret`。

### 邮箱验证码免密登录（`email.login`）

- `email.login` 是邮箱验证码免密登录的独立配置段（与注册/换绑/MFA 验证码完全
  隔离）：独立 `namespace`（Redis key 前缀 `yang-system:<ns>:login-email`）、
  独立 `secret`。**启动校验拒绝** `email.login.secret` 复用注册验证码、换绑
  验证码、MFA 验证码、Token、Step-up 或 `security.totp.aead_key` 密钥——
  否则验证码可跨场景重放（注册码登录、登录码换绑等）。
- 用途：不输密码、凭邮箱一次性验证码直接登录（`POST /api/v1/users/login-email-code`
  请求发码，`POST /api/v1/users/login-by-email-code` 消费码并签发 Token）。
  发码仅对「邮箱已注册且账号启用」真实投递，其余只耗限额、返回统一 accepted；
  验证码错误/邮箱未注册/账号停用统一返回「邮箱验证码无效或已过期」；限流与
  密码登录共用 `AuthOperation::Login` 预算，验证码单次消费、错误尝试用尽即销毁。
- 该段可省略（`#[serde(default)]`）：省略时发码端点返回「未启用」错误，
  免密登录不可用，密码登录不受影响；显式配置后需重启生效。
- 字段语义与 `email.verification` 一致（TTL/冷却/尝试上限/发送额度），且全部
  字段有内置默认值：`namespace` 缺省继承 `authorization.deployment`，其余取
  「内置默认值」表中的验证码默认值；最小配置只需填写 `secret`。

### TOTP 第二因子（`security.totp`）

- `security.totp.aead_key` 是加密 `users.totp_secret` 的**独立密钥域**（32 字节）。
  **启动校验拒绝**占位值、重复字节，且该密钥域不得与 token/step-up/邮箱验证码
  密钥复用——AEAD 解密失败（密钥域不匹配）会按配置损坏拒绝登录的 MFA 阶段。
- 该段可省略：省略时 `totp_setup`/`totp_activate`/`totp_deactivate` Action 不注册，
  登录与 Step-up 回退单因子（既有无 TOTP 账号不受影响）。
- `totp_deactivate`（自助关闭双重验证）要求登录 + Step-up 重认证；已激活账号的
  Step-up 会同时要求出示第二因子（动态码/恢复码/邮箱验证码），即必须证明仍持有
  第二因子才能关闭它。停用成功后密钥、激活时间与全部恢复码即时作废，既有会话
  全部失效（凭据版本递增），需重新登录。
- 密钥轮换：滚动更新 `aead_key` 会让已存 TOTP 密文无法解密（用户在下次登录时
  被要求重新 setup）——属预期行为；如需无缝轮换需先实现多 keyring 版本化。

### 飞书外部数据源集成（`feishu`）

整段可选：省略时相关路由不注册，服务行为与未集成飞书时完全一致。

- `feishu.enabled`（布尔，默认 `false`）：是否启用飞书集成。
- `feishu.management_api_token`（文本，启用时必填）：飞书多维表格自动化工作流调用
  **写入 API** 时使用的静态 Token。至少 32 字节；不得复用 `token` / `step_up` /
  `security.totp` 的密钥，也不得使用占位值或重复字节。
- `feishu.encryption_key`（文本，可选）：外部选项接口的 AES 密钥原文，按
  `sha256(原文)` 派生 256 位密钥。**省略表示明文返回**，对应飞书审批后台
  「不填写 Key」的配置；填写后响应体的 `data.result` 变为 base64 密文。同样受
  密钥域隔离约束。
- `feishu.app_id`（文本，可选）：自建应用的 App ID。与 `app_secret` 成对出现，
  用于**出站**调用飞书开放平台换取 `tenant_access_token`。省略表示不出站。
- `feishu.app_secret`（文本，可选）：自建应用的 App Secret。**建议只走 secret 目录
  `feishu_app_secret`**（见下）。与 `management_api_token` 是两条无关凭证，且它是
  **租户级**凭证——拿到它能读该应用可见的全部协作多维表格，不要写进配置文件或 Git。
- `feishu.pull_interval_seconds`（整数，默认 `900`）：出站拉取轮询间隔，有效范围
  `10..=86400`。契约是「最大可见延迟 = 一个轮询间隔」。
- `feishu.alert_recipients`（字符串数组，默认 `[]`）：出站拉取**连续失败**达阈值时的
  告警收件人。**默认空 = 不告警**。每一项都必须是可投递的地址（空白项、缺 `@`、
  域名缺 `.` 都会在启动期被拒）——一个空白项等于「以为配上了其实没配」，而那种错
  在投递那一刻只会静默失败。
- `feishu.alert_failure_threshold`（整数，默认 `3`）：连续失败多少轮之后开始告警，
  有效范围 `2..=1000`。达阈值后**每轮都发**，直到有一轮成功把计数清零——收口条件
  是「恢复」而不是冷却。下限 2：单次失败与飞书侧抖动无法区分，阈值 1 等于把抖动
  变成邮件；要彻底静音请清空 `alert_recipients`，而不是把阈值调到天上。

对应环境变量：`YANG_SYSTEM_FEISHU_ENABLED`、`YANG_SYSTEM_FEISHU_MANAGEMENT_API_TOKEN`、
`YANG_SYSTEM_FEISHU_ENCRYPTION_KEY`。

`app_id` / `app_secret` / `pull_interval_seconds` / `alert_recipients` /
`alert_failure_threshold` **不登记环境变量**。这是刻意的：
环境变量是白名单，未登记的名称会让进程启动失败——`YANG_SYSTEM_FEISHU_APP_SECRET`
因此会被直接拒绝，secret 只能从 secret 目录进来。

**出站可用性与入站可用性是两个独立判据**：

| 谓词 | 含义 | 需要哪些配置 |
|---|---|---|
| `is_usable()` | 注册**入站**路由（飞书来取选项 / 多维表格来推选项） | `enabled` + `management_api_token` 非空白 |
| `can_pull()` | 允许**出站**拉取飞书开放平台 | `enabled` + `app_id`/`app_secret` 均**非空白且非占位** + 间隔在范围内 |

两者刻意分开：并成一个会改变入站路由的注册条件，让一次「还没配 `app_id`」的滚动发布
把已经在跑的入站端点一起摘掉。`can_pull()` 会把 `CHANGE_ME_*` / `replace-with*` /
`placeholder` 这类占位值判为**未配置**，避免 worker 拿占位凭证按间隔反复出网。

**校验只在段真正生效时执行**（`enabled = true`）。段存在但惰性时（`enabled = false`，
或凭证留待运维后填）不做校验，也不注册任何路由——惰性段没有可被误用的行为面，
不应让进程起不来。注意：`enabled = true` 时 `pull_interval_seconds` 越界、
`alert_failure_threshold` 越界、`alert_recipients` 里有不可投递的地址，都会
**拒绝启动**，即使出站凭证还没配——错在配置里，越早暴露越好。

**出站凭证不套用本系统自己的密钥强度规则**（≥32 字节、非重复字符）：那是约束我们自己
签发的密钥的，飞书 `app_secret` 是第三方凭证，套上去会把合法配置判非法。它只受
「非占位」与密钥域隔离（不得复用 `token` / `step_up` / `security.totp` 的密钥）约束。

**注意**：外部选项接口另有**按数据源**的 Token，与本段的 `management_api_token` 是
两条独立凭证。前者校验**飞书来取选项**的请求来源，以 SHA-256 摘要存于
`feishu_datasource.token_hash`，永不存明文；后者保护**入站写入**（多维表格来调我们）。
二者都是**入站**方向；`app_id`/`app_secret` 才是**出站**方向（我们主动调飞书）。


## 关闭总预算

`shutdown.total_timeout_seconds` 是进程关闭的唯一总预算，默认 30 秒，允许
范围为 1..=300 秒。收到 SIGINT/SIGTERM 后开始计时，HTTP 请求排空、授权
Outbox Worker 退出、MySQL/Redis 资源关闭以及 Prometheus/OTLP 运行时关闭依次
消费同一个截止时间，不会把多个阶段超时相加。若服务在收到信号前失败，则从该
失败出口开始计时。

PowerShell 示例：

```powershell
$env:YANG_SYSTEM_APP_ENVIRONMENT = 'production'
$env:YANG_SYSTEM_SECRET_DIR = 'C:\run\secrets\yang-system'
cargo run --locked
```

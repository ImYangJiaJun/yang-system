# 启动配置契约

yang-system 与 `yang-migrate` 共用一个启动期配置合成器，优先级固定为：

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
| `token.active_secret` | `YANG_SYSTEM_TOKEN_ACTIVE_SECRET` |
| `security.trusted_proxy_cidrs` | `YANG_SYSTEM_SECURITY_TRUSTED_PROXY_CIDRS` |
| `shutdown.total_timeout_seconds` | `YANG_SYSTEM_SHUTDOWN_TOTAL_TIMEOUT_SECONDS` |
| `observability.metrics_enabled` | `YANG_SYSTEM_OBSERVABILITY_METRICS_ENABLED` |
| `observability.metrics_bind` | `YANG_SYSTEM_OBSERVABILITY_METRICS_BIND` |
| `observability.traces_enabled` | `YANG_SYSTEM_OBSERVABILITY_TRACES_ENABLED` |
| `observability.traces_otlp_endpoint` | `YANG_SYSTEM_OBSERVABILITY_TRACES_OTLP_ENDPOINT` |
| `observability.traces_sample_ratio` | `YANG_SYSTEM_OBSERVABILITY_TRACES_SAMPLE_RATIO` |
| `observability.traces_export_timeout_seconds` | `YANG_SYSTEM_OBSERVABILITY_TRACES_EXPORT_TIMEOUT_SECONDS` |

`config.example.toml` 中的所有字段均支持该映射。整数使用非负十进制，
`traces_sample_ratio` 使用有限浮点数，
布尔值只接受小写 `true`/`false`，字符串列表使用逗号分隔。可选的
`max_lifetime_seconds` 可用空字符串或 `none` 清除。不认识的
`YANG_SYSTEM_*` 变量会让启动失败，避免拼写错误被静默忽略；
`YANG_SYSTEM_TEST_*` 保留给测试门禁。

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
| `bootstrap_secret_digest` | `bootstrap.secret_digest` |

每个文件上限 64 KiB；允许一个结尾换行，拒绝空值、内嵌换行、NUL 和非
UTF-8 内容。目录一旦显式配置却不可访问，进程会失败关闭；单个文件缺失则
回退到环境变量或配置文件。文件名固定，不能由外部输入拼接路径。

生产环境建议让 Kubernetes/Docker secret、systemd credentials 或同类设施
把 secret 只读挂载到独立目录。不要把原始 token secret、数据库密码或
bootstrap 原始 secret 提交到 Git；bootstrap provider 保存的是 Argon2id
摘要，而不是操作员输入的原文。

## Token keyring 轮换

`token.active_key_id` 与 `token.active_secret` 只负责签发新 Token；
`token.retiring_keys` 只负责验证存量 Token。轮换顺序固定为：

1. 把旧 active key 移入 retiring，同时部署新 active key；
2. 等待至少一个 `refresh_ttl_seconds`，确保旧 Refresh Token 全部自然过期；
3. 从 retiring 移除旧 key。

keyring 最多 8 把密钥，`key_id` 必须唯一。生产 Token 强制携带 `kid`，
缺失或未知 `kid` 均失败关闭。首次从旧单密钥版本升级时，既有无 `kid`
会话会失效并要求重新登录；系统不保留隐式逐密钥试签名的兼容回退链。

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

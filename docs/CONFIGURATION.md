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
| `token.secret` | `YANG_SYSTEM_TOKEN_SECRET` |
| `security.trusted_proxy_cidrs` | `YANG_SYSTEM_SECURITY_TRUSTED_PROXY_CIDRS` |

`config.example.toml` 中的所有字段均支持该映射。数值使用非负十进制，
布尔值只接受小写 `true`/`false`，字符串列表使用逗号分隔。可选的
`max_lifetime_seconds` 可用空字符串或 `none` 清除。不认识的
`YANG_SYSTEM_*` 变量会让启动失败，避免拼写错误被静默忽略；
`YANG_SYSTEM_TEST_*` 保留给测试门禁。

## Secret provider

设置 `YANG_SYSTEM_SECRET_DIR` 后，加载器会从该目录读取下列可选 UTF-8
单行文件，并在环境变量之后覆盖对应敏感字段：

| 文件名 | 目标字段 |
|---|---|
| `mysql_url` | `mysql.url` |
| `redis_url` | `redis.url` |
| `token_secret` | `token.secret` |
| `bootstrap_secret_digest` | `bootstrap.secret_digest` |

每个文件上限 64 KiB；允许一个结尾换行，拒绝空值、内嵌换行、NUL 和非
UTF-8 内容。目录一旦显式配置却不可访问，进程会失败关闭；单个文件缺失则
回退到环境变量或配置文件。文件名固定，不能由外部输入拼接路径。

生产环境建议让 Kubernetes/Docker secret、systemd credentials 或同类设施
把 secret 只读挂载到独立目录。不要把原始 token secret、数据库密码或
bootstrap 原始 secret 提交到 Git；bootstrap provider 保存的是 Argon2id
摘要，而不是操作员输入的原文。

PowerShell 示例：

```powershell
$env:YANG_SYSTEM_APP_ENVIRONMENT = 'production'
$env:YANG_SYSTEM_SECRET_DIR = 'C:\run\secrets\yang-system'
cargo run --locked
```

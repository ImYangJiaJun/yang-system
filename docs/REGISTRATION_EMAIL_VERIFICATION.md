# 注册邮箱验证码

公开注册现在把“拥有该邮箱”作为创建全局 `users` 账户的前置事实。企业成员资料中的
`org_user.email` 仍是租户内展示字段，不承担登录、找回或全局身份语义。

## HTTP 契约

1. `POST /api/v1/users/registration-email-verifications` 接收 `email`，成功时固定返回
   `202 Accepted` 与通用的 `accepted/expires_in/resend_after`，响应不包含邮箱或验证码。
2. `POST /api/v1/users/register` 必须同时提交 `username/password/email/email_code`；只有
   Redis 中与规范化邮箱绑定的验证码被原子消费后，才写入 `users.email` 与
   `users.email_verified_at`。
3. 验证码无效、过期、已用、尝试次数耗尽或邮箱已注册时，注册接口都使用同一
   `email_code` 拒绝语义；数据库唯一键竞态使用通用“用户名或邮箱已被注册”错误。

邮箱只接受最长 254 字节的严格 ASCII 地址并统一转为小写。该规范化策略不尝试按
供应商折叠 `+tag` 或点号；如果业务将来需要供应商特定别名策略，必须单独迁移，不能
静默改变现有唯一性。

## 安全边界

- 6 位验证码由操作系统 CSPRNG 以拒绝采样生成，避免取模偏差。
- Redis key 中的邮箱和来源 IP 使用独立服务端密钥的 HMAC-SHA256 指纹；value 只保存
  邮箱绑定的 HMAC-SHA256 验证码摘要与错误次数，不保存原文。
- Lua 脚本原子完成单次消费和错误次数递增；成功、并发重放、过期与超过尝试上限均
  不会留下可复用凭证。
- 发送入口同时受单 IP、单邮箱、全局窗口和重发冷却限制。已有邮箱仍经过相同限流，
  但不投递邮件，避免通过响应判断账户是否存在。
- SMTP 只使用 STARTTLS relay；投递错误不会携带 SMTP 响应、收件人或凭据。投递失败
  会按摘要条件删除本次验证码，避免失败邮件留下可消费状态。
- `email.verification.secret` 必须独立于 Access/Refresh Token 和 Step-up 密钥。

## 配置与 secret provider

完整字段见 `config.example.toml`。生产至少需要配置 SMTP relay、发件人、独立验证码
密钥以及部署隔离的 Redis namespace。所有字段均可用 `YANG_SYSTEM_EMAIL_*` 环境变量
覆盖；更推荐通过 `YANG_SYSTEM_SECRET_DIR` 挂载：

- `email_smtp_password`
- `email_verification_secret`

secret 文件必须是权限受限的单行 UTF-8 文本。SMTP 用户名和密码必须同时配置或同时
留空；示例占位值会在连接外部资源前阻止启动。当前适配器不支持明文 SMTP 或隐式
TLS 端口，若供应商只有其他传输模式，应增加并审计新的显式适配器。

## 发布、观测与恢复

先运行 `yang-migrate plan/apply` 应用
`20260731_0017_add_users_verified_email`，再滚动应用。迁移只新增可空列、唯一索引和
“邮箱与验证时间同时为空或同时非空”的强制 CHECK，因此旧账户继续可登录；新注册
必须完成邮箱验证。生产等量 staging 仍需测量唯一索引 DDL 的 metadata lock、复制延迟
和写入影响。

关注以下有限标签指标：

- `yang_system_registration_email_total{result=sent|suppressed|cooldown|limited|failed}`
- `yang_system_registration_email_verify_total{result=consumed|denied}`

不得记录验证码、邮箱、SMTP 响应或 Redis key。SMTP 故障先检查 `failed` 比例和供应商
健康状态；恢复后客户端重新获取验证码。验证码摘要删除或过期无需数据库补偿。

真实对抗门禁使用独立 `_test` MySQL 与 Redis DB 15：

```powershell
$env:YANG_SYSTEM_TEST_DATABASE_URL = "mysql://<dedicated-database-ending-in-_test>"
$env:YANG_SYSTEM_TEST_REDIS_URL = "redis://<dedicated-redis>/15"
cargo test --test registration_email_integration --locked -- `
  --ignored --test-threads=1
```

该用例覆盖迁移约束、Redis 明文泄漏、冷却、已有账户抑制、错误次数、过期、SMTP
失败清理以及同一验证码并发只能成功一次。

# 会话撤销与 Refresh 持久化

`user_session` 是设备会话的事实源。Access 与 Refresh 必须携带 `session_id`，且该行
属于 Token 的用户并且 `revoked_at IS NULL`；缺少标识、行缺失或归属不符均要求重新登录。
没有会话行的旧 Token 不再走兼容放行，不回填猜测的会话记录。

所有使用 `AuthorizationVersionValidator` 的模块都会先查 MySQL 会话，再判定授权版本。
授权缓存命中不能跳过会话撤销；历史 Access JTI 无须逐个保存或拉黑。一次检查之后已开始
的请求不会被追溯中断，撤销提交后新开始的认证检查会拒绝该设备，其他设备保持有效。
会话查询超过 5 秒、存储不可用或查询失败属于授权检查不可用，不能伪装成凭据错误。

Refresh 的授权快照必须同时验证当前 `refresh_jti`，随后框架执行原有签名、时效、Redis
黑名单与原子消费检查。新 Token 对生成后，在返回正文或 Cookie 前执行 MySQL 条件更新：
用户、会话、旧 Refresh JTI 和未撤销状态都须匹配，且影响行数必须为 1。`last_seen_at`
仍按 60 秒节流，两个 JTI 每次成功轮换都更新。

Redis 清空不能让已成功轮换的旧 Refresh 再次使用，也不能恢复已撤销设备的 Access。
Redis 不可达继续按框架规则拒绝认证。Redis 消费后 MySQL 更新失败，或数据库已提交但
响应丢失，可能需要重新登录；系统不会为了避免重新登录而接受已消费 JTI。这不证明
TOTP、验证码或其他 Redis 安全状态在丢失后均已可恢复。

发布时必须让所有认证读者与 Refresh 写者升级到此契约，旧实例仍提供服务期间不具备
上述完整保证。现有 Access 需要有效会话行；Refresh 还要求该行的 `refresh_jti` 非空
且匹配当前令牌。无会话标识或历史 `refresh_jti = NULL` 的旧 Refresh 需要重新登录。
本变更复用现有表结构，不执行数据清理或 SQL 迁移。

真实 MySQL/Redis 回归在 `tests/session_revocation_integration.rs`：历史 Access、旧 Refresh、
无会话/跨用户、并发撤销/推进代次/删除、另一设备正例。验证结果记录在专项修复报告；
本地验证不替代部署后的多实例、延迟与容量验收。

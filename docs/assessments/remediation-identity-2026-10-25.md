# 会话权威与 Refresh 轮换修复记录

日期：2026-10-25。覆盖 DI-02，以及 DI-03 中 Redis 消费记录丢失导致旧 Refresh 可重用的路径。对应未提交工作树；不代表生产已部署，也不关闭 DI-03 的 TOTP 与 MySQL 恢复回滚问题。

## 规则与实现

一次会话是否仍有效必须由持久事实判断，不能只依赖 Redis 中短期或可丢失的黑名单。Access 的有效签名、授权版本和 Redis 撤销校验之外，现在还必须具有属于该用户且未撤销的 MySQL 会话行。会话查询独立限制为 5 秒；连接池耗尽或查询失败返回 503，缺失、错属或撤销返回凭据失效。

Refresh 必须携带会话标识，并匹配该会话当前的 `refresh_jti`。签发新令牌对后、向调用方返回前，以用户、会话、旧 JTI、未撤销条件原子更新；影响行数必须为 1。并发撤销、删除或其他轮换使更新失败时，不向调用方返回新令牌。Redis 仍保留正常单次消费检查，MySQL 当前代次额外阻止空 Redis 恢复旧 Refresh。

没有会话标识、会话不存在或历史 `refresh_jti` 为 NULL 的凭据需要重新登录。其他设备会话保持有效，不通过提高全账号授权版本实现逐设备撤销。详细发布约束见 [SESSION_AUTHORITY](../contracts/SESSION_AUTHORITY.md)。没有新增表或迁移文件。

## 真实依赖验证

隔离 MySQL 容器 `codex-identity-remediation-mysql-20261025`，专用库 `identity_remediation_test`；隔离 Redis 容器 `codex-identity-remediation-redis-20261025`，仅使用 DB 15。端口只发布到 loopback。以下测试没有访问生产、发送真实飞书请求或使用生产账号。

`cargo test --test session_revocation_integration --locked -j 1 -- --ignored --test-threads=1`：7 passed、0 failed、0 ignored。日志：[最终绿测](../../target/infrastructure-remediation/identity/green-durable-sessions-final.log)。在原实现上的 [失败回归](../../target/infrastructure-remediation/identity/red-durable-sessions.log) 已复现新防护对应的业务断言失败。

新增覆盖：

- 目标设备所有仍在有效期内的新旧 Access，在撤销后及 Redis 消费记录清空后均拒绝；另一设备 Access 与 Refresh 正常。
- R1 已轮换为 R2 后清空隔离 Redis，R1 仍不能重用，R2 可继续正常轮换。
- 无会话、缺失会话、跨用户会话的 Access 与 Refresh 均拒绝。
- 使用真实行锁使请求停在持久化之前，分别撤销、推进旧 JTI、删除会话；三种竞态都不能返回成功令牌对。
- 耗尽 20 个连接后在查询预算内返回 503；释放连接后会话可用；会话表查询失败仍返回 503，而非误报 401 或放行。

竞态测试最初使用 `information_schema.innodb_trx` 的等待者连接判定未能观察到实际等待，已改为 `performance_schema.data_lock_waits` 与 `data_locks` 的真实锁等待。修正的是测试同步屏障；没有降低业务断言或用 sleep 替代竞态定位。

## 待验证与边界

8 项摘除式变异均有效变红（8/8：逐项退出码 101，对应真实依赖测试均为业务断言失败；结果见 [session-mutation-results.json](../../target/infrastructure-remediation/identity/session-mutation-results.json)，逐项日志 `target/infrastructure-remediation/identity/mutation-*.log`）。恢复原始字节后复跑全绿：7 项会话回归 7 passed、0 failed（[日志](../../target/infrastructure-remediation/identity/green-durable-sessions-post-mutation.log)）；`cargo test --lib` 1300 passed、0 failed、24 ignored（[日志](../../target/infrastructure-remediation/identity/green-lib-post-mutation.log)）。最终 full、integration、严格 Clippy 和冷缓存 MSRV 尚待新稳定快照统一运行，不能沿用旧全量绿色记录。

每个受保护请求新增 MySQL 会话查询，旧容量 benchmark 不再代表当前实现。此实现不会撤销已在校验后进入业务执行的请求，也不提供整个业务事务与会话撤销的串行化。Refresh 持久化结果未知时要求重新登录；框架签发事件可能早于应用会话提交，不能把该 tracing 事件解释为客户端已收到令牌。

本轮没有验证 MySQL 恢复旧备份后的安全世代、TOTP 消费状态丢失、混跑旧实例或生产容量；这些仍是独立开放项。

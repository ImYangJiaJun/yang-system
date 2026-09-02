# 授权事实 writer 边界

本清单是架构门禁的机器可读输入，不是“同一函数出现版本递增调用”的正则证明。事务正确性仍由 typed writer 与真实 MySQL 集成测试证明。

## 授权事实

| 表 | 字段 | 唯一写入语义 |
|---|---|---|
| `users` | `status` | 注册只能写初始 `active`；后续状态变更必须进入账号生命周期停用流程 |
| `users` | `authz_version` | 仅账号安全版本原语可单调递增，并在同事务追加 Outbox |
| `users` | `credential_version` | 仅凭据变更原语可与授权版本一起单调递增 |
| `authz_grant` | 整行 | 仅 access 授权存储 writer 可插入/删除；每次事实变更必须同事务经账号安全版本原语递增目标用户 `authz_version` 并追加 Outbox |

当前骨架的外围授权域（access）已登记在下方清单；未来其它 Addon（平台/企业等）
新增授权事实表与 writer 时，必须先在下方清单登记再写代码。

## 公共端口分层

`src/infrastructure/authorization/ports.rs` 定义授权失效公共端口（只含抽象，不含 SQL）：

- `AuthorizationVersionSource`：授权版本回源读取（Token 校验 fallback 与管理查询共享）；
- `AuthorizationVersionWriter`：事务内 `FOR UPDATE` 锁定 + 单调递增 `authz_version` + 追加 Outbox；
- `AuthorizationPort`：组合根（`src/app.rs`）装配一次后分发给校验器与业务 Addon 的句柄。

两个端口由 `account-security-version` writer（`src/addon/account/domain/authz_version.rs`
的 `AccountAuthorizationPort`）唯一实现，`users.authz_version` 仍只有上述一个 writer
文件、一条执行路径。业务 Addon 使某用户 Token 失效时只能经 `AuthorizationPort`，
不得直接依赖账号域函数或自写版本 SQL。

## 允许边界

| ID | 文件 | 责任 |
|---|---|---|
| `account-user-facts` | `src/addon/account/domain/repository.rs` | 用户注册的固定初始状态与密码摘要写入；不暴露用户状态更新 API |
| `account-security-version` | `src/addon/account/domain/authz_version.rs` | 用户状态锁、授权/凭据版本单调递增与 Outbox |
| `access-grant-lifecycle` | `src/addon/access/domain/repository.rs` | `authz_grant` 直授权限事实的读取、插入与删除；版本递增复用 `account-security-version` |

<!-- authorization-writer: account-user-facts src/addon/account/domain/repository.rs -->
<!-- authorization-writer: account-security-version src/addon/account/domain/authz_version.rs -->
<!-- authorization-writer: access-grant-lifecycle src/addon/access/domain/repository.rs -->

## 验证边界

- 架构检查器拒绝受保护 Module 的 Action 直接调用通用 insert/update/delete；
- 任意位置直接用 SQL 写 `users` 都必须位于上述 allowlist；
- writer 必须保持私有，并由真实 MySQL 测试逐项证明授权字段变化恰好递增一次，幂等/展示字段不递增，失败事务不留下版本或 Outbox；
- 测试中的故障注入 SQL 不属于生产边界，不能作为在线写入口。

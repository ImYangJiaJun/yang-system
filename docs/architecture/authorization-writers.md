# 授权事实 writer 边界

本清单是架构门禁的机器可读输入，不是“同一函数出现版本递增调用”的正则证明。事务正确性仍由 typed writer 与真实 MySQL 集成测试证明。

## 授权事实

| 表 | 字段 | 唯一写入语义 |
|---|---|---|
| `users` | `status` | 注册只能写初始 `active`；后续状态变更必须进入账号生命周期协调 writer |
| `users` | `authz_version` | 仅账号安全版本原语可单调递增，并在同事务追加 Outbox |
| `users` | `credential_version` | 仅凭据变更原语可与授权版本一起单调递增 |
| `admin_user` | `status`、`admin`、`user_user` | 平台 repository 在操作者事实与目标行锁内写入 |
| `org_user` | `status`、`admin`、`org_org`、`user_user` | 成员 repository 或首次 onboarding writer 在同一事务写入 |

## 允许边界

| ID | 文件 | 责任 |
|---|---|---|
| `account-user-facts` | `src/modules/account/user/repository.rs` | 用户注册的固定初始状态与密码摘要写入；不暴露用户状态更新 API |
| `account-security-version` | `src/modules/account/authz_version.rs` | 用户状态锁、授权/凭据版本单调递增与 Outbox |
| `admin-authorization-facts` | `src/modules/admin/user/repository.rs` | 平台账号创建、状态和管理员事实变更 |
| `org-membership-authorization-facts` | `src/modules/org/user/repository.rs` | 企业成员新增、更新、删除及受影响用户版本 |
| `org-onboarding-authorization-facts` | `src/modules/org/access/repository.rs` | 企业创建时首个管理员成员事实与创建者版本 |

<!-- authorization-writer: account-user-facts src/modules/account/user/repository.rs -->
<!-- authorization-writer: account-security-version src/modules/account/authz_version.rs -->
<!-- authorization-writer: admin-authorization-facts src/modules/admin/user/repository.rs -->
<!-- authorization-writer: org-membership-authorization-facts src/modules/org/user/repository.rs -->
<!-- authorization-writer: org-onboarding-authorization-facts src/modules/org/access/repository.rs -->

## 验证边界

- 架构检查器拒绝受保护 Module 的 Action/Service 直接调用通用 insert/update/delete；
- 任意位置直接用 SQL 写 `users`、`admin_user` 或 `org_user` 都必须位于上述 allowlist；
- writer 必须保持私有，并由真实 MySQL 测试逐项证明授权字段变化恰好递增一次，幂等/展示字段不递增，失败事务不留下版本或 Outbox；
- 测试中的故障注入 SQL 不属于生产边界，不能作为在线写入口。

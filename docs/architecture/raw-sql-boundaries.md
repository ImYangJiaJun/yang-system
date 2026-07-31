# 原始 SQL 边界清单

本文档枚举 `yang-system` 生产代码中允许直接调用 SQLx 的持久化逃生口。通用
CRUD、租户注入和字段权限仍由 YANG `TableQuery` 承担；只有 Join、行锁、Outbox
状态机、审计追加和 schema 校验等无法简洁表达的语义保留原始 SQL。

## 机械约束

- 每个含生产 SQLx 查询的文件必须声明唯一的
  `//! raw-sql-boundary: <kind> <id>`，并与下表一一对应；
- `domain-repository` 只能位于 `src/modules/**/repository.rs`；
- `domain-service` 只用于明确的授权快照、实时 guard 或锁服务边界；
- 基础设施例外只允许审计 repository、授权 Outbox repository 与审计 schema
  validator；
- 查询文本必须是代码中的静态字符串字面量，所有数据参数通过 `bind` 传入；
- 请求参数不得决定表名、列名、排序表达式或其他 SQL 结构。

## 清单

| ID | 类型 | 文件 | 保留原因与不变量 |
|---|---|---|---|
| `account-authz-version` | domain-service | `src/modules/account/authz_version.rs` | 用户授权版本的行锁、单调递增与 Outbox 写入必须处于同一事务 |
| `admin-grant-snapshot` | domain-service | `src/modules/admin/grants.rs` | Token 签发事务内读取平台管理员授权快照 |
| `admin-user-repository` | domain-repository | `src/modules/admin/user/repository.rs` | 跨表列表、管理员行锁和与审计同事务的受控更新 |
| `org-grant-snapshot` | domain-service | `src/modules/org/grants.rs` | Token 签发前按用户有效成员关系聚合授权，不接受请求租户 |
| `org-access-repository` | domain-repository | `src/modules/org/access/repository.rs` | pre-tenant 企业发现 Join，范围固定为已认证 actor |
| `org-member-guard` | domain-service | `src/modules/org/user/guard.rs` | 写操作前按可信租户与 actor 实时验证企业管理员身份 |
| `org-member-repository` | domain-repository | `src/modules/org/user/repository.rs` | 成员关系行锁、同租户校验、批量 writer 与审计原子性 |
| `work-task-repository` | domain-repository | `src/modules/work/task/repository.rs` | 任务关系同租户校验、递归防环和批量完成事务 |
| `audit-event-repository` | infrastructure-repository | `src/audit/repository.rs` | 只提供事务内追加，不提供 UPDATE/DELETE 或独立提交入口 |
| `audit-schema-validator` | schema-validator | `src/audit/schema.rs` | 启动期只读 `information_schema`，验证审计表不可变约束 |
| `authorization-outbox` | infrastructure-repository | `src/authorization/outbox.rs` | claim/lease/retry 状态机必须使用锁与条件更新 |

<!-- raw-sql-boundary: domain-service account-authz-version src/modules/account/authz_version.rs -->
<!-- raw-sql-boundary: domain-service admin-grant-snapshot src/modules/admin/grants.rs -->
<!-- raw-sql-boundary: domain-repository admin-user-repository src/modules/admin/user/repository.rs -->
<!-- raw-sql-boundary: domain-service org-grant-snapshot src/modules/org/grants.rs -->
<!-- raw-sql-boundary: domain-repository org-access-repository src/modules/org/access/repository.rs -->
<!-- raw-sql-boundary: domain-service org-member-guard src/modules/org/user/guard.rs -->
<!-- raw-sql-boundary: domain-repository org-member-repository src/modules/org/user/repository.rs -->
<!-- raw-sql-boundary: domain-repository work-task-repository src/modules/work/task/repository.rs -->
<!-- raw-sql-boundary: infrastructure-repository audit-event-repository src/audit/repository.rs -->
<!-- raw-sql-boundary: schema-validator audit-schema-validator src/audit/schema.rs -->
<!-- raw-sql-boundary: infrastructure-repository authorization-outbox src/authorization/outbox.rs -->

## 证据与变更流程

- 租户相关旁路仍须同时登记在
  [`tenant-data-paths.md`](tenant-data-paths.md)，并通过真实 MySQL 双租户负例；
- 新增或移动 SQL 边界时，先更新代码声明和本清单，再运行
  `python scripts/check_architecture.py`；
- SQLx 离线 metadata 只对 `query!` 系列编译期宏生效。当前生产查询使用运行时
  `query/query_as/query_scalar` API，因此本阶段先用静态字面量、绑定参数、真实迁移
  与集成测试约束；迁移到宏时必须提交 `.sqlx/` metadata，并在无数据库环境执行
  `cargo sqlx prepare --check`。

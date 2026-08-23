# 原始 SQL 边界清单

本文档枚举 `yang-system` 生产代码中允许直接调用 SQLx 的持久化逃生口。通用
CRUD、租户注入和字段权限仍由 YANG `TableQuery` 承担；领域仓储与服务中的简单
查询、行锁、JOIN、分页与条件更新已由 yang-db 查询构造器（含 `SqlExpr` 服务端
时间表达式）承载。只有递归 CTE、`JSON_TABLE` 批量锁、Outbox `SKIP LOCKED`
状态机、`information_schema` 内省和无 FROM 时钟采样等构造器无法表达的方言
特性保留原始 SQL。

## 机械约束

- 每个含生产 SQLx 查询的文件必须声明唯一的
  `//! raw-sql-boundary: <kind> <id>`，并与下表一一对应；
- `domain-repository` 只能位于 `src/addon/**/repository.rs`；
- `domain-service` 只用于明确的授权快照、租户 capability、实时 guard 或锁服务边界；
- 基础设施例外只允许审计 repository、授权 Outbox repository 与审计 schema
  validator；
- 查询文本必须是代码中的静态字符串字面量，所有数据参数通过 `bind` 传入；
- 请求参数不得决定表名、列名、排序表达式或其他 SQL 结构。

## 清单

| ID | 类型 | 文件 | 保留原因与不变量 |
|---|---|---|---|
| `work-task-repository` | domain-repository | `src/addon/work/task/repository.rs` | 任务关系递归 CTE 防环与 `JSON_TABLE` + `FOR UPDATE OF` 批量锁是构造器无法表达的方言特性；普通行锁读已改用构造器 |
| `audit-schema-validator` | schema-validator | `src/infrastructure/audit/schema.rs` | 启动期只读 `information_schema`，验证审计表不可变约束 |
| `authorization-outbox` | infrastructure-repository | `src/infrastructure/authorization/outbox.rs` | claim/lease/retry 状态机必须使用 `FOR UPDATE SKIP LOCKED`、内省与条件更新 |

<!-- raw-sql-boundary: domain-repository work-task-repository src/addon/work/task/repository.rs -->
<!-- raw-sql-boundary: schema-validator audit-schema-validator src/infrastructure/audit/schema.rs -->
<!-- raw-sql-boundary: infrastructure-repository authorization-outbox src/infrastructure/authorization/outbox.rs -->

## 证据与变更流程

- 租户相关旁路仍须同时登记在
  [`tenant-data-paths.md`](tenant-data-paths.md)，并通过真实 MySQL 双租户负例；
- 新增或移动 SQL 边界时，先更新代码声明和本清单，再运行
  `python scripts/check_architecture.py`；
- SQLx 离线 metadata 只对 `query!` 系列编译期宏生效。当前生产查询使用运行时
  `query/query_as/query_scalar` API，因此本阶段先用静态字面量、绑定参数、真实迁移
  与集成测试约束；迁移到宏时必须提交 `.sqlx/` metadata，并在无数据库环境执行
  `cargo sqlx prepare --check`。

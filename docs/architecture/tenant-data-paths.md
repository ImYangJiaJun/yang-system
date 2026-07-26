# 租户数据路径与旁路清单

状态：C2-final capability 模型
适用范围：`src/modules/org/` 的生产代码

## 1. 边界模型

租户隔离的事实链只有一条：

`Token 身份 → 请求租户声明 → 成员与组织状态校验 → TenantResolution → 互斥租户 capability → 数据访问作用域`

- 普通请求必须得到始终携带非可选 `TenantId` 的 `TenantContext`；缺少租户、成员关系无效或组织
  停用时失败关闭。
- `system` 角色只会得到绑定已认证 actor 的 `SystemTenantCapability`，与普通 `TenantContext`
  是互斥类型，不能伪装为无 ID 租户。
- 标准表访问必须从 `ActionContext::table_query()` / `Tables` 进入。`org_user.org_org`
  的 tenant key 会由基础库自动注入查询条件和写入值；系统身份调用标准入口同样失败关闭。
- 全租户 repository 必须先从当前请求取得 system capability，再显式传给
  `system_table_query(capability)` / `system_tables(capability)`；当前业务代码没有此类全租户
  repository。
- pre-tenant 发现、租户解析、授权快照等无法依赖已注入 `TenantContext` 的路径，必须在本清单
  中逐点声明自己的收敛键。

## 2. 表清单

| 表 | 分类 | 隔离键 | 约束与入口 |
|---|---|---|---|
| `org_org` | 租户根 | 主键 `id` 本身 | `org.org` 查询强制取得普通 capability 并限定 `id = tenant_id`；system 不会自动跨租户 |
| `org_user` | 租户数据 | `org_org`，声明为 `tenant_key(true)` | 标准 CRUD 由 `TableQuery` 自动限定/注入 `org_org`；唯一键为 `(org_org, user_user)` |

`users` 和 `admin_user` 是全局身份/平台域表，不属于组织租户表；它们不应因为 C2 的扫描规则
而伪装成租户表。

## 3. Action 清单

| Module.Action | 数据阶段 | 数据路径 | 当前隔离证据 |
|---|---|---|---|
| `org.tenant.list` | pre-tenant | raw SQL Join | 已认证 `user_id` + 有效成员状态 + 有效组织状态 |
| `org.tenant.create` | pre-tenant | 显式事务 + 无租户 TableQuery | 已认证 actor；同事务创建组织与 actor 的管理员成员关系 |
| `org.org.list` | tenant | `scoped_org_tables` | 强制普通租户 capability，并显式限定 `org_org.id = tenant_id` |
| `org.org.select` | tenant/relation | `scoped_org_tables` | 普通 capability 的分页、筛选和 selected 回填均重复施加同一 scope |
| `org.user.add` | tenant | 内置 CRUD `TableQuery` | tenant key 自动注入；实时企业管理员守卫 |
| `org.user.put` | tenant | 内置 CRUD `TableQuery` | tenant key 自动过滤；实时企业管理员守卫 |
| `org.user.del` | tenant | 内置 CRUD `TableQuery` | tenant key 自动过滤；实时企业管理员守卫 |
| `org.user.get` | tenant | 内置 CRUD `TableQuery` | tenant key 自动过滤 |
| `org.user.select` | tenant | 内置 CRUD `TableQuery` | tenant key 自动过滤 |
| `org.user.table` | tenant | 只读契约 | 仍经过认证与租户解析，不访问业务记录 |

`org.org` 和 `org.user` 的中间件顺序固定为 Token 认证后再解析租户；成员写操作在此后增加
`OrgAdminGuardMiddleware`。`org.tenant` 是刻意不运行租户解析器的 pre-tenant 模块，但仍强制认证。

## 4. 显式旁路

下面的 HTML 注释是架构检查器读取的机器清单。每一项必须与生产 Rust 源码中紧邻风险调用的
`// tenant-boundary: <kind> <id>` 一一对应；新增、删除或改名任一侧都会使门禁失败。

| ID | 类型 | 位置 | 收敛键/不变量 |
|---|---|---|---|
| `pre-tenant-table-database` | database | `org/access/repository.rs` | pre-tenant repository 获取无范围数据库能力的唯一 TableQuery 构造点 |
| `pre-tenant-table-query` | unscoped-query | `org/access/repository.rs` | 只供 pre-tenant repository 使用；具体方法必须按 actor 收敛或在创建事务内写入新租户 |
| `tenant-discovery-database` | database | `org/access/repository.rs` | 只供同函数内按 actor 收敛的租户发现 raw SQL 使用 |
| `tenant-discovery-page` | raw-sql | `org/access/repository.rs` | `membership.user_user = actor_id`，同时校验成员和组织为 active |
| `tenant-discovery-count` | raw-sql | `org/access/repository.rs` | 与分页查询使用相同 Join 和三个状态/身份谓词 |
| `tenant-onboarding-create` | transaction | `org/access/repository.rs` | 新组织尚无 tenant id；组织和创建者管理员成员关系同事务提交 |
| `tenant-membership-database` | database | `org/tenant.rs` | 只供租户解析器的成员资格查询使用 |
| `tenant-membership-lookup` | unscoped-query | `org/tenant.rs` | 同时限定请求 `org_id`、已认证 `user_id` 和 active 成员状态 |
| `tenant-organization-database` | database | `org/tenant.rs` | 只供租户解析器的组织状态查询使用 |
| `tenant-organization-status` | unscoped-query | `org/tenant.rs` | 同一次解析继续限定同一 `org_id` 且组织必须 active |
| `authorization-grant-database` | database | `org/grants.rs` | 只供 Token 授权快照的 actor 级查询使用 |
| `authorization-grant-snapshot` | raw-sql | `org/grants.rs` | 只按待签发 `user_id` 汇总“是否任一有效组织管理员”；租户写仍由实时管理员守卫二次校验 |
| `member-admin-database` | database | `org/user/guard.rs` | 只供当前租户管理员实时校验使用 |
| `member-admin-guard` | raw-sql | `org/user/guard.rs` | 同时限定可信 `tenant_id`、已认证 `user_id`、active 与 admin |
| `member-admin-system` | system-capability | `org/user/guard.rs` | system 管理操作必须消费当前请求 capability，并核对 capability actor 与已认证用户一致；不授予数据查询旁路 |

<!-- tenant-boundary: database pre-tenant-table-database -->
<!-- tenant-boundary: unscoped-query pre-tenant-table-query -->
<!-- tenant-boundary: database tenant-discovery-database -->
<!-- tenant-boundary: raw-sql tenant-discovery-page -->
<!-- tenant-boundary: raw-sql tenant-discovery-count -->
<!-- tenant-boundary: transaction tenant-onboarding-create -->
<!-- tenant-boundary: database tenant-membership-database -->
<!-- tenant-boundary: unscoped-query tenant-membership-lookup -->
<!-- tenant-boundary: database tenant-organization-database -->
<!-- tenant-boundary: unscoped-query tenant-organization-status -->
<!-- tenant-boundary: database authorization-grant-database -->
<!-- tenant-boundary: raw-sql authorization-grant-snapshot -->
<!-- tenant-boundary: database member-admin-database -->
<!-- tenant-boundary: raw-sql member-admin-guard -->
<!-- tenant-boundary: system-capability member-admin-system -->

## 5. Relation、批量、事务与后台任务

| 类别 | 当前生产路径 | 结论 |
|---|---|---|
| Relation | `org_user.org_org → org_org.id`，选择器为 `org.org.select` | 当前走租户化 `Tables`；C2-03 用双租户 selected/关联负例证明 |
| RelationLoader | 无 | 新增 `RelationLoader::new` 或 `.relations(...)` 必须声明 `relation` boundary |
| 批量写 | 无 | 新增 many/batch/bulk 写调用必须声明 `batch` boundary |
| 显式事务 | `org.tenant.create` 一处 | 已列为 `tenant-onboarding-create`；C2-03 验证回滚与跨租户负例 |
| 后台任务/导入 Job | 无 | 新增 Tokio spawn/JoinSet 后台入口必须声明 `background` boundary |

迁移 Job 只演进 schema，不读取或修改租户业务记录，因此不列为租户数据访问路径；一旦迁移
包含数据回填，就必须在迁移设计中单独声明全租户 capability、幂等键和分批策略。

## 6. 机械门禁

`python scripts/check_architecture.py` 对 `src/modules/org/**/*.rs` 的非测试生产段扫描：

- raw `sqlx::query*`；
- 直接取得 `Tools.mysql()` 数据库 capability；
- 绕过 `ActionContext::table_query()` 的空角色 `TableDefinition.query(...)`；
- 显式事务；
- RelationLoader/relation 扩展；
- many/batch/bulk 写；
- Tokio/JoinSet 后台任务；
- `system_tenant()` / `system_table_query(...)` / `system_tables(...)` 显式系统 capability。

风险调用前必须紧邻唯一 boundary 声明，且 ID/类型必须出现在本文件。文件级豁免、孤儿清单
和重复 ID 都会失败；生产代码中的 `TenantContext::system()`、`.is_system()` 与
`Option<TenantContext>` 也会直接失败。门禁只证明“旁路可枚举”，不替代 C2-02/C2-03 的
真实双租户行为证据。

## 7. 后续验证矩阵

- C2-02：A/B 双租户标准 CRUD、对象 ID 猜测、跨租户更新和删除。
- C2-03：Join、selected relation、批量、事务回滚与旁路调用。
- C2-04：把真实库测试入口、证据 ID 和本文映射加入架构门禁；任何删减都必须显式更新契约。
- C2-04+：每个真实失败旁路独立修复、独立提交。
- C2-final（完成）：repository 强制接收非可选 tenant capability；移除 `Option + bool`
  system 绕过表达；系统访问改为绑定 actor、显式消费且逐点登记的 capability。

## 8. 真实库证据契约

`python scripts/check_architecture.py` 同时校验以下机器条目与
`tests/tenant_isolation_integration.rs` 的 `tenant-evidence` marker 一一对应；两个矩阵函数必须保持为
`#[ignore]` 的 Tokio 测试，并由 `python scripts/run_ci.py integration` 在 MySQL 8 / Redis 7 上执行。

- CRUD 矩阵：可信 tenant key 注入、租户内正向 CRUD、列表范围、对象 ID 隐藏、跨租户写零影响、
  显式 tenant key 拒绝、租户迁移拒绝、上下文切换拒绝、失败操作零副作用。
- 旁路矩阵：按用户约束的 join、selected relation 的 `IN` 批量范围、批量新增/变更拒绝、
  组织与首成员事务回滚。

<!-- tenant-evidence: tenant_crud_and_object_ids_are_isolated_end_to_end crud-tenant-injection -->
<!-- tenant-evidence: tenant_crud_and_object_ids_are_isolated_end_to_end crud-own-scope -->
<!-- tenant-evidence: tenant_crud_and_object_ids_are_isolated_end_to_end crud-list-scope -->
<!-- tenant-evidence: tenant_crud_and_object_ids_are_isolated_end_to_end crud-object-id-hidden -->
<!-- tenant-evidence: tenant_crud_and_object_ids_are_isolated_end_to_end crud-cross-mutation-zero -->
<!-- tenant-evidence: tenant_crud_and_object_ids_are_isolated_end_to_end crud-explicit-tenant-rejected -->
<!-- tenant-evidence: tenant_crud_and_object_ids_are_isolated_end_to_end crud-tenant-move-rejected -->
<!-- tenant-evidence: tenant_crud_and_object_ids_are_isolated_end_to_end crud-context-switch-rejected -->
<!-- tenant-evidence: tenant_crud_and_object_ids_are_isolated_end_to_end crud-cross-effects-zero -->
<!-- tenant-evidence: tenant_join_relation_batch_and_transaction_bypasses_are_closed join-user-scope -->
<!-- tenant-evidence: tenant_join_relation_batch_and_transaction_bypasses_are_closed relation-selected-scope -->
<!-- tenant-evidence: tenant_join_relation_batch_and_transaction_bypasses_are_closed batch-add-rejected -->
<!-- tenant-evidence: tenant_join_relation_batch_and_transaction_bypasses_are_closed batch-mutation-rejected -->
<!-- tenant-evidence: tenant_join_relation_batch_and_transaction_bypasses_are_closed transaction-rollback -->

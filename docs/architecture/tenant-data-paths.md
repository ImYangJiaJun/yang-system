# 租户数据路径与旁路清单

状态：C2-final capability 模型
适用范围：`src/addon/org/` 与 `src/addon/work/` 的生产代码

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
| `org_user` | 租户数据 | `org_org`，声明为 `tenant_key(true)` | 标准读 Action 与显式事务 writer 均由 `TableQuery` 限定/注入 `org_org`；唯一键为 `(org_org, user_user)` |
| `work_project` | 个人租户数据 | `owner_user`，声明为 `tenant_key(true)` | 已认证用户 ID 被解析为唯一个人工作区；标准 CRUD 自动限定并注入 owner |
| `work_task` | 个人租户数据 | `owner_user`，声明为 `tenant_key(true)` | 标准 CRUD、关系选择器和任务 repository 均以可信个人工作区限定 owner |

`users` 和 `admin_user` 是全局身份/平台域表，不属于组织租户表；它们不应因为 C2 的扫描规则
而伪装成租户表。

## 3. Action 清单

| Module.Action | 数据阶段 | 数据路径 | 当前隔离证据 |
|---|---|---|---|
| `org.tenant.list` | pre-tenant | 查询构造器 Join | 已认证 `user_id` + 有效成员状态 + 有效组织状态 |
| `org.tenant.create` | pre-tenant | 显式事务 + 无租户 TableQuery | 已认证 actor；同事务创建组织与 actor 的管理员成员关系 |
| `org.org.list` | tenant | `scoped_org_tables` | 强制普通租户 capability，并显式限定 `org_org.id = tenant_id` |
| `org.org.select` | tenant/relation | `scoped_org_tables` | 普通 capability 的分页、筛选和 selected 回填均重复施加同一 scope |
| `org.user.add` | tenant | 显式事务 writer + `TableQuery` | tenant key 自动注入；锁定组织和用户；成员事实与授权版本原子提交 |
| `org.user.put` | tenant | 显式事务 writer + `TableQuery` | tenant key 自动过滤；锁定成员和稳定排序用户集合；幂等授权写不递增 |
| `org.user.del` | tenant | 显式事务 writer + `TableQuery` | tenant key 自动过滤；成员删除与当前绑定用户版本原子提交 |
| `org.user.get` | tenant | 内置 CRUD `TableQuery` | tenant key 自动过滤 |
| `org.user.select` | tenant | 内置 CRUD `TableQuery` | tenant key 自动过滤 |
| `org.user.table` | tenant | 只读契约 | 仍经过认证与租户解析，不访问业务记录 |
| `work.project.*` | personal-tenant | 内置 CRUD / relation options | Token actor 被解析为 `owner_user`，客户端伪造其他 tenant 失败关闭 |
| `work.task.get/select/table/del` | personal-tenant | 内置 CRUD `TableQuery` | `owner_user` tenant key 自动过滤；跨用户对象 ID 不可见 |
| `work.task.add/put/options` | personal-tenant/relation | `TableQuery` + 构造器行锁 + 递归 CTE 校验 | 项目和父任务重复限定可信 owner，拒绝跨项目父任务与关系环 |
| `work.task.complete` | personal-tenant/batch | 显式事务 + `TableQuery` | 最多 100 个唯一 ID；事务内 tenant scope 锁定并全有或全无更新 |

`org.org` 和 `org.user` 的中间件顺序固定为 Token 认证后再解析租户；成员写操作在此后增加
`OrgAdminGuardMiddleware`。租户解析用单次 JOIN 同时校验成员与企业状态，并把绑定
`actor + tenant + admin` 的请求 capability 交给 guard；它只承担事务前快速拒绝，成员
writer 仍在事务内锁定并复核管理员事实。`org.tenant` 是刻意不运行租户解析器的
pre-tenant 模块，但仍强制认证。

本次收敛有可复现的本地证据，而不是只按查询条数推断。10,000 名成员、10 路并发、
1,000 次请求的真实 MySQL 对比中，旧三查询路径 p95 为 10,951 μs、总耗时 863 ms；
单 JOIN capability p95 为 3,830 μs、总耗时 275 ms，每请求 SQL 从 3 条降为 1 条。
基准与非成员、disabled 企业、非管理员和 actor/tenant 绑定负例见
`.ecc/benchmarks/tenant-capability.json` 与 `tests/tenant_query_benchmark.rs`。这是本地改造
依据，不是生产容量声明；生产仍须观察 endpoint p95、连接池等待和数据库 QPS。

## 4. 显式旁路

下面的 HTML 注释是架构检查器读取的机器清单。每一项必须与生产 Rust 源码中紧邻风险调用的
`// tenant-boundary: <kind> <id>` 一一对应；新增、删除或改名任一侧都会使门禁失败。

| ID | 类型 | 位置 | 收敛键/不变量 |
|---|---|---|---|
| `pre-tenant-table-database` | database | `org/access/repository.rs` | pre-tenant repository 获取无范围数据库能力的唯一 TableQuery 构造点 |
| `pre-tenant-table-query` | unscoped-query | `org/access/repository.rs` | 只供 pre-tenant repository 使用；具体方法必须按 actor 收敛或在创建事务内写入新租户 |
| `tenant-discovery-database` | database | `org/access/repository.rs` | 只供同函数内按 actor 收敛的租户发现查询构造使用 |
| `org-onboarding-database` | database | `org/access/repository.rs` | 只供 onboarding 事务内用户授权行锁的查询构造；不读取租户数据 |
| `tenant-onboarding-create` | transaction | `org/access/repository.rs` | 新组织尚无 tenant id；组织和创建者管理员成员关系同事务提交 |
| `tenant-membership-capability-database` | database | `org/tenant.rs` | 只供单次租户 capability JOIN 查询使用；同时限定请求 `org_id`、已认证 `user_id`、active 成员与 active 企业，并投影当前 admin 事实 |
| `member-admin-system` | system-capability | `org/user/guard.rs` | system 管理操作必须消费当前请求 capability，并核对 capability actor 与已认证用户一致；不授予数据查询旁路 |
| `org-member-add-database` | database | `org/user/repository.rs` | 只供 add writer 开启显式事务并构造事务内行锁查询；普通租户仍由 `table_query()` 注入 tenant key |
| `org-member-put-database` | database | `org/user/repository.rs` | 只供 put writer 开启显式事务并构造事务内行锁查询；成员锁和后续更新重复限定同一 capability |
| `org-member-delete-database` | database | `org/user/repository.rs` | 只供 delete writer 开启显式事务并构造事务内行锁查询；成员锁和删除重复限定同一 capability |
| `org-member-resource-resolve-database` | database | `org/user/repository.rs` | system capability 的 put/delete 在事务外只解析目标组织；事务内仍会重新按组织锁定目标成员 |
| `org-member-resource-resolve-system` | system-capability | `org/user/repository.rs` | system 资源解析必须核对 capability actor 与已认证用户一致 |
| `org-member-linearization-system` | system-capability | `org/user/repository.rs` | system 写事务在最终线性化点再次消费并核对 actor-bound capability |
| `org-member-add-system` | system-capability | `org/user/repository.rs` | system add 必须显式提供目标组织，且组织存在并 active |
| `org-member-lock-system` | system-capability | `org/user/repository.rs` | system put/delete 在无普通租户 capability 时必须显式消费 system capability |
| `work-task-workspace-lock-database` | database | `work/task/repository.rs` | 任务关系写入先按可信 owner 锁定个人工作区，串行化同一用户的并发关系变更 |
| `work-task-current-links-database` | database | `work/task/repository.rs` | 更新前按可信 owner 与任务 ID 锁定当前项目/父任务关系 |
| `work-task-links-validation-database` | database | `work/task/repository.rs` | 锁定目标项目与父任务，并要求 owner 等于可信个人工作区 |
| `work-task-cycle-check` | raw-sql | `work/task/repository.rs` | 递归链每层重复限定 owner，深度上限 100，并拒绝形成关系环 |
| `work-task-add-transaction` | transaction | `work/task/actions/add.rs` | 工作区锁、项目/父任务校验和新增任务在同一事务提交 |
| `work-task-put-transaction` | transaction | `work/task/actions/put.rs` | 工作区锁、当前关系锁、项目/父任务校验和更新任务在同一事务提交 |
| `work-task-complete-lock` | raw-sql | `work/task/repository.rs` | JSON_TABLE 只承载绑定 ID 值；按可信 owner 全量锁行，缺失或跨工作区时批次失败 |
| `work-task-complete-transaction` | transaction | `work/task/actions/complete.rs` | 最多 100 个唯一 ID 在 tenant-scoped 事务内全量可见后才原子更新 |

<!-- tenant-boundary: database pre-tenant-table-database -->
<!-- tenant-boundary: unscoped-query pre-tenant-table-query -->
<!-- tenant-boundary: database tenant-discovery-database -->
<!-- tenant-boundary: database org-onboarding-database -->
<!-- tenant-boundary: transaction tenant-onboarding-create -->
<!-- tenant-boundary: database tenant-membership-capability-database -->
<!-- tenant-boundary: system-capability member-admin-system -->
<!-- tenant-boundary: database org-member-add-database -->
<!-- tenant-boundary: database org-member-put-database -->
<!-- tenant-boundary: database org-member-delete-database -->
<!-- tenant-boundary: database org-member-resource-resolve-database -->
<!-- tenant-boundary: system-capability org-member-resource-resolve-system -->
<!-- tenant-boundary: system-capability org-member-linearization-system -->
<!-- tenant-boundary: system-capability org-member-add-system -->
<!-- tenant-boundary: system-capability org-member-lock-system -->
<!-- tenant-boundary: database work-task-workspace-lock-database -->
<!-- tenant-boundary: database work-task-current-links-database -->
<!-- tenant-boundary: database work-task-links-validation-database -->
<!-- tenant-boundary: raw-sql work-task-cycle-check -->
<!-- tenant-boundary: transaction work-task-add-transaction -->
<!-- tenant-boundary: transaction work-task-put-transaction -->
<!-- tenant-boundary: raw-sql work-task-complete-lock -->
<!-- tenant-boundary: transaction work-task-complete-transaction -->

## 5. Relation、批量、事务与后台任务

| 类别 | 当前生产路径 | 结论 |
|---|---|---|
| Relation | `org_user.org_org → org_org.id`，选择器为 `org.org.select` | 当前走租户化 `Tables`；C2-03 用双租户 selected/关联负例证明 |
| RelationLoader | 无 | 新增 `RelationLoader::new` 或 `.relations(...)` 必须声明 `relation` boundary |
| 批量写 | `work.task.complete` | 1..=100 个唯一 ID；tenant-scoped 事务内先验证全量可见再原子更新 |
| 显式事务 | `org.tenant.create` 与 `org.user.add/put/del` | onboarding 和成员授权事实 writer 均显式提交/回滚；C2/C3 真实库矩阵验证隔离、回滚与版本原子性 |
| 后台任务/导入 Job | 无 | 新增 Tokio spawn/JoinSet 后台入口必须声明 `background` boundary |

迁移 Job 只演进 schema，不读取或修改租户业务记录，因此不列为租户数据访问路径；一旦迁移
包含数据回填，就必须在迁移设计中单独声明全租户 capability、幂等键和分批策略。

## 6. 机械门禁

`python scripts/check_architecture.py` 对 `src/addon/org/**/*.rs` 与
`src/addon/work/**/*.rs` 的非测试生产段扫描：

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

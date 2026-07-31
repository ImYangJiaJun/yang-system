# yang-system 数据库迁移

生产数据库只通过独立 `yang-migrate` 作业演进。应用进程保持
`app.environment = "production"`、`schema.mode = "validate"`，不在副本启动期执行
DDL。

## 命令与发布门禁

```powershell
# 只读检查：不创建 _migrations，不执行 SQL
cargo run --locked --bin yang-migrate -- plan --config config.toml

# 数据库级串行迁移；成功前必须完成同一版本应用定义的 Schema 校验
cargo run --locked --bin yang-migrate -- apply --config config.toml

# 迁移作业成功后才滚动应用；启动期再次 validate，随后才可能 ready
cargo run --locked --bin yang-system
```

`apply` 只读取 `[mysql]` 与 `[security]`，不会连接 Redis、创建 HTTP 监听或使用
Token 密钥。MySQL `max_connections` 至少为 2：一个连接持有数据库级 advisory
lock，另一个执行迁移与校验。

## 不可变清单

清单位于 `src/migrations.rs`，SQL 位于 `migrations/`。每个版本必须同时声明：

- 严格递增且永久唯一的 `version`；
- 一条可独立审计的前向 SQL；SQL 必须自身可重入，或声明能完整代表其效果的精确完成探针；
- 变更目的 `description`；
- 执行前提 `prerequisite`；
- 中断或失败后的 `recovery`。

`_migrations` 记录 `module + version + checksum + status`。已发布 SQL 不能修改；
相同版本内容变化会成为 `ChecksumMismatch` 并阻断发布。新增变更只能追加更高版本，
不能重排、删除或覆盖旧文件。

前 4 个基线版本使用 `CREATE TABLE IF NOT EXISTS`，用于接管此前由本地
`schema.apply` 创建的同构数据库。建表 no-op 后仍会执行完整 Schema 校验；已有表若
结构不一致，发布会在 validate 阶段失败，不会进入 ready。

`20260726_0005_add_user_authz_version` 是首个非幂等原子 DDL。它使用列完成探针
精确匹配 `COLUMN_TYPE`、可空性与默认值；只有完整结构一致时，执行器才会在崩溃
恢复中把遗留 `running` 记录改为 `applied`，不会凭“列存在”跳过错误结构。

`20260726_0006_create_authorization_outbox` 建立应用内部授权传播表。它不属于业务
Module，也不进入对外 Catalog；`(user_id, authz_version)` 唯一键负责事件幂等，
`(state, available_at, id)` 索引支持多 Worker 以稳定顺序批量 claim。该表必须在
启用授权 writer 的应用版本滚动前完成迁移。

`20260726_0007_create_audit_event` 建立应用内部高权限审计事实表。它不进入对外
Catalog；启动和迁移作业都会精确校验列、CHECK、索引、引擎与 collation。运行账号
只能追加/读取，UPDATE/DELETE 由独立保留账号控制，具体权限和保留流程见
[`AUDIT.md`](AUDIT.md)。

`20260731_0008_create_work_project` 与 `20260731_0009_create_work_task` 建立首个
真实个人任务规划 Addon。两张表都以已认证 `users.id` 作为不可伪造的租户键；
项目名称唯一性、任务分页组合索引和 owner/project/parent 复合外键共同约束跨工作区
关系、跨项目父子关系与删除顺序。发布时必须按版本顺序先项目、后任务。

`20260731_0010_add_user_credential_version` 增加独立的凭据/全量会话版本，使用列
完成探针精确匹配 `BIGINT NOT NULL DEFAULT 0`。该版本必须按三阶段发布：先执行迁移；
再让全部实例以 `security.issue_refresh_credential_version = false` 部署兼容读取（旧
Refresh 缺字段按 0 比较）；确认没有旧实例后改为 `true`，开始只在 Refresh Token
签发 `credential_version`，并注册 `account.user.change_password`。Access Token 继续
只使用 `authz_version`。开关关闭时 Registry/Catalog 不暴露会递增凭据版本的改密
Action，防止版本大于 0 的用户拿到无法继续刷新的兼容期 Token。

`20260731_0011_create_password_reset_token` 建立密码重置凭证表。原始凭证不落库，
只保存 SHA-256 摘要与 16 字符指纹；目标用户、发起管理员、到期、消费和失效时间均
受数据库约束。一个用户创建新凭证会使旧的未消费凭证失效，成功消费时也会失效该
用户的其他未消费凭证。该迁移可先发布，但创建与消费 Action 和改密一样只在
`security.issue_refresh_credential_version = true` 时注册。

`20260731_0012_add_users_status_check` 把 `users.status` 的 `active/disabled` 领域集合
固化为强制 CHECK。执行前会拒绝未经验证的 MySQL 实现/版本和全部越界状态；精确完成
探针同时核对约束名、表达式与 `ENFORCED`，不能用同名异义约束恢复中断记录。

`20260731_0013` 至 `0015` 为三个授权关系增加单列外键：

- `admin_user.user_user -> users.id`；
- `org_user.user_user -> users.id`；
- `org_user.org_org -> org_org.id`。

三个外键都使用 `ON UPDATE RESTRICT ON DELETE RESTRICT`。授权关系和审计事实不能因
父记录删除而静默级联消失；业务若需要删除父记录，必须先通过显式生命周期 writer 处理
关系、版本与审计。`apply` 在 DDL 前逐项核对父子表均为 InnoDB、列类型完全兼容并统计
孤儿行；任一计数非零即在建约束前失败。完成探针精确核对约束名、本地列、目标列以及
双 `RESTRICT`，可恢复“DDL 已提交、迁移记录仍为 running”的 MySQL 中断窗口，同时拒绝
同名异义外键。

`20260731_0016_add_admin_bootstrap_key_check` 在现有 `bootstrap_key NULL UNIQUE`
并发守卫之上增加强制 CHECK，只允许 `NULL` 或保留值 `initial-admin`。`NULL` 仍允许普通
平台授权记录共存，唯一非空值继续保证 bootstrap 最多成功一次；任意其他非空占位值会
在数据库边界失败。`apply` 会先核对 MySQL 版本与现有非空值分组，精确完成探针同时
匹配约束名、表达式和 `ENFORCED`。该迁移不会把 bootstrap 状态拆到新表，也不改变现有
API；DDL 锁预算仍须在生产等量 staging 评估。

```powershell
$env:YANG_SYSTEM_TEST_DATABASE_URL='mysql://<staging-test-database>'
$env:YANG_SYSTEM_BOOTSTRAP_DDL_SCALE_ROWS='<admin_user 生产等量行数>'
$env:YANG_SYSTEM_BOOTSTRAP_DDL_BUDGET_MS='<允许的DDL毫秒预算>'
cargo test -p yang-system --test migration_job_integration --locked `
  bootstrap_key_check_ddl_rehearsal_obeys_configured_budget -- `
  --ignored --nocapture --test-threads=1
```

发布前必须在生产等量 staging 运行授权外键 DDL 演练，并用发布窗口设置预算：

```powershell
$env:YANG_SYSTEM_TEST_DATABASE_URL='mysql://<staging-test-database>'
$env:YANG_SYSTEM_AUTHZ_FK_DDL_SCALE_ROWS='<每张授权关系的生产等量行数>'
$env:YANG_SYSTEM_AUTHZ_FK_DDL_BUDGET_MS='<允许的DDL毫秒预算>'
cargo test -p yang-system --test migration_job_integration --locked `
  authorization_foreign_key_ddl_rehearsal_obeys_configured_budget -- `
  --ignored --nocapture --test-threads=1
```

演练会确认三个外键复用既有索引并输出实际耗时。该耗时不能代替并发流量下的元数据锁
观测；正式发布还必须在 staging 同时施加代表性读写负载，观察 metadata lock wait、事务
超时和复制延迟。超过预算时不得直接上线，应调整发布窗口或采用在线 Schema 变更。

## 中断、并发与恢复

- 同一数据库的显式迁移作业由 MySQL advisory lock 串行化；后到作业等待锁并重新
  读取执行记录。
- SQL 失败会删除本次 `running` 预留。进程崩溃会自动释放连接级锁；下一作业取得锁
  后只恢复 checksum 一致的遗留预留：普通迁移重跑可重入 SQL，带探针迁移先精确
  验证完成状态，已生效则只修复记录，否则执行原 SQL。
- checksum 不一致、未知状态或 Schema 差异均 fail-closed。先保留现场并诊断，再追加
  修复版本；禁止手工把异常记录直接改成 `applied`。
- MySQL DDL 会隐式提交，因此一个版本只放一条语句；非幂等原子 DDL 必须声明完整
  完成探针。多阶段变更拆成多个版本，并使用 expand → backfill → switch → contract。

## 新增迁移路径

1. 先以兼容性 Schema 扩展为目标，新增 `migrations/<version>_<name>.sql`。
2. 在 `MIGRATIONS` 尾部追加同版本元数据；不要编辑旧项。
3. 为新行为增加真实 MySQL 集成断言，覆盖首次执行、重复执行和失败恢复。
4. 运行 `python scripts/run_ci.py full` 与 `python scripts/run_ci.py integration`。
5. 发布时先审阅 `plan`，再运行 `apply`；只有退出码 0 才能滚动应用副本。

回退采用前向恢复：应用版本需要后退时，数据库仍保持向前兼容；破坏性 contract
版本必须在旧应用完全退出并经过观测窗口后单独发布。

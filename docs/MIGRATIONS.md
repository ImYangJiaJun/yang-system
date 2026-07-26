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
- 一条可独立审计、可重入的前向 SQL；
- 变更目的 `description`；
- 执行前提 `prerequisite`；
- 中断或失败后的 `recovery`。

`_migrations` 记录 `module + version + checksum + status`。已发布 SQL 不能修改；
相同版本内容变化会成为 `ChecksumMismatch` 并阻断发布。新增变更只能追加更高版本，
不能重排、删除或覆盖旧文件。

当前 4 个基线版本使用 `CREATE TABLE IF NOT EXISTS`，用于接管此前由本地
`schema.apply` 创建的同构数据库。建表 no-op 后仍会执行完整 Schema 校验；已有表若
结构不一致，发布会在 validate 阶段失败，不会进入 ready。

## 中断、并发与恢复

- 同一数据库的显式迁移作业由 MySQL advisory lock 串行化；后到作业等待锁并重新
  读取执行记录。
- SQL 失败会删除本次 `running` 预留。进程崩溃会自动释放连接级锁；下一作业取得锁
  后只恢复 checksum 一致的遗留预留，并重跑可重入 SQL。
- checksum 不一致、未知状态或 Schema 差异均 fail-closed。先保留现场并诊断，再追加
  修复版本；禁止手工把异常记录直接改成 `applied`。
- MySQL DDL 会隐式提交，因此一个版本只放一条可重入语句。多阶段变更拆成多个版本，
  并使用 expand → backfill → switch → contract。

## 新增迁移路径

1. 先以兼容性 Schema 扩展为目标，新增 `migrations/<version>_<name>.sql`。
2. 在 `MIGRATIONS` 尾部追加同版本元数据；不要编辑旧项。
3. 为新行为增加真实 MySQL 集成断言，覆盖首次执行、重复执行和失败恢复。
4. 运行 `python scripts/run_ci.py full` 与 `python scripts/run_ci.py integration`。
5. 发布时先审阅 `plan`，再运行 `apply`；只有退出码 0 才能滚动应用副本。

回退采用前向恢复：应用版本需要后退时，数据库仍保持向前兼容；破坏性 contract
版本必须在旧应用完全退出并经过观测窗口后单独发布。

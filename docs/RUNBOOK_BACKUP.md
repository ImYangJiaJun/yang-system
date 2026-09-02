# 备份与恢复 Runbook

面向值班运维。本文约定 yang-system 的数据备份范围、备份方式、恢复演练步骤与
RPO/RTO 目标建议。所有命令示例基于仓库根目录 `compose.yaml` 的本地部署形态
（服务名 `mysql`/`redis`），生产环境请替换为对应实例的访问方式。

## 数据事实源判断

| 存储 | 角色 | 是否需要备份 |
|---|---|---|
| MySQL `yang_system` | **最终事实源**：业务表、账号凭据（argon2 哈希）、授权直授（`authz_grant`）、授权版本、授权 Outbox、append-only 审计（`audit_event`） | **必须备份** |
| Redis | 纯加速层：授权版本短 TTL 缓存、认证限流计数、Step-up proof 单次消费、邮箱验证码计数 | **不备份** |

Redis 数据全部可重建或自然过期，丢失的影响上限为：

- 授权版本缓存失效：重启后从 MySQL 重放 Outbox 重建，期间按 MySQL 事实源校验，不会放大权限；
- 限流/验证码计数清零：限流窗口重新计算，属于可用性扰动而非安全缺口；
- 进行中的 Step-up challenge/proof 失效：用户重新发起重认证即可。

因此**不要为 Redis 投入备份预算**；本地 `compose.yaml` 中 Redis 的
`--appendonly yes` 只用于减少重启抖动，不是持久化承诺。

## RPO/RTO 目标建议

| 指标 | 建议值 | 依据 |
|---|---|---|
| RPO | ≤ 5 分钟 | 全量备份 + binlog 点位恢复；审计与授权 Outbox 不允许长时间丢失 |
| RTO | ≤ 1 小时 | 单实例全量 + binlog 重放可在一小时内完成；每季度演练验证 |

如业务规模增长，先缩短的是演练间隔与 binlog 保留核对，而不是引入更复杂的
拓扑。

## 备份方式

### 每日全量逻辑备份（mysqldump）

```bash
# 在仓库根目录执行；备份文件带 binlog 点位（--source-data=2 以注释形式写入，
# 不阻塞从库场景）。密码经环境变量传入，不出现在命令行参数里。
export MYSQL_PWD='<root 或备份专用账号密码>'
docker compose exec -T mysql mysqldump \
  --user=root \
  --single-transaction \
  --routines --triggers \
  --source-data=2 \
  --databases yang_system \
  | gzip > "backup/yang_system_$(date +%Y%m%d_%H%M%S).sql.gz"
```

- `--single-transaction`：InnoDB 一致性快照，不锁表；
- `--source-data=2`：在 dump 头部记录 `CHANGE MASTER TO MASTER_LOG_FILE=..., MASTER_LOG_POS=...`（注释形式），作为 binlog 增量恢复的起点；
- 生产环境应使用只授予 `SELECT, LOCK TABLES, SHOW VIEW, TRIGGER, RELOAD, REPLICATION CLIENT` 的备份专用账号，不使用 root。

### binlog 增量（点位恢复）

全量备份之间的新增数据依赖 MySQL 二进制日志：

- MySQL 侧确认开启：`log_bin=ON`、`binlog_format=ROW`、`sync_binlog=1`；
- 建议 `binlog_expire_logs_seconds` ≥ 7 天（604800），覆盖"每日全量 + 最长一周回追"；
- binlog 文件随数据卷持久化，备份脚本应定期把 binlog 归档到与全量备份相同的异地位置：

```bash
docker compose exec -T mysql sh -c \
  'mysqlbinlog --read-from-remote-server --raw --stop-never-slave-server-id=9999 \
   --user=root --password="$MYSQL_PWD" --host=127.0.0.1 mysql-bin.0000XX' 
```

（示例仅示意归档形态；也可用对象存储的 MySQL 托管备份能力替代手工 binlog 归档。）

### 备份文件的安全要求

- 备份含密码哈希、邮箱等敏感数据，必须**加密存储**并限制访问；不要提交到 Git；
- 全量与 binlog 归档存放于独立于数据库主机的位置（异机/异可用区）；
- 建议保留期：每日全量保留 30 天，月度全量保留 12 个月；到期自动清理。

## 恢复演练步骤（每季度一次）

演练目标：证明备份可用、记录实际 RTO。在**隔离环境**执行，严禁直接对生产实例操作。

1. **准备隔离实例**：启动一台干净的 MySQL 8.0（可用独立 compose 项目或临时容器），版本不低于备份来源。
2. **恢复最近全量**：

   ```bash
   gunzip -c backup/yang_system_YYYYMMDD_HHMMSS.sql.gz | mysql --user=root --password='<密码>' --host=<隔离实例>
   ```

   从 dump 头部注释读出 binlog 点位（`MASTER_LOG_FILE` / `MASTER_LOG_POS`）。
3. **应用 binlog 到目标点位**：

   ```bash
   mysqlbinlog --start-position=<POS> mysql-bin.0000XX | mysql --user=root ... --host=<隔离实例>
   ```

   按时间恢复时使用 `--start-datetime` / `--stop-datetime` 替代点位。
4. **一致性校验**（至少执行）：
   - 各业务表行数与源端快照一致；
   - 授权 Outbox 最大序号、`authz_grant` 行数与源端一致；
   - `audit_event` 计数单调无缺口（append-only，不允许空洞）。
5. **启动应用指向恢复库**：用临时配置把 `mysql.url` 指向隔离实例（Redis 可用全新空实例），确认管理面 `/health/ready` 就绪、Schema 预检通过、抽查登录与只读接口。
6. **记录演练**：恢复开始/结束时间、实际 RTO、数据校验结论，归档到运维记录。演练失败按事故流程处理并修复备份链路。

## 销毁性操作警告

- `docker compose down -v` 会**永久删除** `mysql-data` 与 `redis-data` 命名卷，
  等同于删除本地全部业务数据与审计记录，不可恢复。执行前确认已备份或确实要清空环境；
  日常停止只使用 `docker compose down`（不带 `-v`）。
- 删除卷后即使重新 `up`，`docker/mysql/init/` 也只会重建空库，历史数据不会回来。

## 相关文档

- `docs/CONFIGURATION.md`：凭据注入与轮换流程；
- `docs/OBSERVABILITY.md`：恢复后验证用 readiness 与指标契约；
- `docs/AUDIT.md`：`audit_event` 作为追责事实源的语义。

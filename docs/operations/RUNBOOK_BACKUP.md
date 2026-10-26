# 备份与恢复 Runbook

MySQL 是最终事实源。备份成功必须有异机副本、可验证摘要和隔离恢复记录；仅有 gzip 文件或任务退出 0 不足以证明恢复能力。下面的脚本是仓内交付资产，生产任务、凭据、异机存储和最近可恢复时间须逐环境核实。

## 恢复资产

| 资产 | 恢复要求 |
|---|---|
| MySQL 全量与持续 binlog | 业务数据、头像、账号、授权、Outbox、审计、飞书配置/任务/凭据；记录快照时点与 binlog 起点 |
| 两个应用镜像与源码身份 | 后端、前端镜像 ID/digest，两仓 commit、dirty 事实、测试记录；保留可重新取得的镜像归档 |
| 版本匹配配置与密钥 | `.releases/` 中私有配置快照、有效环境/secret 来源、Token keyring、TOTP/飞书加密密钥；加密备份，恢复私钥独立托管 |
| 网关与基础设施 | 网关配置、证书恢复/续期权限、DNS/端口/可信代理关系、MySQL/Redis 版本及实际配置 |
| Redis | 不用旧缓存覆盖新事实；清空会重置限流、验证码和消费记录等安全状态，必须单独验证恢复后的认证/重放边界 |

Redis 不是纯性能缓存。丢失消费记录可能影响 Step-up 单次使用和并发认证语义；恢复旧 MySQL 也会回退撤销、授权版本与凭据事实。开放流量前按安全运行手册处置旧 Token/会话、keyring/epoch 和缓存失效，不能承诺“重新登录即可”。旧快照中的 OAuth refresh token 可能已失效，须只读检查授权状态并重新授权，不盲目重复外部操作。

## 全量备份入口

`deploy/backup-mysql.sh` 使用 `set -euo pipefail`，显式把宿主 `MYSQL_PWD` 传入 `docker exec -e MYSQL_PWD`；先检查 SQL 表定义与 dump 完成标记，再用恢复方公钥 CMS 加密，异机校验密文 SHA-256 后才原子更新本地 `last-success`。失败清理私有暂存目录，不更新成功标记。该标记证明流水线成功，**不代替解密与数据库恢复验证**。

依赖：Bash、Docker CLI、gzip、OpenSSL、SSH/SCP、sha256sum。配置 SSH 已知主机和非交互专用备份账号，不禁用主机身份检查。备份主机只保存恢复方公钥证书，私钥由独立密钥托管持有；公钥指纹与密文摘要须存入受控运维记录，不能信任备份存储中可被一起替换的摘要。

```bash
# 密码由 secret 环境注入，禁止在历史/脚本中填真实值；不要 set -x。
export MYSQL_CONTAINER=yang-mysql MYSQL_DATABASE=yang_system BACKUP_USER=backup
export BACKUP_DIR=/srv/yang-backup
export BACKUP_RECIPIENT_CERT=/etc/yang-backup/recovery-public.pem
export BACKUP_SSH_TARGET=backup@backup-host
export BACKUP_REMOTE_DIR=/srv/yang-system
bash deploy/backup-mysql.sh
```

脚本不会创建备份账号或决定保留期。权限按当前 MySQL 版本和 `--routines --triggers --source-data=2` 实际要求授予专用账号，禁止把“权限不足”当空备份成功。`--single-transaction` 提供 InnoDB 一致性快照；配合 source-data 可能短暂全局读锁，备份期间应避开 DDL。非事务表需要单独一致性方案。

每天全量的 RPO 上界接近一天；只有持续归档成功并验证可恢复的 binlog 才能争取分钟级 RPO。RTO 必须以恢复演练实测，不从脚本推导。保留期沿用已批准策略，本次不调整。

## 持续 binlog

核对 `log_bin=ON`、`binlog_format=ROW`、`sync_binlog=1`、过期策略及归档落后量。以下由专门归档主机运行，`MYSQL_PWD` 来自 secret 环境；输出落到归档主机明确目录，必须另接加密、异机持久化、断线重连与告警，不能把此命令本身当完整归档服务。

```bash
mysqlbinlog --read-from-remote-server --raw --stop-never \
  --stop-never-slave-server-id=9999 --user=binlog_backup \
  --host=<受控MySQL入口> --result-file=/srv/private-binlog/ mysql-bin.0000XX
```

起始文件取全量记录，server-id 保证唯一；首次归档后故障重连/轮转/缺段恢复必须演练。禁用将密码放入 `--password=...` 的命令行写法。

## 隔离恢复演练

1. 在独立 Docker 网络或隔离宿主建立干净 MySQL，使用与备份兼容的已验证版本。恢复 SQL 包含 CREATE/DROP 等命令，只能送到专用 scratch 实例。
2. **启动应用前**先阻断外网和生产数据库/Redis/飞书/邮件网络，替换生产凭据。可以先运行 `yang-system --smoke-check`：仅只读 schema/旧数据预检和依赖检查，不启动业务 worker、HTTP 或 telemetry exporter。它不是可供业务验收的只读 Web 服务；正常模式仍会启动全部符合配置条件的 worker，不能拿“测试配置”当副作用隔离。
3. 从独立运维记录取得摘要并校验密文，再用恢复私钥解密到权限 0700 的临时目录。全过程开启 pipefail；失败不得继续下一步。

   ```bash
   set -euo pipefail
   umask 077
   openssl cms -decrypt -binary -inform DER -in backup.cms \
     -recip recovery-public.pem -inkey /secure/recovery-private.pem \
     -out /scratch/private/dump.sql.gz
   gzip -t /scratch/private/dump.sql.gz
   # SCRATCH_MYSQL_PWD 由隔离环境 secret 注入；容器名必须经过人工核对。
   gzip -dc /scratch/private/dump.sql.gz | \
     MYSQL_PWD="$SCRATCH_MYSQL_PWD" docker exec -i -e MYSQL_PWD scratch-mysql mysql --user=root
   ```

4. 根据全量头部的 binlog 文件/点位，校验连续性后重放到批准的目标时点（整条 `mysqlbinlog | mysql` 同样使用 pipefail）。**不要把快照覆盖当前生产库当无损回滚**；发布后新写会丢失。
5. 按备份时点记录抽样业务行、表数、授权版本、Outbox、审计计数与关键状态。自增 ID 可能合法存在间隙，不能把“无空洞”当审计一致性条件。飞书实例对账先 dry-run，严禁恢复环境创建审批或发通知。
6. 准备版本匹配的镜像、配置、密钥和隔离 Redis；确认锁内 schema 策略与 worker 禁用已生效后，再验证只读接口、ready、目标库身份及旧会话拒绝。只有这一步可以说明应用恢复，而数据库导入成功只能说明 SQL 可导入。
7. 记录最近可恢复时间、实际 RPO/RTO、加密解密结果、来源镜像/配置身份、断网证据、数据抽样和失败项；归档演练日志，安全清理明文临时文件。

## 销毁操作与边界

`docker compose down -v` 会永久删除数据卷；`deploy.ps1 -ResetDb` 会 DROP 目标数据库并清 Redis，均不创建自动备份。停止失败或受管写者状态未知时脚本拒绝清库，但它无法证明没有其他系统/容器写者；维护窗口还必须停全部外部写者并限制入口。重置、生产恢复和清空操作需要独立授权，严禁用生产 ResetDb 作为测试手段。

相关契约：`docs/contracts/CONFIGURATION.md`、`docs/contracts/OBSERVABILITY.md`、`docs/contracts/AUDIT.md`、`deploy/tls/README.md`。

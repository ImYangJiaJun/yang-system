# S01 生产事实采集（只读）

- **日期**：2026-10-25 05:10–05:20 CST
- **范围**：测试机 `47.109.148.207`（`evx_test`，已免密 SSH）；财务机 `47.112.16.98`（仅外部端口探测，内部采集待授权）
- **方法**：只读命令；不读取、不打印任何 Token／密码／私钥；配置只取 5 个非密键
- **关联**：`docs/assessments/2026-10-25-network-infrastructure-audit-adversarial-review-and-roadmap.md` 的 S01

> 本文件不包含凭据。`config.cloud.toml` 只读取了 `environment`／`trusted_proxy_cidrs`／`log_inbound_requests`／`link_base_url`／`redis.url` 是否存在 userinfo；其余内容未打开。

---

## 一、测试机 `47.109.148.207`

### 1.1 主机与容器

| 项 | 实测 |
|---|---|
| hostname | `iZ2vcdm3ooxcv6gpopwl6uZ` |
| 时间 | `2026-10-25T05:11:53+08:00`，NTP 同步正常，offset ≈ 0.0001s |
| 宿主性质 | **多租户**：同机约 30 个其他项目容器（hk2、joymeet、pts、cd-ledger、finance-management 等），不是审计假设的单机单租户 |
| yang 容器 | `yang-backend`（healthy）、`yang-frontend`（healthy）、`yang-mysql`（healthy）、`yang-redis`（healthy） |
| 应用端口 | `yang-backend` 发布 `0.0.0.0:18654->8081/tcp`；管理面 `127.0.0.1:9154->9090/tcp`；`yang-frontend` 无宿主端口（共享 backend netns） |
| 监听 | `0.0.0.0:18654`、`0.0.0.0:80`、`0.0.0.0:443`、`127.0.0.1:9154` |
| 网络 | `yang-system-net` = `172.31.0.0/16`，网关 `172.31.0.1`；成员仅 `yang-redis`／`yang-backend`／`yang-mysql`；`bridge` = `172.17.0.0/16` |
| 镜像 | `yang-system-backend:live`、`yang-system-frontend:live`；`Created=2026-10-23T10:58:34Z`；backend 无 label，frontend 只有 nginx maintainer label |

### 1.2 既有 80/443 边缘

| 项 | 实测 |
|---|---|
| 80/443 归属 | 容器 `shared-gateway`（`nginx:1.27-alpine`，`0.0.0.0:80->80`、`0.0.0.0:443->443`） |
| 网络 | `claude-code-hub-prod_default`、`shared-gateway_default`；**不在** `yang-system-net` |
| 配置 | `/mnt/data0/data/prod/shared-gateway/nginx-conf.d` → `/etc/nginx/conf.d`；已有 cch／joymeet／ecommerce-ops／mcp 等 server block；**grep 不到 `yang` 或 `18654`，即 yang-system 没有 TLS server block** |
| 证书 | `/etc/letsencrypt/live` 已有 3 个其他域名证书；`certbot.timer` 与 `snap.certbot.renew.timer` 活跃；`renewal-hooks/deploy/reload-shared-gateway.sh` 存在 |
| 外部行为 | `http://47.109.148.207/` → 404（nginx/1.27.1）；`https://47.109.148.207/` → 302 到 `http://47.109.148.207/client/`（其他项目）；`http://47.109.148.207:18654/` → 200；`https://47.109.148.207:18654/` → TLS 握手失败 |

**结论**：测试机已有可复用的 TLS 边缘与自动续期，但 yang-system 尚未接入；P0 的「明文 18654 公网可达」**确认成立**。TLS 落地不需要新装宿主 nginx，只需要给 `shared-gateway` 增加 yang server block + 证书 + DNS。注意这是**共享网关**，reload 失败会影响其他项目，变更必须 `nginx -t` 后 reload、不 restart。

### 1.3 配置（只读 5 个非密键）

| 键 | 实测值 | 影响 |
|---|---|---|
| `[app].environment` | `"test"` | production-only fail-fast 不生效 |
| `[security].trusted_proxy_cidrs` | `["127.0.0.1/32"]` | 后端信任本机 nginx；因 `Forwarded` 未清洗，**D2-2 的伪造链在测试机成立** |
| `[feishu].log_inbound_requests` | `false` | 测试机当前不泄 Token；财务机待核 |
| `[email.password_reset].link_base_url` | `"http://47.109.148.207:18654"` | 明文 http；切 production 前必须改 https |
| `[redis].url` | 不含 userinfo | Redis 无认证，与 `PING` 结果一致 |
| `[http]` | 仅 `[http]` 段，无 `max_body_bytes`／`request_timeout_seconds` 覆盖 | 走默认 16 MiB／60s |
| `[logging]` | 无该段 | 走默认 filter |

**新发现（审计未列）**：`/home/yjj/yang-system/config.cloud.toml` 权限为 **`666`**（`-rw-rw-rw-`，`yjj:yjj`，13671 B，2026-10-17 修改）。该文件含全部生产凭据，且宿主是多租户；任何本地用户可读写。建议立即 `chmod 600`（部署脚本以 `yjj` 运行，仍然可读；容器只读挂载不受影响）。

### 1.4 Redis／MySQL／备份／监控

| 项 | 实测 | 影响 |
|---|---|---|
| Redis | `PING` → `PONG`（无认证）；`maxmemory=0`；`maxmemory-policy=noeviction`；镜像 `redis:7-alpine`；无宿主端口 | D5-2 确认；无内存上限 |
| MySQL | `mysql:8.0`；无宿主端口；与 Redis 同网络 | 仅 `yang-system-net` 三成员可直连 |
| 备份 | 无 MySQL 备份 timer／cron；只有 `dpkg-db-backup.timer`（系统包数据库，无关） | 「唯一事实源无自动备份」确认 |
| 监控/采集端 | 无 Prometheus／Alertmanager／Grafana／Alloy／exporter 容器 | 13 条告警规则没有消费者；S50 必须先解决采集端 |
| 容器限额 | 四个 yang 容器 `Memory=0`、`MemorySwap=0`、`NanoCpus=0`、`PidsLimit=null`；`StopTimeout` 未设置（默认 10s） | D2-7 确认；D2-5 的 10s SIGKILL 前提成立 |
| 日志 | 宿主 daemon.json：`json-file` + `max-size=100m`，无 `max-file`；四个容器 `LogConfig` 显示 `max-size=100m` | 「无日志轮转」要修正为「有 100 MiB 单文件上限，无多文件轮转」；不会无限写盘，但历史会被覆盖 |

### 1.5 外部可达性

| 目标 | 结果 |
|---|---|
| `http://47.109.148.207:18654/` | 200 |
| `http://47.109.148.207:18654/health/ready` | **200（匿名公网可打 DB/Redis）** |
| `https://47.109.148.207:18654/` | TLS 握手失败 |
| `http://47.109.148.207/` | 404（shared-gateway 无 yang block） |
| `https://47.109.148.207/` | 302 到其他项目 |

---

## 二、财务机 `47.112.16.98`（仅外部探测）

| 目标 | 结果 | 说明 |
|---|---|---|
| `http://47.112.16.98/` | 连接超时（6s） | 80 被安全组阻断或无监听 |
| `https://47.112.16.98/` | 连接超时（6s） | 443 同样不可达 |
| `http://47.112.16.98:18654/` | **200** | 明文应用边缘公网可达 |
| `http://47.112.16.98:18654/health/ready` | **200** | 匿名 readiness 公网可达 |
| `https://47.112.16.98:18654/` | TLS 握手失败 | 无 TLS |

**结论**：财务机 P0 同样成立；与测试机不同，财务机 80/443 **外部不可达**。TLS 方案必须二选一：

1. 在安全组放行 443（并让 certbot 走 DNS-01 或放行 80 做 HTTP-01）；
2. 在已放行的 18000–19000 段内选一个 TLS 端口（如 18655），用 DNS-01 证书；飞书 external-options URL 可以带端口，但需要确认飞书侧允许。

财务机内部事实（端口绑定、config 键、Redis、备份、监控、限额）**尚未采集**：本机没有可用免密 key，SSH 返回 `Permission denied (publickey,password)`。需要操作者用密码登录执行，或临时加 key。

---

## 三、与审计发现的对应

| 发现 | 测试机实测 | 财务机实测 | 结论 |
|---|---|---|---|
| D2-1 公网明文边缘 | `0.0.0.0:18654` 公网 200，无 TLS | `:18654` 公网 200，无 TLS；80/443 不可达 | **P0 确认，两台都是** |
| D2-2 `Forwarded` 可伪造 | `trusted_proxy_cidrs=["127.0.0.1/32"]` + 边缘未清 Forwarded | 未知（待内部采集） | 测试机**可利用**；先加 `proxy_set_header Forwarded "";` |
| D1-1 portal 无真实 IP | 成立（代码） | 未知 | portal 限流仍是共享桶 |
| D1-3／D7-2 公开 readiness | `/health/ready` 200 | 同 | 确认 |
| D6-1 日志泄 Token | `false` | 未知 | 测试机不泄；财务机必须核 |
| D5-2 Redis 无认证 | `PONG`、`maxmemory=0` | 未知 | 测试机确认；宿主多租户，但 `yang-system-net` 仅 3 成员 |
| D2-7 无资源上限 | `Memory=0`、`NanoCpus=0` | 未知 | 测试机确认；日志有 100 MiB 上限（审计说「无轮转」过强） |
| D7-7 告警无消费端 | 无任何监控容器 | 未知 | 确认「规则存在但没人抓」 |
| 缺失不变量：备份 | 无备份 timer/cron | 未知 | 测试机确认 |
| 缺失不变量：静态秘密 | `config.cloud.toml` 666 | 未知 | **新发现，建议立即 chmod 600** |
| S75 制品身份 | `:live` tag、无 revision label | 未知 | 测试机确认 |
| S30/S31 TLS 成本 | 已有 shared-gateway + certbot（3 个其他域名） | 80/443 被阻断 | 测试机可复用网关；财务机需先开 443 或走 18xxx TLS |

---

## 四、建议的下一步（未执行，需确认）

1. **立即（测试机，安全加固）**：`chmod 600 /home/yjj/yang-system/config.cloud.toml`。理由：文件含全部明文凭据、宿主多租户、当前 `666`。部署用户 `yjj` 仍是 owner，容器只读挂载不受影响。
2. **立即（代码，测试机）**：在 `frontend/deploy/nginx.conf` 的 API location 加 `proxy_set_header Forwarded "";`（1 行），重建前端镜像并发布；这是 D2-2 的最小修复。不要在容器 nginx 把 XFF 覆盖成 `$remote_addr`——共享 netns 下会把真实客户端 IP 换成网关地址；XFF 保持追加，由最外层边缘覆盖。
3. **财务机内部 S01**：用密码登录执行文末命令块，或临时加本机 `id_ed25519` 公钥；重点核对 `log_inbound_requests`、`trusted_proxy_cidrs`、端口绑定、Redis、备份、限额。
4. **TLS 方案调整**：测试机复用 `shared-gateway`，加 yang server block + 域名证书（`nginx -t` 后 reload，不 restart）；财务机先决定 443 安全组 or 18xxx TLS。
5. **Redis 认证前**：先确认 `-ResetDb` 的 FLUSHALL、compose healthcheck、所有直连客户端都能带密码；按报告 S57 三步走。
6. **备份**：先落逻辑 dump + off-host 加密 + 恢复演练，再谈 binlog 与 RPO/RTO。
7. **监控**：先决定采集端（Prometheus/Alloy/云监控）归属，否则 S50 的规则不会响。

---

## 五、财务机内部采集命令块（只读，不打印凭据）

在财务机 `/home/yjj/yang-system`（或实际目录）执行；只回传输出，不需要 `cat` 整份配置：

```bash
hostname; date -Is
docker ps --filter name=yang-backend --filter name=yang-frontend --format '{{.Names}}\t{{.Status}}\t{{.Ports}}'
(ss -lntp 2>/dev/null || netstat -lnt 2>/dev/null) | grep -E ':(80|443|8080|8081|9090|9154|9155|18654|18655)\b' || true
docker network inspect yang-system-net -f '{{json .IPAM.Config}}' 2>/dev/null || true
docker network inspect bridge -f '{{json .IPAM.Config}}' 2>/dev/null || true

# 只取非密键；不要 cat 整份文件
cd /home/yjj/yang-system
awk -F= '
/^[[:space:]]*\[app\]/{s="app";next}
/^[[:space:]]*\[security\]/{s="security";next}
/^[[:space:]]*\[feishu\]/{s="feishu";next}
/^[[:space:]]*\[email\.password_reset\]/{s="email";next}
/^[[:space:]]*\[redis\]/{s="redis";next}
s=="app" && /^[[:space:]]*environment[[:space:]]*=/{print "app.environment =" $2}
s=="security" && /^[[:space:]]*trusted_proxy_cidrs[[:space:]]*=/{print "security.trusted_proxy_cidrs =" $2}
s=="feishu" && /^[[:space:]]*log_inbound_requests[[:space:]]*=/{print "feishu.log_inbound_requests =" $2}
s=="email" && /^[[:space:]]*link_base_url[[:space:]]*=/{print "email.link_base_url =" $2}
s=="redis" && /^[[:space:]]*url[[:space:]]*=/{print ($0 ~ /@/ ? "redis.url = <contains userinfo>" : "redis.url = <no userinfo>")}
' config.cloud.toml
stat -c 'config mode=%a owner=%U:%G size=%s' config.cloud.toml

docker exec yang-redis redis-cli PING 2>&1
docker exec yang-redis redis-cli CONFIG GET maxmemory 2>&1
docker exec yang-redis redis-cli CONFIG GET maxmemory-policy 2>&1
docker inspect -f '{{.Name}} image={{.Config.Image}} mem={{.HostConfig.Memory}} swap={{.HostConfig.MemorySwap}} nanocpus={{.HostConfig.NanoCpus}} log={{json .HostConfig.LogConfig}} restart={{.HostConfig.RestartPolicy.Name}}' yang-backend yang-frontend yang-mysql yang-redis 2>&1
docker ps --format '{{.Names}}\t{{.Image}}' | grep -iE 'prometheus|alertmanager|grafana|alloy|exporter|node-exporter' || echo 'no monitoring containers'
systemctl list-timers --all 2>/dev/null | grep -iE 'backup|dump|mysql' || echo 'no backup timer'
crontab -l 2>/dev/null | grep -iE 'backup|dump' || echo 'no user backup cron'
timedatectl 2>/dev/null | sed -n '1,8p'; (chronyc tracking 2>/dev/null | sed -n '1,6p' || true)
```

外部探测（在本机执行，不经 SSH）：

```bash
for u in http://47.112.16.98/ https://47.112.16.98/ http://47.112.16.98:18654/ http://47.112.16.98:18654/health/ready; do
  printf '%-50s ' "$u"
  curl --noproxy '*' -k -sS -o /dev/null -w '%{http_code}\n' --max-time 6 "$u" || echo FAILED
done
```

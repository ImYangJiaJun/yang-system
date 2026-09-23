# yang-system 部署（服务器 `/home/yjj/yang-system`）

本目录是**部署产物**：本地打包脚本、服务器侧蓝绿脚本、服务器配置模板、基础设施 compose。

## 文件清单

| 文件 | 在哪跑 | 作用 |
|---|---|---|
| `deploy.ps1` | **本机 Windows** | 构建两个镜像 → 导出压缩 → 上传 → 触发远程 |
| `deploy-blue-green.sh` | **服务器** | 载入镜像、起绿容器冒烟、切流量、回退 |
| `config.cloud.example.toml` | 入库 | 配置模板（占位值，可提交） |
| `config.cloud.toml` | **服务器** | 生产配置（**由模板复制而来，凭据只在服务器上填**） |
| `compose.infra.yaml` | **服务器** | MySQL + Redis（只起依赖，不起应用） |

> **只有 `config.cloud.toml` 需要你维护。** MySQL 的账号/密码/库名由脚本从
> `[mysql].url` 解析出来喂给 compose，root 密码首次使用时**自动随机生成**并存到
> `/home/yjj/yang-system/.mysql-root-password`（chmod 600，请纳入备份）。

## 拓扑

```
本机 Windows                          服务器 /home/yjj/yang-system
┌──────────────┐                     ┌────────────────────────────────────┐
│ deploy.ps1   │  scp 镜像包 + 脚本   │ deploy-blue-green.sh               │
│  build ×2    │ ──────────────────▶ │  ├─ yang-backend   ┐ 共享 netns     │
│  save + gzip │  ssh 触发远程命令    │  ├─ yang-frontend  ┘（同色一对）     │
└──────────────┘                     │  ├─ yang-mysql / yang-redis        │
                                     │  └─ config.cloud.toml（只读挂载）    │
                                     └────────────────────────────────────┘
                                                    │
                            18654（默认 loopback / 当前公网直连）
                                                    │
                                     ┌──────────────▼──────────────┐
                                     │ 受信 TLS 边缘（HTTPS / 443） │ ← 尚未配置
                                     └─────────────────────────────┘
```

**每个颜色是「一对」容器**：后端是网络命名空间的拥有者并发布端口，前端用
`--network container:<同色后端>` 加入它的命名空间。`nginx.conf` 的 upstream 写死
`127.0.0.1:8080`，只有共享 netns 才能生效。

---

## 一、一次性准备（服务器）

1. **装 Docker**（含 compose 插件）与 `curl`、`gunzip`。

2. **建目录**（**必须**，`deploy.ps1` 不再代建，原因见下面的「上传卡住不动」）：
   ```bash
   mkdir -p /home/yjj/yang-system && cd /home/yjj/yang-system
   ```
   目录不存在时上传会失败，脚本会提示你回来做这一步。

3. **填配置（只在服务器上填）**。第一次跑 `deploy.ps1` 会传上模板并复制成 `config.cloud.toml`，然后在服务器上：
   ```bash
   cd /home/yjj/yang-system
   vi config.cloud.toml          # 填所有 replace-with-*
   openssl rand -base64 48       # 生成各段密钥（每段独立，互不复用）
   openssl rand -hex 24          # 生成 MySQL 密码（只能字母数字）
   ```

   > ⚠️ **凭据只填在服务器上，本机不要留生产口令。**
   > `deploy.ps1` 只上传模板，**绝不上传本机的 `config.cloud.toml`**；
   > 那份文件也已被 `.gitignore` 忽略。

   **两条会挡住启动的硬约束**：
   - `[email.password_reset].link_base_url` 在 `environment = "production"` 下**必须是 https**
     （http 只允许 development / test）。TLS 边缘没落地之前，这一项会挡住启动。
   - `[feishu]` 段的合法键**只有** `enabled` / `management_api_token` / `encryption_key` /
     `app_id` / `app_secret` / `pull_interval_seconds`。它有 `deny_unknown_fields`，
     多写一个键就在反序列化阶段直接起不来。`enabled = true` 且 Token 非空会注册飞书
     **入站写入 API**——Token 是占位值时，那等于开放一个口令写在仓库里的写接口。

   **要在 TLS 落地前先用 http 联调飞书**：把 `[app].environment` 改成 `"test"` 同时把
   `link_base_url` 换成 `http://<公网IP或域名>:18654`（飞书审批的「关联外部选项」官方
   明确支持 HTTP 或 HTTPS，不受影响）。**这只是联调态**：`test` 下密码重置链接会以明文
   发出，且生产的 https 强制被关掉；拿到证书后必须改回 `production` + https。

   `deploy-blue-green.sh` 会在启动前检查占位值，**没填完会拒绝部署并把未填项列出来**。
   注意它只认 `replace-with-*` 一族：**`CHANGE_ME_*` 不会被识别为占位**，长度够就会
   静默通过校验，服务带着一个写在仓库里的"密钥"正常启动。填真值，别只改前缀。

4. **起基础设施**（只做一次）：
   ```bash
   ./deploy-blue-green.sh infra-up
   ```
   它会：从 `config.cloud.toml` 解析出 MySQL 账号/密码/库名 → 生成 root 密码（首次）→
   起 MySQL + Redis → 等 MySQL 就绪 → **确保 `[mysql].url` 里的库存在**（不存在就建，
   并授权给该账号）。首次初始化要几十秒。

---

## 二、首次部署

在**本机**（先把 `deploy.ps1` 顶部的 `$SshUser` / `$SshHost` / `$SshKey` 改成你自己的）：

```powershell
cd <lib_yang>\project\yang-system\deploy
.\deploy.ps1 -Mode deploy
```

它会：构建两个镜像 → 导出压缩 → 上传 → 远程 `smoke`（起绿容器并做健康检查）→ 远程 `cutover`（切流量）。

**首次部署时线上容器不存在**，脚本会提示「没有可备份的线上容器」，这是正常的。

---

## 三、日常部署

```powershell
.\deploy.ps1                 # build + 上传 + smoke + cutover 一条龙
.\deploy.ps1 -Mode smoke     # 只到冒烟为止，先验证不切流量
.\deploy.ps1 -Mode cutover   # 冒烟确认后，单独切（不重新打包）
.\deploy.ps1 -Mode rollback  # 回退到上一版本
.\deploy.ps1 -Mode status    # 看容器/镜像/网络状态
.\deploy.ps1 -Mode logs      # 追踪线上后端日志（Ctrl+C 退出）
.\deploy.ps1 -Mode logs -LogContainer yang-backend-green   # 追踪绿容器
.\deploy.ps1 -SkipBuild      # 跳过构建，复用上次导出的镜像包，直接上传（重试用）
```

服务器上也可以直接操作：`./deploy-blue-green.sh {status|logs|smoke|cutover|deploy|rollback}`。

**建议**：生产变更先 `-Mode smoke`，确认绿容器健康再 `-Mode cutover`——两段之间线上完全不受影响。

**重试提速**：构建两个镜像要 8~9 分钟（网络受限时更久）。若一次部署在**上传或远程步骤**
失败、而改动没变，别重跑构建——加 `-SkipBuild` 复用上次导出的镜像包：

```powershell
.\deploy.ps1 -SkipBuild
```

> 它要求 `yang-system-images.tar.gz` 还在本地。注意**上传成功后脚本会删掉这个包**，
> 所以那种情况下改用 `-Mode cutover` / `-Mode rollback` 重跑远程步骤。
> 镜像包不存在时脚本会直接报错，**不会**退化成悄悄重新构建。

### 排障：上传卡住不动

**症状**：`==> [4/5] 上传到 ...` 之后长时间没有输出。

**已实测的成因**（2026-09-23）：SSH 的 TCP 已建立、认证也成功，但**命令请求没送达**——
客户端 `ssh` 进程 CPU ≈ 0（不是在算，是在等），服务器上会话已建却**没有任何子进程**。
两端就这么互等，除非有一方超时。

**已做的两道防护**：

1. 上传不再先跑 `ssh ... "mkdir -p $RemoteDir"`，直接 `scp`（与
   `D:\code\ProfitScope\web\deploy.ps1` 的写法一致）。代价是远端目录改为**必须预先建好**。
2. 所有 `ssh` / `scp` 都带
   `-o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=4`，
   把这种「静默黑洞」变成**有界失败**（连不上 15s 报错；连上后失联约 60s 断开），
   不再是无限挂起。

卡住时按 `Ctrl+C`（或让脚本自己超时），然后 `.\deploy.ps1 -SkipBuild` 重试，不必重新构建。

---

## 四、端口与网络

| 端口 | 谁 | 绑定 | 说明 |
|---|---|---|---|
| **18654** | 线上应用边缘 | 见下 | **前后端共用**：`/api`、`/.well-known`、`/health` 转后端，其余走 SPA |
| 8155 | 绿容器边缘 | 见下 | 仅冒烟期间存在 |
| 9154 / 9155 | 管理面（`/metrics`、`/health/ready`） | **`127.0.0.1`**（`METRICS_BIND_ADDR`） | 给采集端与探针，**不随边缘一起暴露** |
| 8080 / 8081 / 9090 | — | 容器内 | 后端业务 / nginx / 后端管理面 |

**端口必须落在云安全组放行的范围内。** 这台服务器的安全组只放行 `80/443` 与 **`18000-19000`**
段，所以默认端口是 **18654**。早期用的 8154 在段外——从公网连不上（2026-09-23 实测：同机
18501/18093/18110 可达，8154 超时）。`LIVE_HOST_PORT` 可覆盖，但换到段外会让公网直接不可达。

**绑定地址 `BIND_ADDR`**：`deploy-blue-green.sh` 的**默认值是 `127.0.0.1`**（只对宿主机可见，
公网交给受信 TLS 边缘）。本测试环境用 `deploy.ps1 -EdgeBindAddr 0.0.0.0` **显式覆盖**成公网
直连——见第五节。

> ⚠️ **别用浏览器判断通不通。** Windows 系统代理会让 Chrome 对不可达端口返回**误导性的
> `503`**，把「连不上」伪装成「服务器错误」。一律用 `curl.exe --noproxy "*"` 直连验证。

机械校验的**边界**（重要）：`frontend/scripts/verify-deployment-contract.mjs` 会读本目录的
`deploy-blue-green.sh`，要求发布 8081 的 `-p` 绑定只能是 `127.0.0.1` 或 `${BIND_ADDR}`，
且 `BIND_ADDR` 的**源码默认值**必须是 loopback。它**不检查运行时环境变量**——所以它保证的是
「默认不暴露」，**不是**「不会暴露」。前端 `pnpm check` 会跑它。

---

## 五、当前暴露形态与 TLS 边缘

**本测试环境当前是公网明文直连**：`http://47.109.148.207:18654/`，由
`deploy.ps1 -EdgeBindAddr 0.0.0.0` 发布（`deploy.ps1` 的该参数默认值即 `0.0.0.0`，
所以不带参数也是公网直连；要收回请显式传 `-EdgeBindAddr 127.0.0.1`）。

> ⚠️ 这是**明文 http**：登录凭据、会话 Cookie、密码重置令牌都在公网上不加密传输。
> 之所以这样，是因为飞书审批的「关联外部选项」要求**公网可访问、不能是内网地址**
> （官方原文：`docs/reference/feishu` 的 `approval/.../associate-external-options.md`，
> 另有 **3 秒**请求超时），而 TLS 边缘还没落地。
> **上线真实用户前必须换成下面的形态之一。**

演进路径，按暴露面从小到大：

1. **宿主 nginx 只放行取选项路径**（最小暴露）：`location ^~ /api/v1/feishu/approval/options/`
   → `proxy_pass http://127.0.0.1:18654`，其余 `return 404`。控制台仍走 SSH 隧道，
   登录表单不会以明文暴露在公网。
2. **宿主 nginx 全量反代到 `127.0.0.1:18654`**（推荐，本节的最终形态）：用
   `-EdgeBindAddr 127.0.0.1` 重新发布，宿主边缘终结 TLS 后反代。重新部署不会改变暴露面。

两种都要：

- 只允许 TLS 1.2/1.3，HTTP 永久重定向到 HTTPS；
- **覆盖**客户端传入的 `Forwarded` / `X-Forwarded-*`，再转发给 18654；
- 保留应用返回的 CSP、HSTS 等安全响应头；
- 对 `/api`、`/.well-known`、`/health` 不做 SPA fallback 或 HTML 缓存；
- 云安全组放行边缘端口（80/443），并可把 18654 收回。

加好边缘后，**回来把 `config.cloud.toml` 的 `[security].trusted_proxy_cidrs` 填上**，
并把 `[email.password_reset].link_base_url` 改回 `https://`、`[app].environment` 切回
`production`（当前是 `test`，为的是在 TLS 落地前允许 http 的 reset 链接）。

不填的后果是**客户端 IP 退化成 TCP 对端地址**（不是"HTTPS 被当成 HTTP"——后端
**只读 `Forwarded` 与 `X-Forwarded-For` 推导客户端 IP，不读 `X-Forwarded-Proto`**；
登录 Cookie 的 Secure 由 Origin/Referer 的 scheme 决定，与转发头无关）。
本拓扑下前端 nginx 与后端共享 netns、upstream 写死 `127.0.0.1:8080`，后端看到的对端
永远是 nginx，于是 `security.auth_rate_limit_ip_attempts`（默认 30 次/60 秒）变成
**全站共享的单一 IP 桶**。

取值取决于边缘放在哪：边缘在宿主机上 → 通常要同时信 `127.0.0.1/32` 与 docker 网桥网段；
边缘是同网络里的容器 → 填该网络子网。查看网段：

```bash
docker network inspect yang-system-net -f '{{.IPAM.Config}}'
docker network inspect bridge -f '{{.IPAM.Config}}'
```

---

## 六、排障

```bash
# 容器状态与端口
./deploy-blue-green.sh status

# 后端日志（启动失败最常见的原因是配置校验）
docker logs --tail 200 yang-backend

# readiness 探针（带 MySQL/Redis 依赖检查）
curl -sS -o /dev/null -w '%{http_code}\n' http://127.0.0.1:9154/health/ready

# 应用边缘
curl --noproxy '*' -I http://127.0.0.1:18654/
```

| 症状 | 多半是 |
|---|---|
| 容器起来几秒就退出，日志含 `unknown field` | `config.cloud.toml` 里有 schema 不存在的键。`[feishu]` 段的合法键**只有** `enabled` / `management_api_token` / `encryption_key` / `app_id` / `app_secret` / `pull_interval_seconds`（`deny_unknown_fields`，多一个就起不来） |
| 容器起来几秒就退出，日志含「必须使用 https」 | `[email.password_reset].link_base_url` 用了 http，而 `environment = "production"` 下只接受 https |
| 容器起来几秒就退出，日志含「占位」或密钥长度 | 有未填的 `replace-with-*`，或密钥不足 32 字节 |
| 日志里有 `Unknown database` | 库不存在。跑 `./deploy-blue-green.sh infra-up`（内含建库步骤），或手动：`docker exec -e MYSQL_PWD="$(cat .mysql-root-password)" yang-mysql mysql -uroot -e "CREATE DATABASE IF NOT EXISTS yang_system CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci"` |
| 日志里有 `Access denied` | `[mysql].url` 的账号/密码与容器里实际的不一致（改了 config 但没重建 MySQL 容器；初始化只在数据卷为空时执行） |
| readiness 一直不 200 | MySQL/Redis 没起、密码不匹配、或 `config.cloud.toml` 里的主机名不是 `yang-mysql` / `yang-redis` |
| `curl --noproxy '*' http://127.0.0.1:18654/` 不通 | 端口没发布 / 绑错地址，或容器没起来。看 `docker ps` 的端口映射与 `docker logs yang-backend` |
| **浏览器打不开、报 503** | ⚠️ **先怀疑系统代理**：Windows 代理会让 Chrome 对不可达端口返回**假 503**。用 `curl.exe --noproxy "*"` 直连复验；真不通再查云安全组是否放行了该端口（当前只放行 `80/443` 与 `18000-19000`） |
| 前端 502 | 前端容器没起来，或它没加入后端容器的网络命名空间（`--network container:` 写错） |

---

## 七、注意

- **数据库由脚本确保存在**：应用**只建表不建库**（全仓没有 `CREATE DATABASE`），
  所以 `infra-up` 与每次 `deploy` 前都会检查 `[mysql].url` 里的库并建它。
- **`.mysql-root-password`**（服务器上，chmod 600）：脚本首次运行时生成的 MySQL root 密码。
  你不需要维护它，但**要纳入备份**，也不要提交到仓库。丢了可以删掉此文件——
  下次会重新生成，但那时它已与实际 root 密码不符（MySQL 的 root 密码只在初始化时设定）。
- **数据卷**：`yang_mysql_data` / `yang_redis_data`。`docker compose down -v` 会**永久删除**，
  MySQL 是唯一事实源，Redis 不备份（见 `docs/operations/RUNBOOK_BACKUP.md`）。
- **应用容器由本脚本管理，不要用 compose 管理**：蓝绿需要精确控制容器名、端口与 netns，
  交给 compose 会互相打架。`compose.infra.yaml` 只管 MySQL/Redis。
- **回退保留一个版本**：`cutover` 会把上一版容器停掉并改名为 `*-blue`，镜像备份为
  `:previous`。再切一次会覆盖掉它——**连续两次部署后就不能回退到第一版了**。

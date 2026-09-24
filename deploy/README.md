# yang-system 部署（服务器 `/home/yjj/yang-system`）

本目录是**部署产物**：本地打包脚本、服务器侧蓝绿脚本、服务器配置模板、基础设施 compose。

## 文件清单

| 文件 | 在哪跑 | 作用 |
|---|---|---|
| `deploy.ps1` | **本机 Windows** | 构建两个镜像 → 导出压缩 → 上传 → 触发远程；另外两个开关 `-SyncConfig`（推配置）与 `-ResetDb`（清库） |
| `deploy-blue-green.sh` | **服务器** | 载入镜像、起绿容器冒烟、切流量、回退；`refresh` 子命令负责换配置与清库 |
| `config.cloud.example.toml` | 入库 | 配置模板（占位值，可提交） |
| `config.cloud.toml` | **服务器** | 生产配置（**由模板复制而来，凭据默认只填在服务器上**；也可以用 `-SyncConfig` 从本机推一份覆盖它） |
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

   > ⚠️ **默认只上传模板**：`deploy.ps1` 不会碰服务器上已填好的 `config.cloud.toml`
   > （凭据仍建议只留在服务器上）。想主动用本机那份覆盖它，用显式的 `-SyncConfig`
   > ——它会先备份服务器上的原文件、打印键级差异预览，失败还会自动回滚，
   > 见下面「三、日常部署」里的「刷新配置 / 重置数据库」。
   > 本机那份 `config.cloud.toml` 已被 `.gitignore` 忽略。

   **两条会挡住启动的硬约束**：
   - `[email.password_reset].link_base_url` 在 `environment = "production"` 下**必须是 https**
     （http 只允许 development / test）。TLS 边缘没落地之前，这一项会挡住启动。
   - `[feishu]` 段的合法键**只有** `enabled` / `management_api_token` / `encryption_key` /
     `app_id` / `app_secret` / `pull_interval_seconds` / `alert_recipients` /
     `alert_failure_threshold` / `log_inbound_requests`。它有 `deny_unknown_fields`，
     多写一个键就在反序列化阶段直接起不来。`enabled = true` 且 Token 非空会注册飞书
     **入站写入 API**——Token 是占位值时，那等于开放一个口令写在仓库里的写接口。
     `log_inbound_requests = true` 会把三个机器入口的完整请求参数（**含 Token 明文**）
     写进日志，只为联调抓飞书真实报文，**不要在生产开启**（见 `docs/contracts/OBSERVABILITY.md`
     的例外条款）。

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
.\deploy.ps1 -SyncConfig     # 推本机配置到服务器并热替换（不构建）
.\deploy.ps1 -ResetDb        # 清空本系统的库 + Redis（不构建），由应用启动时重建 schema
```

服务器上也可以直接操作：
`./deploy-blue-green.sh {status|logs|smoke|cutover|deploy|rollback|refresh}`。

**建议**：生产变更先 `-Mode smoke`，确认绿容器健康再 `-Mode cutover`——两段之间线上完全不受影响。

**重试提速**：构建两个镜像要 8~9 分钟（网络受限时更久）。若一次部署在**上传或远程步骤**
失败、而改动没变，别重跑构建——加 `-SkipBuild` 复用上次导出的镜像包：

```powershell
.\deploy.ps1 -SkipBuild
```

> 它要求 `yang-system-images.tar.gz` 还在本地。注意**上传成功后脚本会删掉这个包**，
> 所以那种情况下改用 `-Mode cutover` / `-Mode rollback` 重跑远程步骤。
> 镜像包不存在时脚本会直接报错，**不会**退化成悄悄重新构建。

### 刷新配置 / 重置数据库

```powershell
.\deploy.ps1 -SyncConfig                        # 本机 config.cloud.toml → 服务器，热替换并重启应用
.\deploy.ps1 -ResetDb                           # 清空本系统的库 + Redis，由应用启动时重建 schema
.\deploy.ps1 -SyncConfig -ResetDb               # 两件一起做：重置配置 + 重置数据库
.\deploy.ps1 -Mode deploy -SyncConfig -ResetDb  # 先部署新镜像，**再**重置
.\deploy.ps1 -ResetDb -Yes                      # 跳过交互确认（脚本化用）
```

两个开关都**不涉及镜像**：不给 `-Mode` 时不会构建（刷新配置/清库是运维动作，不该顺带重打
8~9 分钟）。给了 `-Mode deploy` 时是「先部署、后重置」——**顺序不能反**：库要清在「新版本已经
在跑」之后，否则旧容器一被 `--restart` 拉起来就会把**旧 schema** 同步进刚清空的库，破坏性
schema 变更就白做了。与 `-Mode logs` / `-Mode upload` 组合会直接报错（前者是交互式阻塞命令，
后者本就不触发远程）；`-ResetDb` 还**只认 `-Mode deploy`**——别的 Mode 跑完时线上仍是旧版本，
重启起来的旧容器会把旧 schema 同步进刚清空的库（`smoke` 尤其容易踩：它只起绿容器，线上那对
根本没换）。

> **首次部署的例外**：服务器上还没有 `config.cloud.toml` 时，`-Mode deploy -SyncConfig` 会
> **提前报错**（部署一上来就要用配置，而配置是在部署之后才安装的）。先单独推一次配置
> （`.\deploy.ps1 -SyncConfig`），再正常部署。

服务器上也可以直接调用（`refresh` 的动作由环境变量选）：

```bash
SYNC_CONFIG=1 ./deploy-blue-green.sh refresh
RESET_DB=1 RESET_DB_CONFIRM=yes ./deploy-blue-green.sh refresh
SYNC_CONFIG=1 RESET_DB=1 RESET_DB_CONFIRM=yes ./deploy-blue-green.sh refresh
```

两者都不可逆，所以默认会打印影响面并要求输入 `yes`；加 `-Yes` 可以跳过，但**无法交互时它会
直接失败**，不会因为「没人回答」而放行。远端还有一道 `RESET_DB_CONFIRM=yes` 的硬闸门，
它由 `deploy.ps1` 在确认通过后才转发。

**`-SyncConfig` 做的事**：

1. 本机先校验（占位值 / 缺 `[authorization]` 段 / URL 写法），不合格就不上传；
2. 打印**键级差异预览**（只列键名，一个值都不打印），并把会「让存量状态失效」的键单独标出：

   | 键 | 改了会怎样 |
   |---|---|
   | `[token].active_secret` | 所有已签发 Token 立即失效（所有人要重新登录） |
   | `[security.totp].aead_key` | 它加密库里的 `users.totp_secret` —— 已绑定 TOTP 的用户登录不了 |
   | `[feishu].encryption_key` | 它封存库里的飞书 token 密文 —— 那些密文解不开 |

3. 上传为服务器的暂存文件 `config.cloud.toml.upload`，由远端校验后**就地安装**（原地截断写，
   保住 inode：配置是单文件 bind mount，换 inode 的写法运行中的容器读不到新内容），
   原文件备份为 `config.cloud.toml.bak.<时间戳>`；
4. 重启应用容器（配置只在进程启动时读一次，光换文件不生效），健康检查不通过就
   **自动把备份还原回去**并再次重启，然后以非 0 退出。

`[http].bind` / `[observability].metrics_enabled` / `[observability].metrics_bind` 三项**推配置
改不动**：`docker/app/Dockerfile` 里同名 ENV 的优先级更高（优先级为 配置文件 < 环境变量）。

**`-ResetDb` 做的事**（`DROP DATABASE` + 重建空库 + 清 Redis）：

- 清的库 = 服务器 `config.cloud.toml` 里 `[mysql].url` 指向的那个库。MySQL 容器、数据卷、
  root 密码、同机其它库都**保留**；要连容器一起重置才用 `docker compose down -v`。
  库名从 `[mysql].url` 解析出来，脚本会拒绝清系统库与含非法字符的库名，也**一律拒绝**在
  `[mysql].url` / `[redis].url` 指向非本容器时动手——MySQL 的 SQL 全部走
  `docker exec yang-mysql`、Redis 的清空走 `docker exec yang-redis`，都不按 URL 里的 host 建连接，
  所以对外部实例执行只会「打错目标还报成功」。真要清外部库/缓存请由 DBA 处理。
- 应用账号会按 `[mysql].url` **重新对齐**（`CREATE USER` / `ALTER USER` / `GRANT`）。
  MySQL 的账号密码只在数据卷为空时初始化，改了配置里的账号/密码而不重建卷是不会生效的
  （症状就是日志里的 `Access denied`）。
- Redis 一起清（会话 / 授权版本缓存 / 验证码 / step-up 令牌）。只清库不清 Redis 会留下指向
  已消失用户的缓存，让「重置」停在半截状态；Redis 本就不备份，代价只是重新登录。
- 应用是「停 → 清 → 起」：schema 同步只在进程启动时跑一次，所以清完必须重启才会重建表。
  脚本会额外校验「库里的表数 > 0」——readiness 探针只做 `SELECT 1`，空库照样返回 200，
  证明不了 schema 已经重建。
  中途失败（清库语句、Redis、起容器）时，脚本会**先把容器起回去**再退出，不会把站点留在
  停着的状态；库若停在半途，修好原因后重跑一次同样的 refresh 即可重新对齐。
  服务器上还没有线上容器时，结论会明确写成「已执行，但**未经验证**」而不是「完成 ✔」。
- ⚠️ **清库之后没有任何账号能登录**：注册出来的新账号零权限，而授予权限只有手工 SQL
  （见 `docs/contracts/AUTHZ_GRANTS.md` 的「初始授权运维手册」），系统也没有「首个注册账号
  即管理员」的引导。
- ⚠️ **不可恢复**：MySQL 没有备份机制，清掉就没了（见 `docs/operations/RUNBOOK_BACKUP.md`）。
- ⚠️ 清库对**已签发 Token** 的影响：`users` 行被删掉后，请求期的授权版本校验会拒绝**绝大多数**
  旧 Token（缓存未命中或版本不等时会回查 MySQL 的 `users.authz_version`，用户不存在即拒）。
  但重建后的自增 id 从 1 重排：若旧 Token 的 `authz_version` 恰好等于新账号的版本（都取默认 1），
  就存在被继承的可能。要彻底作废，改 `[token].active_secret` 后再 `-SyncConfig`（所有人重新登录）。

> 这条路径也是应用**破坏性 schema 变更**（删列、改类型等）的唯一干净做法：schema 同步是
> 只增不删的，改结构得先清库再让应用重建。清库 + `-Mode deploy` 一条命令即可。

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

### 明文 HTTP 下浏览器会静默失去什么

非安全上下文（`http://<公网IP>:<端口>`）在浏览器里是**一套独立的能力集**，而且失败方式
很坑：一批 API **不是被拒绝，而是压根不存在**（`undefined`），调用点于是抛 `TypeError`
而不是一个能读出原因的 `NotAllowedError`——写惯了 `try/catch` 的代码会把它当成一次普通失败
吞掉，界面表现成「点了没反应」。

2026-09-24 在 `http://47.109.148.207:18654` 上实测到的清单：

| API / 响应头 | 该源上的形态 | 后果 | 代码侧现状 |
| --- | --- | --- | --- |
| `navigator.clipboard` | `undefined` | 所有「复制」按钮失效 | 降级到 `execCommand("copy")`，两条路都不通时给可手动复制的退路（`frontend/src/shared/lib/clipboard.ts`） |
| `navigator.locks` | `undefined` | 跨标签页的续期互斥消失：两个标签页同时刷新会撞上 Token Rotation，其中一个被登出 | 降级到 `localStorage` 租约（`frontend/src/engine/session/refresh-lock.ts`） |
| `crypto.randomUUID` / `crypto.subtle` | `undefined` | 客户端随机 UUID 与 WebCrypto 不可用 | `randomUUID` 已有兜底；`subtle` 未被使用 |
| `Cross-Origin-Opener-Policy` | 被浏览器忽略 | 失去跨源隔离 | 无功能依赖（未用 `SharedArrayBuffer`） |
| `Strict-Transport-Security` | 被浏览器忽略 | **HSTS 完全不生效**——它只对 HTTPS 响应生效 | 无（这是「明文下写什么都没用」的一项） |

注意 `navigator.geolocation` 与 `Notification` **不在**这张表里：它们的对象在明文源上
依然存在（不是安全上下文门控），区别在于「对象在不在」而不是「调用成不成」。

**上面的降级是「让界面在明文 HTTP 上也能用」，不是「明文 HTTP 没问题」。** 登录凭据、
会话 Cookie 与飞书审批 Token 依旧在公网上明文传输，第五节列的 TLS 形态仍是必须做的事。

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
| 容器起来几秒就退出，日志含 `unknown field` | `config.cloud.toml` 里有 schema 不存在的键。`[feishu]` 段的合法键**只有** `enabled` / `management_api_token` / `encryption_key` / `app_id` / `app_secret` / `pull_interval_seconds` / `alert_recipients` / `alert_failure_threshold` / `log_inbound_requests`（`deny_unknown_fields`，多一个就起不来） |
| 容器起来几秒就退出，日志含「必须使用 https」 | `[email.password_reset].link_base_url` 用了 http，而 `environment = "production"` 下只接受 https |
| 容器起来几秒就退出，日志含「占位」或密钥长度 | 有未填的 `replace-with-*`，或密钥不足 32 字节 |
| 日志里有 `Unknown database` | 库不存在。跑 `./deploy-blue-green.sh infra-up`（内含建库步骤），或手动：`docker exec -e MYSQL_PWD="$(cat .mysql-root-password)" yang-mysql mysql -uroot -e "CREATE DATABASE IF NOT EXISTS yang_system CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci"` |
| 日志里有 `Access denied` | `[mysql].url` 的账号/密码与容器里实际的不一致（改了 config 但没重建 MySQL 容器；初始化只在数据卷为空时执行） |
| readiness 一直不 200 | MySQL/Redis 没起、密码不匹配、或 `config.cloud.toml` 里的主机名不是 `yang-mysql` / `yang-redis` |
| `curl --noproxy '*' http://127.0.0.1:18654/` 不通 | 端口没发布 / 绑错地址，或容器没起来。看 `docker ps` 的端口映射与 `docker logs yang-backend` |
| **浏览器打不开、报 503** | ⚠️ **先怀疑系统代理**：Windows 代理会让 Chrome 对不可达端口返回**假 503**。用 `curl.exe --noproxy "*"` 直连复验；真不通再查云安全组是否放行了该端口（当前只放行 `80/443` 与 `18000-19000`） |
| 前端 502 | 前端容器没起来，或它没加入后端容器的网络命名空间（`--network container:` 写错） |
| `refresh` 之后 readiness 一直不 200 | 新推上去的配置有问题。看 `refresh` 的输出：没用 `-SyncConfig` 时它不会回滚，配置得自己修；用了的话它会把备份还原并重启（输出里会写「配置已回滚」） |
| `refresh` 报告「库 X 里一张表都没有」 | 应用启动了但 schema 同步没生效——通常是配置里的库名/账号不对（`docker logs yang-backend` 里会有线索）。清库本身没失败，别把它当成功 |
| 清库后表数正常，但**登不进去** | 这是预期行为：清库后没有任何账号，注册出来的账号零权限。按 `docs/contracts/AUTHZ_GRANTS.md` 手工插 `authz_grant` |
| `refresh` 报「[mysql].url 的主机名不是本脚本管理的容器」 | 脚本只清 `docker exec yang-mysql` 里的库，不按 URL 的 host 连接。若这确实是本机容器，把主机名改成 `yang-mysql` 再跑；真是外部实例则请 DBA 处理 |
| `refresh` 报「容器读不到刚写进 config.cloud.toml 的内容」 | 那份配置在容器创建之后被换过 inode（`sed -i` / `vim` / `mv`）。就地改写对它不生效，先 `./deploy-blue-green.sh deploy`（或 `cutover`）重建容器，再重试 `-SyncConfig` |
| `refresh` 报「脚本以退出码 N 中止，而容器之前被停过」 | 清库/换配置中途失败了。脚本会**自动把容器起回去**（然后你可以从日志排查）；库可能停在半途，修好原因后重跑一次同样的 refresh 即可对齐 |

---

## 七、注意

- **数据库由脚本确保存在**：应用**只建表不建库**（全仓没有 `CREATE DATABASE`），
  所以 `infra-up` 与每次 `deploy` 前都会检查 `[mysql].url` 里的库并建它。
- **`.mysql-root-password`**（服务器上，chmod 600）：脚本首次运行时生成的 MySQL root 密码。
  你不需要维护它，但**要纳入备份**，也不要提交到仓库。丢了可以删掉此文件——
  下次会重新生成，但那时它已与实际 root 密码不符（MySQL 的 root 密码只在初始化时设定）。
- **数据卷**：`yang_mysql_data` / `yang_redis_data`。`docker compose down -v` 会**永久删除**，
  MySQL 是唯一事实源，Redis 不备份（见 `docs/operations/RUNBOOK_BACKUP.md`）。
  注意 `-ResetDb` **不删卷**：它只清 `[mysql].url` 指向的那个库，容器、卷、root 密码都留着。
- **换配置 / 清库只走 `deploy.ps1 -SyncConfig` / `-ResetDb`（或服务器上的 `refresh`）**：
  两件事都必须重启应用容器才生效，手工 `scp` + 改文件容易「传了却没生效」——
  配置是单文件 bind mount，用 `mv` / `sed -i` 这类换 inode 的写法写下去，运行中的容器读到的
  仍是旧内容，连 `docker restart` 都救不回来。
- **应用容器由本脚本管理，不要用 compose 管理**：蓝绿需要精确控制容器名、端口与 netns，
  交给 compose 会互相打架。`compose.infra.yaml` 只管 MySQL/Redis。
- **回退保留一个版本**：`cutover` 会把上一版容器停掉并改名为 `*-blue`，镜像备份为
  `:previous`。再切一次会覆盖掉它——**连续两次部署后就不能回退到第一版了**。

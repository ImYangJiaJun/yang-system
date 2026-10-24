# YANG System 基础组件与网络基础设施审计（第一性原理）

- **日期**：2026-10-24
- **基线**：`main @ cd1c333`
- **范围**：`yang-base` / `yang-db` / `yang-runtime` + `project/yang-system` 的 HTTP 入口、出站客户端、飞书集成、MySQL/Redis、nginx/Docker 边缘、前端网络层、可观测性与关停链。
- **方法**：8 个维度并行只读审计，逐条对抗性复核，完整性批评者复查；未修改代码。
- **边界**：静态审计 + 上游源码核对；未做动态压测；未读取 `deploy/config.cloud*.toml` 等生产凭据文件。

> 路径约定：应用路径相对 `project/yang-system`；框架 crate 路径相对仓库根 `D:/code/lib_yang`。

---

## 目录

1. [结论先行](#一结论先行)
2. [第一性原理基线](#二第一性原理基线)
3. [逐组件详细发现](#三逐组件详细发现)
4. [横向主题](#四横向主题)
5. [30/60/90 路线图](#五306090-路线图)
6. [明确不建议做](#六明确不建议做)
7. [成熟系统对照](#七成熟系统对照参考基线)
8. [方法与局限](#八方法与局限)

---

## 一、结论先行

系统的基本骨架是扎实的：类型化 Action、请求 ID、multipart 上限、受信代理解析、会话续期、Schema 预检、关闭预算、审计/Outbox 都有真实实现。真正的问题集中在 **五个系统性缺口**：

| # | 系统性问题 | 典型证据 | 优先级 |
|---|---|---|---|
| 1 | 信任边界只做了一半：TLS 缺位、`Forwarded` 可伪造、日志可泄凭据 | `deploy/README.md:34,280-302`；`frontend/deploy/nginx.conf:79-82`；`crates/yang-base/src/transport/client_ip.rs:159-163` | **P0/P1** |
| 2 | 不可逆输出：错误分类把瞬态失败写成终态 | `src/addon/feishu/domain/outbound.rs:352-358`；`src/addon/feishu/domain/approval_dispatch.rs:1161-1176` | **P1** |
| 3 | 超时/背压没有单一链：nginx 非活动超时、应用 60s、Docker 10s、前端无超时、Redis recycle 300s、MySQL 无语句超时 | 见 D1/D2/D5/D7/D8 | **P1** |
| 4 | 蓝绿发布未闭环：启动期改共享 schema、回滚不回退 schema、前端会滞留在旧 netns | `src/bootstrap.rs:252-259`；`deploy/deploy-blue-green.sh:330-333,1231-1265,1330-1349` | **P1** |
| 5 | Redis 无认证、无内存上限，却承载授权版本、验证码、Step-up 单次消费 | `deploy/compose.infra.yaml:60-72` | **P1** |

**最值得先做的 10 件事**（按依赖顺序）：

1. 宿主 TLS 只放行飞书 options 路径并覆盖 `Forwarded`，再做全量 TLS 反代。
2. 生产禁止 `log_inbound_requests=true`，两台机人工核对。
3. `Fatal{code:0}` 改为可重试；`write_extended` 失败不得 ack 事件。
4. 审批 worker 有界重试 + `attempts` 真正递增 + 死信/告警。
5. Redis `requirepass`/ACL + `maxmemory 256mb` + recycle 超时降到 1–2s。
6. `docker stop -t 40`，并在 worker 关闭上做 abort/监督。
7. nginx 开 gzip + 显式 connect/send/read 超时 + 上传/下载 buffering 分流 + `limit_req`。
8. 前端统一 `AbortSignal.timeout`；非 JSON 错误保留 status/requestId。
9. 业务面 `/health/ready` 在边缘 404，只保留管理面 readiness。
10. JSON 路由体上限降到 1 MiB，multipart 保持 16 MiB（不要全局降，xlsx 导入会挂）。

---

## 二、第一性原理基线

一个可运营的网络系统至少要同时满足七条不变量。本次用它判断“是否真的成立”，而不是看代码是否存在：

1. **信任边界唯一**：只有受信边缘能写转发头；TLS 必须在凭据之前终结；日志不得成为凭据的第二出口。
2. **超时预算单向收敛**：外层超时 > 内层超时；区分“总时长”与“非活动时长”；每一跳都能回答“失败算在谁的预算里”。
3. **过载必须卸载**：有界队列 + 明确拒绝（429/503），而不是无限排队到超时；拒绝也要计入分母。
4. **不可逆动作先确认**：外部副作用未确认前不得写终态；不得把“解析不了”当成“确定失败”。
5. **状态归属明确**：单实例要机械强制；多实例必须有租约、集中状态或幂等键。
6. **可观测性覆盖分母**：SLI 要包含被拒绝、排队、超时的请求；规范事件必须在生产 filter 下真的落盘。
7. **交付与预算同口径**：预算按 gzip 算，线上就必须 gzip；契约要校验运行时，而不只是源码默认值。

评分（强/中/弱）：

| 维度 | 评分 | 一句话理由 |
|---|---|---|
| D1 入站 HTTP | 中 | 限制齐全，但过载只排队、body 上限一刀切、健康检查混在业务面 |
| D2 边缘/部署 | 弱 | TLS 缺位、代理头可伪造、静态资源裸传、发布生命周期未闭环 |
| D3 出站客户端 | 中 | 超时/上限有，但 connect/read 不分离、熔断未启用、指标缺失 |
| D4 飞书集成 | 中 | 状态机与 uuid 回捞是亮点，但错误分类、ack 语义、重试上限有洞 |
| D5 数据层 | 中 | 池与探活有，但 recycle 300s、无认证/内存上限、无语句超时 |
| D6 安全边界 | 弱-中 | Cookie/同源/限流键设计不错，但 TLS、Forwarded、日志泄密拖后腿 |
| D7 可观测/生命周期 | 弱 | 默认 filter 丢框架日志、SLI 只统计 Action、worker 无监督 |
| D8 前端网络层 | 中 | 跨标签页刷新锁是亮点，但零压缩、无超时、错误分类粗 |

---

## 三、逐组件详细发现

### D1 入站 HTTP 服务端

**现状与优点**：axum 0.8.9；每个 method-router 单独施加 `DefaultBodyLimit`（`crates/yang-base/src/transport/axum.rs:306`）；256 并发、60s 请求超时、关闭预算；multipart 临时目录与字段/文件/字节上限；响应头注入防护；请求 ID 透传。

**D1-1（P1）portal 拿不到真实客户端 IP，门户口令限流退化成全站单桶**

- 证据：`TrustedClientIpMiddleware` 全仓只在 `account.user` 装配（`src/addon/account/user/mod.rs:62`）；`feishu.portal` 只挂 `TokenAuthMiddleware`（`src/addon/feishu/portal/mod.rs:54`）。`client_ip_identity` 在扩展缺失时退回 TCP 对端（`crates/yang-base/src/transport/client_ip.rs:205-216`），而生产拓扑下对端恒为 nginx 的 `127.0.0.1`。
- 影响：portal 的 passphrase 限流键（`src/addon/feishu/domain/rate_limit.rs:53-60`）变成全局共享桶；攻击者可轻易锁死所有门户用户，审计 IP 也全部失真。
- 建议：给 portal 挂 `TrustedClientIpMiddleware`；`admin_users` 若将来引入 IP 限流也一并挂；加一条“伪造 Forwarded 不改变限流键”的变异测试。

**D1-2（P1）16 MiB 是全局 JSON 体上限，不是仅上传路由**

- 证据：`src/config/mod.rs:155-159` 默认 16 MiB；`axum.rs:306` 按 method-router 统一施加；JSON 先全量读入内存再建 `Value` 树（`axum.rs:559-598`）；全局并发 256。
- 影响：登录、门户等匿名 JSON 端点也接受 16 MiB；最坏 256 × 16 MiB ≈ 4 GiB 的内存上界，解析峰值更高。
- 建议：JSON 路由单独加 `DefaultBodyLimit::max(1 MiB)`，multipart 保持 16 MiB。**不要全局降到 1 MiB**——xlsx 导入硬需求是 16 MiB（`src/addon/feishu/datasource/actions/import_xlsx.rs:170-176`）。

**D1-3（P1）业务面 `/health/ready` 经 nginx 公开，无预算、无鉴权**

- 证据：`frontend/deploy/nginx.conf:68` 的 location 把 `/health` 整段反代到业务后端；`axum.rs:400-402` 的 `ready()` 直接 `await health_check()`；`Tools::health_check` 每次执行 MySQL `SELECT 1` 与 Redis `PING`（`crates/yang-base/src/tools.rs:203-232`），没有 readiness budget。
- 影响：匿名者可低成本放大 DB/Redis 负载；依赖变慢时 256 并发会在健康检查上堆积；drain 期间管理面 503 而业务面仍 200，语义相反。
- 建议：边缘 `location = /health/ready { return 404; }`，只保留管理面 readiness；若必须保留，加缓存/预算/鉴权。

**D1-4（P2）并发上限只排队不卸载**

- 证据：`axum.rs:318-330` 先 `ConcurrencyLimitLayer` 后 `TimeoutLayer`；tower 的 `ConcurrencyLimit` 在 `poll_ready` 等待信号量，`TimeoutLayer` 在 `call` 才启动计时。
- 影响：过载时请求排队到 408，没有 429/503 快速失败，也没有 in-flight/queue 指标。复核认为影响有界（nginx `worker_connections 1024` 封顶、排队请求尚未读 body），降为 P2。
- 建议：满额 `try_acquire` 返回 503 + `Retry-After`；补 `http_inflight`、`http_queue_wait_seconds`。

**D1-5（P2）404/405/408 不返回 JSON 包络，408 语义不符**

- 证据：未注册 `Router::fallback` / `method_not_allowed_fallback`（`axum.rs` 全文无命中）；`TimeoutLayer` 命中时返回空体；`x-request-id` 只在 `dispatch_request` 出口写入（`axum.rs:528`）。408 按 RFC 9110 §15.5.9 是“未及时收到完整请求”，服务端处理超时应该是 503/504。
- 建议：加 fallback 与 method-not-allowed fallback，统一 `ApiResponse` 包络并补 `x-request-id`；超时改 503 或 504。

**D1-6（P2）nginx 超时/缓冲未显式配置**

- 证据：`frontend/deploy/nginx.conf:68-84` 没有 `proxy_connect_timeout/proxy_send_timeout/proxy_read_timeout/proxy_buffering/proxy_request_buffering`，全部落默认值。
- 关键修正：`proxy_read_timeout` 是**两次读之间的非活动超时**，不是总时长；`proxy_buffering on` 只增加缓冲/临时文件，**不阻止**向客户端流式发送。真正的问题是“没有显式预算与余量”，不是“必然 60s 切断”。
- 建议：connect 3s、send 30s、read 60s；导入路由 `proxy_request_buffering off; client_body_timeout 300s; proxy_read_timeout 300s;`；下载/预览路由 `proxy_buffering off`。

**D1-7（P2）TraceLayer 默认 DEBUG 被生产 filter 丢弃**

- 证据：`axum.rs:335` 用 `TraceLayer::new_for_http()`；默认 span/on_response 是 DEBUG；生产 filter 为 `yang_system=info,tower_http=info,error`（`src/config/mod.rs:853-856`）。未进入 Action 的 404/405/408 没有访问日志，也没有 HTTP 状态/在途指标。
- 建议：INFO 级访问日志（request_id/route/status/latency）；加传输层 counter/histogram。

**D1-8（P2）readiness 在 listener bind 之前置位**

- 证据：`src/bootstrap.rs:372` `mark_ready()`；`axum.rs:200` 才在 `serve_with_shutdown` 内 bind。
- 影响：端口冲突/绑定失败时，管理面可能短暂报告 ready，探针把流量导到一个没有监听的边缘。
- 建议：bind 成功后再 `mark_ready`。

**D1-9（P2）边缘没有 `limit_req/limit_conn`**

- 证据：`frontend/deploy/nginx.conf:43-51` 只有 `client_max_body_size`。
- 建议：`limit_req_zone $binary_remote_addr zone=yang:10m rate=20r/s; limit_req zone=yang burst=40 nodelay; limit_conn yang 50;`，登录/派发端点单独收紧。

---

### D2 边缘与部署网络

**现状与优点**：蓝绿容器、镜像备份、发布前只读预检、健康检查带 `--noproxy`/`--max-time`、部署契约脚本；CSP/HSTS/Permissions-Policy 等安全头齐全；`server_tokens off`。

**D2-1（P0）公网明文边缘 + TLS 缺位**

- 证据：`deploy/README.md:34` 明确受信 TLS 边缘“尚未配置”；`:280-284` 当前线上容器仍是 `0.0.0.0:18654`；`:298-302` 明确这是明文 http，登录凭据/会话 Cookie/重置令牌不加密；`:320-329` 当前 `environment=test`。`frontend/deploy/nginx.conf:43-51` 只监听 8081，无 443。
- 影响：公网嗅探即可接管账号；HSTS 在明文下完全不生效；OAuth 回调与飞书 Token 同样明文。
- 建议（顺序很重要）：
  1. 宿主 nginx 只放行 `/api/v1/feishu/approval/options/` 并终结 TLS；
  2. 全量 TLS 反代到 `127.0.0.1:18654`；
  3. 再把 `EdgeBindAddr` 收回 `127.0.0.1`；
  4. 最后把 `environment` 切回 `production`，并补齐 `trusted_proxy_cidrs`。
- **不要先收回 loopback**：飞书“关联外部选项”要求公网可访问，先收回会直接打断现网功能。

**D2-2（P1）`Forwarded` 可被客户端伪造，当前就可利用**

- 证据：`frontend/deploy/nginx.conf:79-82` 只设置 `X-Forwarded-For/Host/Proto`，从不删除或覆盖客户端自带的 `Forwarded`；后端在 TCP 对端受信时**优先**解析 `Forwarded`（`crates/yang-base/src/transport/client_ip.rs:159-163`），从右向左取首个不受信跳（`:179-184`）。生产配置已把 `127.0.0.1/32` 设为受信（复核时核对部署配置）。
- 影响：公网请求带 `Forwarded: for=<任意IP>` 即可决定限流键；绕过登录 IP 桶、门户口令限速，并污染登录事件/会话记录的 IP。
- 建议：边缘 `proxy_set_header Forwarded "";`；XFF 覆盖为 `$remote_addr` 而不是追加；部署契约加变异测试。

**D2-3（P1）前端 `--network container:<backend>` 的 netns 僵尸**

- 证据：`deploy/deploy-blue-green.sh:330-333` 前端加入后端 netns；`:672-677` 脚本自己记录“backend 重启后 frontend 滞留在已消失 netns”；`:1488-1501` 只在 refresh 的健康检查阶段做一次重启自愈；后端带 `--restart unless-stopped`。
- 影响：MySQL 抖动/OOM 触发后端重启，前端仍留在旧 netns；readiness 绿、站点边缘 000，直到人工或下一次 refresh——静默全站不可用。
- 建议：常驻外部探针/守护（edge 000 且 backend ready 时重启前端）；长期把前端移到同一 bridge 网络，upstream 改 `yang-backend:8080`。

**D2-4（P1）蓝绿启动期同步 schema，rollback 不回退 schema**

- 证据：`src/bootstrap.rs:252-259` 每次启动执行 `sync_table_definitions`；`deploy/deploy-blue-green.sh:1231` smoke 阶段就起绿容器；`:1246-1265` cutover 前旧版本仍在服务；`:1330-1349` rollback 只 rename/start 旧容器，不碰 schema。
- 影响：新版本在旧版本仍服务时改共享库；破坏性 DDL（删列/改类型/重命名）会让旧代码直接 500；rollback 换回旧镜像但 schema 已是新的。
- 建议：expand-contract 兼容窗口；绿容器启动前迁移门禁；破坏性 DDL 前自动备份；rollback 剧本区分“只回镜像”与“回镜像 + 回 schema”；记录 schema 版本。

**D2-5（P1）`docker stop` 默认 10s < 应用关闭预算 30s**

- 证据：`deploy/deploy-blue-green.sh:337` `stop_pair() { docker stop "$2" "$1"; }` 未带 `-t`；`src/config/mod.rs` 默认 `shutdown.total_timeout_seconds=30`；`bootstrap.rs:406-452` 分阶段 drain。
- 影响：10s 后 SIGKILL，worker drain 不完整，任务停在 `creating`，日志被截断；在途导入/附件上传被强杀。
- 建议：`docker stop -t 40`（或 `--stop-timeout`），并让应用预算小于 Docker 宽限；实测 drain 时长。

**D2-6（P2）`trusted_proxy_cidrs` 模板为空，与真实部署漂移**

- 证据：`config.show.toml:327` 为 `[]`，`deploy/config.cloud.example.toml:149` 被注释；复核时核对部署配置，实际已设置 `127.0.0.1/32`。README 的“不填的后果”是条件句，不是现状。
- 建议：保留为默认加固项；`environment=production` 且列表为空时启动告警；模板与文档同步，避免下一次部署把真实配置覆盖回去。

**D2-7（P2）容器无内存/CPU 上限、无日志轮转**

- 证据：`deploy-blue-green.sh:320-333` 的 `docker run` 没有 `--memory/--cpus/--log-opt`；`deploy/compose.infra.yaml:24-72` 也没有 limits/logging。
- 影响：应用、MySQL、Redis 任一可吃满宿主内存触发 OOM；json-file 日志无上限会写满磁盘。
- 建议：应用 `--memory 1g --memory-swap 1g --cpus 2 --log-opt max-size=50m --log-opt max-file=5`；compose 加 `deploy.resources.limits` 与 logging。

**D2-8（P2）cutover 是 stop→start，存在确定性停机窗口**

- 证据：`deploy-blue-green.sh:1256` 停旧对，`:1265` 起新对；新对健康检查失败只提示手工 rollback（`:1325`）。
- 影响：单实例内部系统可接受的有界中断，但发布窗口内请求会失败；>10s 的长请求还会被 SIGKILL（见 D2-5）。
- 建议：先修 `stop-timeout` 与失败自动起旧；宿主 nginx 双 upstream 零停机对当前规模属于额外基础设施，非必需。

**D2-9（P2）nginx 代理超时/缓冲/限流/日志全停留在默认值**

- 证据：`frontend/deploy/nginx.conf` 全文无 `proxy_connect/read/send_timeout`、`proxy_buffering`、`proxy_max_temp_file_size`、`limit_req/limit_conn`、`log_format/access_log`。
- 建议：显式预算；访问日志加 `$request_time $upstream_response_time $upstream_addr $request_id`；对匿名端点加限流。

**D2-10（P2）部署契约只校验源码默认值，不校验运行时绑定**

- 证据：`deploy/README.md:271-274` 契约只要求 `BIND_ADDR` 源码默认值是 loopback；`:265-266` 测试环境用 `-EdgeBindAddr 0.0.0.0` 显式覆盖。
- 影响：CI 绿、运行时却是公网明文。
- 建议：部署后 `docker inspect` 断言实际绑定；公网绑定且无 TLS 时发布失败；把实际绑定写入发布证据。

**D2-11（P2）TLS 方案未含 HTTP/2 与证书续期/到期告警**

- 证据：`deploy/README.md:312-318` 只写 TLS 1.2/1.3、重定向、覆盖转发头、保留安全头。
- 建议：`listen 443 ssl http2`（或新版 `http2 on`）；certbot timer + `nginx -t && reload`；30/14/7 天到期告警。

**D2-12（P2）前端 Docker HEALTHCHECK 假阳性**

- 证据：`frontend/deploy/Dockerfile:33-34` 只 `wget --spider http://127.0.0.1:8081/`；SPA fallback 恒 200，后端不可达时容器仍 healthy。
- 建议：healthcheck 打管理面 readiness，或至少打一个会经过后端的路径。

---

### D3 出站 HTTP 客户端

**现状与优点**：reqwest 0.12.16；连接池复用；响应体上限有 Content-Length 快速失败与流式累计；URL 日志脱敏；飞书出站自建重试能读取业务码与 `x-ogw-ratelimit-reset`，比框架 `RetryConfig` 更贴合飞书语义。

**D3-1（P2）只有总超时，没有 connect/read 超时**

- 证据：`crates/yang-base/src/http/client.rs:264-268` 只设置 `.timeout()`；`HttpClientConfig` 无 `connect_timeout`；`crates/yang-base/src/http/request.rs:873` 也没有 `.read_timeout()`。
- 影响：DNS/建连慢会吃掉整个出站预算，无法区分“DNS 失败/建连失败/上游慢”；慢滴流可占满总超时。
- 建议：加 `connect_timeout(3s)` 与 `read_timeout(10s)`；错误分类区分 DNS/connect/read；补 `outbound_connect_seconds`、`dns_failures_total`。

**D3-2（P2/backlog）`RetryConfig` 无生产调用方，且文档与实现漂移**

- 证据：`request.rs:45-51` 明确“默认不重试，只有显式 `.retry()` 才启用”；全仓 `.retry(` 只出现在测试与文档；飞书出站明确不用它（`src/addon/feishu/domain/outbound.rs:10-19`）。同时文档说“熔断尚未实现”，但 `send_guarded` 已接入 `CircuitBreaker`（`request.rs:841-858`）。
- 建议：删除或标记 experimental；修文档漂移。**不要把它当现网风险**——没有生产流量走这条路径。

**D3-3（P2）响应体读取阶段的错误被归类为不可重试的 Client 错误**

- 证据：`read_body_limited` 把 `chunk()` 错误字符串化为 `HttpResponseParseFailed`，category 归 Client，`is_retryable` 只认 Transient；飞书层因此判 `Fatal` 不重试。`send_once` 用 `map_err(HttpRequestFailed)` 绕过已有 `From<reqwest::Error>`，使 `HttpTimeout` 在该路径不可达。
- 建议：读取阶段的超时/连接错误映射为 Transient/Retryable；只把真正的 JSON 解析错误归 Client。

**D3-4（P2）附件搬运全量缓冲，峰值约 200 MiB/附件**

- 证据：框架 body 是 `Option<Vec<u8>>`，`send_once` 每次 clone（`request.rs:889-891`）；飞书附件先整块下载再复制进 multipart Vec；`outbound.rs:604` 的 `transport.send(request.clone())` 还会深拷贝 `raw_body`（50 MiB Vec）。
- 影响：双实例并发上传时约 400 MiB 瞬时峰值，有 OOM 风险。
- 建议：改 `Bytes` + 流式 body；至少去掉 `request.clone()` 的整块拷贝。

**D3-5（P2）熔断器生产未装配，HalfOpen 不限并发探测**

- 证据：`HttpClient::new(30)` 使 `circuit_breaker` 恒为 None（`client.rs:56,74`；`bootstrap.rs:140`）；全仓无装配、无状态迁移日志/指标；HalfOpen 文档明确不限并发探测（`circuit_breaker.rs:17`）。
- 建议：要么按 endpoint 分键并补指标后启用，要么删除配置，避免“看起来有保护”。

**D3-6（P2）出站可观测性断链**

- 证据：框架成功日志 DEBUG、失败日志 WARN，target 是 `yang_base::http`；默认 filter 丢弃 WARN（`src/config/mod.rs:853-856`）。飞书只有 `feishu_outbound_request_total{result="failed"}`（`outbound.rs:648-656`），没有成功/耗时/重试/状态码/endpoint。
- 建议：`ok/retry/failed` 三态 + 延迟 histogram + 低基数 endpoint + DNS/connect 分类；接入失败率与 p95 告警。

**D3-7（P2）客户端隐式默认未固定**

- 证据：`proxy_url=None` 的注释与 reqwest 自动读取环境代理的行为矛盾；未配置 redirect 即跟随 10 次，且 301/302/303 会把 POST 降为 GET；scheme 白名单允许任意 `http`。
- 建议：显式 `no_proxy`/代理策略；API 客户端默认 `redirect::Policy::none()`；生产出站 `https_only(true)`。

---

### D4 飞书集成网络行为

**现状与优点**：创建前落 `state=creating + uuid`，uuid 冲突可反查恢复；创建速率桶放 Redis；通知两阶段；租户 token 缓存；事件 worker 有终态/告警。

**D4-1（P1）`Fatal{code:0}` 落终态并写回填列，不可逆**

- 证据：`src/addon/feishu/domain/outbound.rs:352-358` 非 2xx 且无业务码时归 `Fatal{code:0}`（包括网关 HTML、504 等）；`src/addon/feishu/domain/approval_dispatch.rs:1161-1176` 对 `Fatal` 写回填列并返回 `Terminal`；同文件 `:24-25` 明确“回填字段写进去就不可逆，该行再也不会被扫到”。
- 影响：一次网关抖动可能把“系统繁忙”写成“配置错误”，该记录永久退出扫描；用户必须手动清空那一格才能重试。
- 建议：无业务码的非 2xx 改判 `Retryable/Ambiguous`，绝不写回填；只有已知配置码才终态；补单测“code 0 不写回填”。

**D4-2（P1）事件回写失败仍 `mark_processed`**

- 证据：`src/addon/feishu/domain/approval_dispatch.rs:953-957` `write_extended` 返回 `()`，内部失败只 `warn`；`src/infrastructure/feishu_event_worker.rs:631` 直接调用、`:666` 返回 `Processed`、`:249-251` 标记 `processed_at`。
- 影响：事件已 ack 给飞书，但多维表格状态列没更新；系统不会再重试，形成静默数据分叉。
- 建议：`write_extended` 返回 `Result`；失败返回 `EventOutcome::Failed` 并走有上限重试；成功才 `mark_processed`。

**D4-3（P1）同步派发把 50 MiB 附件链路塞进 60s 入站预算**

- 证据：`src/addon/feishu/approval/actions/dispatch.rs:1415` 在入站 handler 内直接 `await dispatch_one`；附件链路每个文件先下载后上传，各带 120s 超时且 `PULL_RETRY` 三次（`src/addon/feishu/domain/approval_attachment.rs:188/209/130`），单文件上限 50 MiB（`src/addon/feishu/domain/widget_rules.rs:92`）；入站全局预算是 60s（`src/config/mod.rs:165` + `src/bootstrap.rs:380`）。任务行要等 `handle_dispatch` 返回后才写入（`dispatch.rs:1467` 起），超时取消后 worker 不会接手。
- 影响：用户看到 408，审批单可能已创建，但任务行不存在；无法自动恢复。
- 建议：入队优先，返回 202；单条同步仅保留为兼容入口；统一由 worker 执行与记录。

**D4-4（P1）`user_token` 刷新成功但落库失败会烧掉 refresh_token**

- 证据：`src/addon/feishu/domain/user_token.rs:721-726` 先 `refresh_tokens`，再 `update_tokens`；后者失败直接返回错误，新 refresh token 丢失，旧 token 已被轮换（官方 refresh token 一次性）。
- 影响：用户永久失去自动续期能力，只能重新授权；错误还被归成可重试的 `UserTokenError::Failed`，掩盖了不可恢复性。
- 建议：刷新结果先写 outbox/事务，成功后才认为刷新完成；落库失败保留新 token 于内存并告警；加对账任务。

**D4-5（P1）审批任务重试无上限，`attempts` 是死字段**

- 证据：`src/addon/feishu/approval/domain/task_table.rs:102` 有 `attempts` 字段；`src/infrastructure/feishu_approval_worker.rs:1265-1310` 的 `record_outcome` 从不递增/读取它；`:1085-1089` 读记录失败一律 `Retryable`，不区分 `Fatal`。
- 影响：坏记录每 30 秒重试一次，永久烧飞书配额与数据库；`attempts` 给人“已限次”的错觉。
- 建议：`record_outcome` 递增 `attempts`；达到上限（如 10）转 `Terminal/Unnotified`；加退避表与 `feishu_approval_retry_exhausted_total`。

**D4-6（P2）共用 challenge URL 时，解密/解析失败返回 200**

- 证据：`src/addon/feishu/event/actions/event_challenge.rs:100-107` 在“解密失败”“解密后非 JSON”时走 `raw_json`；`ResponseBody::raw` 是普通成功响应（默认 200），而 200 是飞书判定投递成功的唯一信号。
- 建议：这两种故障返回 5xx；只有真正的 challenge 才 200。

**D4-7（P2）蓝绿 smoke 阶段线上与绿色实例同时跑完整 worker**

- 证据：`deploy/deploy-blue-green.sh:1227/1231` smoke 阶段起绿容器；同一份 `config.toml` 挂进两个容器（`:325`）；四个 worker 仅受 `can_pull()` 门控且无关闭开关（`src/bootstrap.rs:292/316/338/354`）；事件认领 `processed_at IS NULL`、审批认领 `state=pending`、拉取明确不做跨实例单飞。
- 建议：加 `workers_enabled` 配置，smoke 实例只跑 HTTP/readiness；或让 smoke 使用独立库/只读模式。

**D4-8（P2）事件缺少顶层 uuid 时 `event_id` 落空串，后续同类事件被当成重复**

- 证据：`src/addon/feishu/event/actions/receive_approval_event.rs:175` 取不到顶层 uuid 时用空串；`src/addon/feishu/approval/domain/event_table.rs:33-40` 的 `event_id` 是 required+unique；而 required 只拒绝 null、不拒绝空串（`crates/yang-base/src/table/field_config.rs:145`）。
- 建议：空串直接拒绝/生成确定性哈希；required 校验补空串。

**D4-9（P2）通知去重依赖 `last_error` 字符串与写库前发送**

- 证据：`src/infrastructure/feishu_approval_worker.rs:1244-1252` 用 `last_error == 本轮原因` 判“已通知”；`:1263` 注释承认“发完、还没写库就被杀掉，下一轮会重发”。
- 建议：持久化 `notified_at` 或 `(task_id, outcome, reason_hash)` 幂等键；接受 at-least-once 但加重复计数。

**D4-10（P2）同步派发与 worker 双路径语义漂移**

- 证据：`src/addon/feishu/approval/actions/dispatch.rs:344-351` 公开同步路由；worker 走 claim/record_outcome（`src/infrastructure/feishu_approval_worker.rs:402-489`）。两条路径共享领域函数，但通知、重试、状态管理各有一套。
- 建议：入队优先；单条同步仅保留兼容入口；统一由 worker 执行与记录。

**D4-11（P2）出站只有“最终失败”计数**

- 证据：`outbound.rs:648-656` 只有 `result="failed"`；没有耗时、成功、重试、429、endpoint 维度；事件 worker 只有终态 counter，没有 pending gauge。
- 建议：补 `feishu_outbound_request_total{result}`、`feishu_rate_limited_total`、`feishu_event_pending`、延迟 histogram；接入告警。

---

### D5 数据层网络（MySQL / Redis）

**现状与优点**：sqlx 池默认 `test_before_acquire=true`；Redis 授权版本缓存用 Lua 单调发布；readiness 走独立管理面；池指标已接入 telemetry。

**D5-1（P1）Redis recycle 超时被映射成 300s，且发生在借出时**

- 证据：`crates/yang-db/src/redis/config.rs:225-235` 把 `timeouts.recycle` 映射到 `idle_timeout`；`src/config/mod.rs:250-252` 默认 300s；deadpool 0.12.3 在借出空闲连接时执行 `try_recycle`（UNWATCH+PING），不是归还时。
- 影响：半死 Redis（黑洞/VM 挂起）下每个 `get` 可挂 300s，并逐个空闲对象重试；HTTP 路径被 60s `TimeoutLayer` 截断，但非 HTTP 调用方（outbox 发布、飞书 worker）会吃满 300s。
- 建议：新增独立 `recycle_timeout`（1–2s），与 idle TTL 解耦。

**D5-2（P1）Redis 无认证/ACL、无 `maxmemory`**

- 证据：`deploy/compose.infra.yaml:60-72` Redis 启动命令只有 `--appendonly yes`，无 `requirepass`/ACL/`maxmemory`；`deploy/config.cloud.example.toml:84` 是无密码 URL；Redis 与所有应用容器同处 `yang-system-net`。
- 影响：同网络任意容器可读写/`FLUSHALL` 授权版本缓存、邮箱验证码、Step-up 单次消费；无内存上限可拖垮宿主；Redis 明确不备份。
- 建议：`requirepass` 或 ACL 每应用独立用户；`maxmemory 256mb`；`maxmemory-policy noeviction`（安全状态不能被静默淘汰）；网络策略限制来源；轮换凭据。

**D5-3（P2）MySQL 无语句级超时**

- 证据：`crates/yang-db/src/mysql/database.rs:209-218` 只设置池参数，没有 `after_connect`；全仓无 `max_execution_time/innodb_lock_wait_timeout`。
- 影响：一条失控查询或锁等待可长期占用连接；HTTP 60s 返回 408，但 DB 侧仍在跑；池耗尽后全站失败。
- 建议：`after_connect` 设置 `SET SESSION max_execution_time=5000`、`innodb_lock_wait_timeout=5`；接线 slow query 指标。

**D5-4（P2）池容量与排队不可观测**

- 证据：`PoolStatus.waiting` 在 MySQL 侧恒为 0（sqlx 无 API，`database.rs:253-266`）；没有 acquire 等待 histogram；HTTP 并发 256 直接压在 20 连接上。
- 建议：不再导出假 0；加 acquire 等待 histogram/告警；可选有界信号量按依赖隔舱。

**D5-5（P2）配置语义与真实行为不一致**

- 证据：`mysql.connect_timeout` 实际下推为 sqlx `acquire_timeout`（含等槽+探活+建连），`config.show` 注释写“建立新连接的超时”；Redis 的 `min_connections/max_lifetime/test_before_acquire` 是 no-op，但样例仍以 2/1800/true 作默认，`docs/contracts/CONFIGURATION.md:171` 还承诺 Redis 也受 `max_lifetime` 约束。
- 建议：重命名 `acquire_timeout`；Redis 三个无效字段标注为 no-op 或删除；文档同步。

**D5-6（P2）授权版本缓存 5s + Redis 故障回源放大**

- 证据：`src/infrastructure/authorization/version_cache.rs:6` TTL 5s；`src/infrastructure/authorization/request_validator.rs:90-134` Redis miss/error 降级查 MySQL；HTTP 并发 256。
- 影响：Redis 故障时每个请求回源 MySQL，256 并发可形成风暴；5s TTL 本身也造成周期性回源。
- 建议：按用户 single-flight；短负缓存/熔断；版本发布时主动写缓存；加 `auth_version_fallback_total`。

**D5-7（P2）readiness 串行探针共用业务池**

- 证据：`crates/yang-base/src/tools.rs:203-232` 先 await MySQL，再 await Redis，串行；探针与业务共用连接池。
- 影响：一个慢依赖吃掉整个预算；过载时探针也失败，形成“越忙越 not-ready”。
- 建议：探针并发 + 单探针超时；独立轻量连接；结果短缓存。

**D5-8（P2）缺少 Redis/MySQL 服务端指标**

- 证据：`ops/prometheus/yang-system.rules.yml` 只有应用侧连接池 gauge；没有 Redis `used_memory/evicted_keys`、MySQL `Threads_connected/InnoDB 锁等待/慢查询`。
- 建议：补 exporter 或 `INFO`/`SHOW GLOBAL STATUS` 采集；容量与内存故障不能只靠池饱和间接推断。

**D5-9（backlog）Redis TLS/response_timeout**

- 证据：`redis` crate 未启用 TLS feature；`MultiplexedConnection` 的 `response_timeout` 为 None。
- 说明：当前 `redis://127.0.0.1` 同宿主机部署不受影响；跨主机/托管 Redis 前再做。

**保持现状**：`test_before_acquire=true` 不建议关闭。它多一个 RTT，但换来 MySQL 重启后的透明自愈；同宿主 RTT 约 0.1ms，收益可忽略。

---

### D6 安全信任边界

**现状与优点**：受信代理解析器按从右向左剥离、限制 header 大小/跳数/CIDR 数量；BrowserSession 同源校验；refresh Cookie Host-only/HttpOnly/SameSite=Strict；access token 只存内存；CORS 通配+credentials 构建期拒绝；响应头/文件名注入防护。

**D6-1（P1）`log_inbound_requests` 开启时明文记录 Token**

- 证据：`src/config/mod.rs:644-655` 注释明确“开启期间 stdout 会含数据源 Token 与多维表格管理 Token 的明文”；`src/addon/feishu/domain/request_log.rs:110,273,297` 记录 method/path/query/全部 headers/完整 body；`src/addon/feishu/option/actions/mod.rs:33-72` 只覆盖三个机器入口。
- 影响：若生产为 true，日志采集与保留系统会长期保存凭据；与 `docs/contracts/OBSERVABILITY.md` 的禁令相反。历史运维记录称两台生产机为 true，但本次未读取 `config.cloud*.toml`，**状态待人工核对**。
- 建议：`environment=production` 且开关为 true 时启动失败（除非显式 `-AllowInsecureRequestLog`）；或只记录结构、脱敏 token/authorization；上线前逐台确认。

**D6-2（P2）公开 options/dispatch 端点没有边缘限流**

- 证据：`approval_options` 是 `.public()`，按用户可控 `source_key` 先查两次 DB 再验 token；`dispatch_approval` 也是 `.public()`（`src/addon/feishu/approval/actions/dispatch.rs:351`），先按 config_key 查库再验 token；`frontend/deploy/nginx.conf:43-51` 无 `limit_req`。
- 影响：匿名流量可占连接池、爆破 token；token 泄露后无边缘闸门。
- 建议：对 `/api/v1/feishu/approval/options/` 与 `.../dispatch/` 加 `limit_req`；按 config_key 做应用层速率桶；保持常量时间比较。

**D6-3（P2）密码重置令牌放在查询串**

- 证据：`src/addon/account/domain/email_delivery.rs:145` 生成 `{base}/reset-password?token=<raw>`；`frontend/deploy/nginx.conf` 整份替换 `/etc/nginx/nginx.conf` 且没有 `access_log`，落到默认 combined，官方镜像把 access.log 链接到 stdout。
- 影响：token 进入 docker logs 与浏览器历史；虽然一次性、TTL 900s、只存摘要、Referrer-Policy no-referrer 限制了窗口，但日志留存通常远超 15 分钟。
- 建议：改 POST body 或 URL fragment；nginx access log 对 reset 路径脱敏。

**D6-4（P2）OAuth state 存在进程内 HashMap**

- 证据：`src/addon/feishu/domain/user_token.rs:176-195`；mint 在 `src/addon/feishu/approval/actions/oauth_authorize_url.rs:74`，consume 在 `src/addon/feishu/approval/actions/oauth_callback.rs:94`；蓝绿 smoke 双实例同时运行。
- 影响：state 在 A 实例签发、回调命中 B 实例即失败；重启会丢失 10 分钟内的 state，错误文案还误导为“链接过期”。
- 建议：Redis 存 state（TTL + 一次性消费）；或部署层强制单实例并写进契约。

**D6-5（P2）TLS 边缘落地后 `trusted_proxy_cidrs` 会反向退化**

- 证据：按 `deploy/README.md:315-320` 让宿主边缘覆盖 XFF 后，容器 nginx 再用 `$proxy_add_x_forwarded_for` 追加 Docker 网桥网关地址（`frontend/deploy/nginx.conf:80`），而 `trusted_proxy_cidrs` 仍只有 `127.0.0.1/32`，解析会在网关这一跳停止。
- 建议：TLS 落地时同时补 Docker 网关段（以 `docker network inspect` 实际值为准，如 `172.17.0.0/16`）；部署契约加断言。

---

### D7 可观测性与运行时生命周期

**现状与优点**：TelemetryRuntime、Prometheus 管理面、readiness 预算、ShutdownBudget 分阶段、池指标、Outbox/审批积压告警；Argon2 与 xlsx 解析已用 `spawn_blocking`。

**D7-1（P1）默认日志 filter 丢弃框架层 INFO/WARN；SLI 只统计进入 Action 的请求**

- 证据：`src/config/mod.rs:853-856` 默认 `yang_system=info,tower_http=info,error`；契约承诺的“Action 执行完成”在 `yang_runtime`（`crates/yang-runtime/src/observability/logging.rs:184/201/219`），shutdown/readiness 的 INFO/WARN 也在 `yang_runtime`/`yang_base`；`logging.rs:108-109,227-233` 只有 Action 维度的 counter/duration。
- 影响：生产只剩这两个 crate 的 ERROR；404/408/413/401 等传输层请求不在分母，SLO 失真。
- 建议：filter 加 `yang_base=warn,yang_runtime=info`；在 axum 层加传输级 counter；SLI 区分 edge requests 与 action requests。

**D7-2（P1）业务面 `/health/ready` 公开、无预算、绕过 readiness gate**

- 见 D1-3；复核确认 P1：drain 期间管理面 503 而业务面仍 200，语义相反。

**D7-3（P2）worker fire-and-forget，panic 后静默死亡**

- 证据：`src/bootstrap.rs:406-452` 只按阶段调用 `worker.shutdown()`，没有 `select!` 监听 `JoinHandle`；readiness 不包含 worker 状态；无 `worker_alive` 指标。
- 影响：worker panic/退出后飞书事件与任务静默停止，readiness 仍绿。
- 建议：所有 `JoinHandle` 进 `select!`，失活即摘 readiness/退出进程；补 `worker_alive{worker}` 与 last-success 时间戳。

**D7-4（P2）关闭预算超时是 detach 而非 abort**

- 证据：`crates/yang-runtime/src/shutdown.rs:141-152` 的 `Err(_)` 分支只是 drop phase future；`JoinHandle` drop 仅 detach；feishu worker 的 Drop 只发 watch 信号不 abort（`src/infrastructure/feishu_approval_worker.rs:262-266`、`src/infrastructure/feishu_pull.rs:237-242`），而 `run_loop` 只在轮次之间看信号、单轮最坏 300s。
- 影响：预算耗尽后 worker 仍活到进程退出，且 Tools 已关闭后继续空转刷错。
- 建议：关闭时显式 `abort()` 或把单轮工作切成可取消的短段；让 worker 的 Drop 有真正的 abort 语义。

**D7-5（P2）指标直方图上界只有 10s**

- 证据：`crates/yang-runtime/src/observability/telemetry.rs:32-34` buckets 最大 10.0；`set_buckets` 应用于所有 histogram。
- 影响：导入、审批等 >10s 请求全部落 `+Inf`，p95/p99 与延迟告警在慢请求区间失真。
- 建议：补 30/60/120/300s bucket，或为长任务单独建 histogram。

**D7-6（P2）worker 触发通道无界、轮询无退避**

- 证据：`src/infrastructure/feishu_pull.rs:119-120` 与 `src/infrastructure/feishu_approval_worker.rs:155-157` 用 `unbounded_channel`；`feishu_approval_worker.rs:340` 与 `feishu_event_worker.rs:157` 用 `tokio::time::interval`，默认 `MissedTickBehavior::Burst`；outbox 250ms（`src/config/mod.rs:111-113`），每轮失败都 ERROR（`src/infrastructure/authorization/worker.rs:212-218`）。
- 影响：运维连点/前端重试会无界积压并逐条重放；MySQL 持续故障时约 1.4 万行 ERROR/小时；长轮结束后立即补跑。
- 建议：`mpsc::channel(1)` + `try_send` 合并触发；outbox 指数退避到 5s ±20%；approval/event 用 `MissedTickBehavior::Delay`；ERROR 只在状态变化时打。

**D7-7（P2）失败计数未接告警**

- 证据：`outbound.rs:648-656` 只有 failed counter；审批 worker 有 pending gauge/terminal counter；仓库内无告警规则。
- 建议：对 failed 速率、pending 积压、event terminal 配告警；按症状（失败率/积压）而不是仅日志。

---

### D8 前端网络层

**现状与优点**：React 19 + Vite 8 + TanStack Query 5；跨标签页刷新互斥（`frontend/src/engine/session/refresh-lock.ts:1-45`，`navigator.locks` 优先，非安全上下文退 localStorage 租约 + 心跳）；access token 只存内存，refresh 走 Host-only/HttpOnly/SameSite=Strict Cookie；会话边界 `queryClient.clear()`；multipart 预检与后端上限对齐。

**D8-1（P1）静态资源零压缩**

- 证据：`frontend/deploy/nginx.conf:23-51` 全文无 gzip/brotli；`frontend/deploy/Dockerfile:22-23` 整份覆盖 `nginx.conf` 且不 include `conf.d`；本地实测入口闭包 raw ≈ 0.91 MB → gzip ≈ 0.27 MB（约 3.3×）。
- 影响：每次发版后首次加载多传约 0.6 MB；预算门禁用 `gzipSync` 计算，线上劣化不会被门禁发现。
- 建议：`gzip on; gzip_vary on; gzip_min_length 1024; gzip_types application/javascript text/javascript text/css application/json image/svg+xml;`；部署契约脚本加机械校验。

**D8-2（P1）业务请求没有客户端超时兜底**

- 证据：`frontend/src/engine/http/client.ts:27-31,60-63` 裸 fetch 透传 signal；全仓 `AbortSignal.timeout` 只出现在续期与登出/停用（`frontend/src/engine/session/auth-session.ts:92`、`session-controller.ts:16`，10s）；`frontend/src/shell/auth-gate.tsx:25` pending 时渲染全屏骨架。
- 影响：浏览器到 nginx 的连接被黑洞时骨架长时间无错误、无重试（TanStack 只在失败时重试）；配合 `retry:1` 最坏放大两次长等待。
- 建议：统一包装 `AbortSignal.any([callerSignal, AbortSignal.timeout(ms)])`（按 `auth-session` 的可用性判断留 setTimeout 回退），超时映射为可重试 `ApiError`；按动作类型取 15–30s。

**D8-3（P2）401 透明续期靠逐点手动接入**

- 证据：`frontend/src/features/account/api.ts:50,156,310` 与 `features/account/admin-api.ts:41-51` 裸 fetch；同文件 `:78` 注释宣称透明续期；access TTL 3600s（`config.show.toml:153`）。
- 影响：空闲超过 1 小时后，撤销会话/头像上传/签发重置令牌等路径硬 401，与声明不符。
- 建议：给这 5 处各套一层已有 `requestWithTokenRefresh`（每处约 2 行）；出现第三种请求形态再抽统一出口。

**D8-4（P2）非 JSON 错误被当成协议错误**

- 证据：`frontend/src/engine/http/http.ts:16-25` 解析失败直接抛 `ContractError`；`action-response.ts:64-74` 与 `client.ts:40-48` 均先 `parseJson` 后看 `ok`；2xx 空 body 时 `envelope?.code !== 0` 为 true。
- 影响：nginx 的 413/502/504 HTML 页显示“服务端返回了无效 JSON”，status/requestId 全丢；204 被当失败。
- 建议：解析失败按 status 构造 `ApiError` 并保留正文前 200 字符/requestId；2xx 空 body 视为成功；`ContractError` 只留给 schema 不符。

**D8-5（P2）启动恢复把可重试失败当已登出**

- 证据：`frontend/src/engine/session/session-controller.ts:98-100` catch 全部 `commit anonymous`；`auth-session.ts:115-119` 下层已区分非终态并 rethrow；`use-session.ts:49-55` 只在 mount 调一次。
- 影响：启动瞬间一次网络抖动就把仍持有效 Cookie 的用户静默丢到登录页，只有整页刷新能恢复。
- 建议：仅终态（`undefined`）才 `commit anonymous`；非终态短退避重试一次，仍失败展示“网络异常，重试”并监听 `online` 事件。

**D8-6（P2）TanStack 默认值造成请求放大**

- 证据：`frontend/src/shell/App.tsx:24-25` 只有 `retry:1`；`features/account/api.ts:139,427` 与 `engine/catalog/use-catalog.ts:20` 的 queryKey 含 access token；`engine/renderers/table/use-table-query.ts:36-52` 未设 `staleTime`。
- 影响：窗口聚焦集中重取；403/404 也重试两次；token 每小时轮换使 me/security-events 多一次请求与缓存副本。
- 建议：全局 `staleTime 15s` + retry 谓词（仅网络/5xx/408/429）；queryKey 改固定键；身份隔离仍靠 `queryClient.clear()`。

**D8-7（P2）`redirect:manual` 在浏览器里是死分支**

- 证据：`frontend/src/engine/http/action-request.ts:126` 用 `manual`；`action-response.ts:22-33` 读 `location`；浏览器对 `manual` 返回 `opaqueredirect`（status 0、headers 不可读），`response.headers.get('location')` 恒为 null；`use-presented-actions.ts:165` 无 location 不跳转。
- 影响：任何 `response_kind=redirect` 的 Action 都是“成功但页面不动”；当前业务未使用，属潜在缺陷。
- 建议：改为 200 JSON 信封携带目标 URL，渲染层按字段 `navigate`；不要用 `redirect:follow`。

**D8-8（P2）`client_max_body_size 16m` 与后端精确撞墙**

- 证据：`frontend/deploy/nginx.conf:51` 16m == 后端 `max_body_bytes=16777216` == multipart 契约 `max_total_bytes=16*1024*1024`（`src/addon/feishu/datasource/actions/import_xlsx.rs:176-177`）；multipart boundary/part 头开销必然把“恰好 16 MiB 的文件”顶过上限；前端预检只按纯文件字节放行。
- 建议：nginx 提到 `17m`，或前端预检预留 multipart 开销；错误页返回 JSON。

---

## 四、横向主题

1. **信任边界只做了一半**：解析器算法正确，但 TLS 缺位、`Forwarded` 可伪造、日志可泄凭据；三者叠加时“按 IP 限流/审计”整体不可信。
2. **超时预算没有单一链**：nginx 非活动超时（不是总时长）、应用 60s、Docker 10s、前端无超时、Redis recycle 300s、MySQL 无语句超时；每一层都假设自己不是最短的板。
3. **过载与背压缺位**：并发上限只排队不卸载、边缘无 `limit_req`、无 in-flight/queue 指标、worker 触发通道无界。过载时系统不是快速拒绝，而是无声堆积。
4. **不可逆输出与 ack 语义**：`Fatal{code:0}` 写回填、事件回写失败仍 ack、user_token 刷新落库失败丢 refresh token。共同点是“把不确定当确定”，且都没有自动解除通路。
5. **多实例状态假设不成立**：smoke 双实例、OAuth state 在内存、user_token 刷新锁在进程内、事件认领无租约；代码注释里的“单实例”保证已被部署脚本自己打破。
6. **可观测性分母缺失**：SLI 只统计进入 Action 的请求；默认 filter 丢弃 `yang_runtime/yang_base` 的 INFO/WARN；传输层 408/413/429 不计入失败；直方图上界 10s。
7. **交付与预算口径脱节**：预算按 gzip 算、线上按 raw 传；一次修复即可回收约 2/3 首屏字节。
8. **配置与文档漂移**：模板与实际部署不一致（`trusted_proxy_cidrs`）、注释与实现相反（RetryConfig 熔断、Redis recycle“归还时”）、能力未启用却被文档描述为已启用。
9. **发布生命周期未闭环**：蓝绿只换镜像不换 schema；前端 netns 依赖人工自愈；“部署完成”不覆盖数据库兼容性。
10. **依赖组件安全默认缺位**：Redis 无认证、无内存上限，却承载安全状态；这是“内网即安全”的典型误设。

---

## 五、30/60/90 路线图

### 30 天：quick wins（按依赖排序）

| 顺序 | 项 | 验收 |
|---|---|---|
| 1 | 宿主 TLS 仅放行 options 路径 + `proxy_set_header Forwarded ""` | HTTPS options 200；伪造 Forwarded 不改变限流键 |
| 2 | 全量 TLS 反代后再 `EdgeBindAddr 127.0.0.1` | 公网 18654 不可达；HTTPS 全站可用；飞书 options 仍可达 |
| 3 | nginx gzip + 部署契约校验 | `content-encoding: gzip`；首屏字节下降 ≥60% |
| 4 | nginx 显式超时 + 上传/下载 buffering 分流 | 上传/下载/JSON 各自预算；无 60s 误杀 |
| 5 | `log_inbound_requests` production fail-fast + 两台机人工核对 | 配置为 true 启动失败；生产确认为 false |
| 6 | `Fatal{code:0}` → Retry；补单测“code 0 不写回填” | 网关抖动不写终态 |
| 7 | `write_extended` 返回 Result；事件成功才 ack | 写失败进重试，不 `mark_processed` |
| 8 | `docker stop -t 40` | 关闭日志完整，无 SIGKILL |
| 9 | Redis recycle 独立 1–2s | 半死 Redis 下单次 get ≤2s |
| 10 | Redis `requirepass`/ACL + `maxmemory 256mb` + `noeviction` | 无密码连接失败；内存有上限 |
| 11 | MySQL `max_execution_time` + `innodb_lock_wait_timeout` | 长查询 ≤5s 被杀；锁等待 ≤5s |
| 12 | 前端统一超时 | 断网 30s 内出现可重试错误 |
| 13 | 默认 filter 加 `yang_base=warn,yang_runtime=info` | 规范事件与失败日志落盘 |
| 14 | 仅 JSON 路由 1 MiB，multipart 保持 16 MiB | 登录 1 MiB 拒绝；xlsx 16 MiB 通过 |
| 15 | 边缘 `limit_req` + burst | 匿名打满时 429 而非拖垮池 |
| 16 | portal 挂 `TrustedClientIpMiddleware` | 门户键含真实 IP |
| 17 | 前端 netns 常驻探针/自愈 | backend 重启后 edge ≤60s 自愈 |
| 18 | `mark_ready` 移到 bind 成功之后；业务 `/health/ready` 边缘 404 | bind 失败不 ready；匿名探针不打 DB |

### 60 天：结构性改造

- **TLS 完整边缘**：HTTP/2、certbot 续期、30/14/7 天到期告警；TLS 就绪后切 `production`；补 Docker 网关 CIDR。
- **派发入队化**：先落任务行返回 202，复用审批 worker 的 claim/record_outcome，消除同步/worker 双路径语义漂移。
- **审批重试闭环**：`attempts` 递增、上限 10、死信/告警；读记录区分 Fatal/Retryable。
- **user_token 刷新补偿**：刷新结果先持久化/outbox，失败保留内存并告警。
- **蓝绿 schema 门禁**：expand-contract 兼容窗口、迁移前备份、破坏性 DDL 拦截、rollback 剧本与演练。
- **worker 监督**：JoinHandle 进 `select!`，失活即摘 readiness/退出；补 `worker_alive` 与事件积压 gauge。
- **传输层指标与 SLI**：薄中间件 + OTel 语义；SLO 规则改用 edge counter。
- **容器运行面**：`--memory/--cpus` 上限、json-file 轮转、compose limits。
- **出站可观测性**：outbound histogram + ok/retry/failed 三态计数 + DNS/connect 分类。
- **前端续期收敛**：5 处裸 fetch 套刷新通道；request 层统一超时。
- **readiness 探针并发化**：独立连接、单探针超时。
- **auth 版本缓存**：single-flight + 短负缓存 + 回源指标。

### 90 天：可选未来项

- **多实例就绪**：仅当确定要并行多活时，事件/任务改 `SELECT ... FOR UPDATE SKIP LOCKED` 租约、OAuth state 与 user_token 刷新锁改 Redis。
- **附件零拷贝**：`Bytes` + `Body::wrap_stream`。
- **直方图 buckets 扩到 300s**。
- **gzip_static**：确认镜像模块后加预压缩，省 CPU。
- **schema 版本化/expand-contract 自动化**。
- **Redis TLS/response_timeout**：跨主机/托管 Redis 前再做。

---

## 六、明确不建议做

- **Kubernetes / 服务网格 / 新数据库 / Redis Cluster/Sentinel**：单实例模块化单体的证据不支持，运维成本远大于收益。
- **为熔断器补指标后强行启用**：唯一上游 + 有界重试已覆盖大部分 fast-fail；优先删除死配置，避免“看起来有保护”。
- **为零停机引入编排器**：先用 `workers_enabled` 与 `docker stop -t` 消除强杀；宿主 nginx 双 upstream 可延后，先量化停机窗口是否消耗真实 SLO。
- **关闭 `test_before_acquire`**：同宿主 RTT 约 0.1ms，收益可忽略，还会失去 MySQL 重启后的透明自愈。
- **全局把 JSON body 降到 1 MiB**：会直接打断 xlsx 导入；只对 JSON 路由降。
- **做 per-action body 上限配置系统**：先按路由把 JSON 降到 1 MiB，确有大批量 Action 再加覆盖。
- **进程内 LRU 缓存 `source_key → token_hash`**：把凭据常驻内存换一个 P2 的缓解，不划算；边缘 `limit_req` 足够。
- **在 TLS options 入口就绪前收回 18654 loopback**：飞书审批“关联外部选项”要求公网可访问，先收回会打断现网功能。

---

## 七、成熟系统对照（参考基线）

| 系统/文档 | 可借鉴的实践 | 对应发现 |
|---|---|---|
| axum 0.8 / tower-http | per-route body limit、graceful shutdown、Timeout/Trace 语义 | D1-2、D1-4、D1-7、D1-8 |
| tower `ConcurrencyLimit` | 语义是排队而非拒绝；过载卸载要用 load-shed/边缘限流 | D1-4 |
| nginx `gzip` / `proxy_buffering` / `limit_req` | 静态压缩、上传/下载 buffering 分流、边缘限流 | D1-6、D2-9、D8-1 |
| reqwest `ClientBuilder` | `connect_timeout` + `read_timeout` + 显式 redirect policy | D3-1、D3-7 |
| deadpool / redis-rs | recycle 超时独立、ACL、`response_timeout` | D5-1、D5-2、D5-9 |
| sqlx `MySqlPoolOptions` | 池 sizing、acquire timeout、statement timeout | D5-3、D5-4 |
| 飞书开放平台 | 频率限制、事件订阅 3 秒 ack、refresh_token 一次性 | D4-1、D4-2、D4-4、D4-5 |
| OWASP ASVS / RFC 9700 | TLS、Cookie、OAuth state、公网端点限流 | D2-1、D6-2、D6-4 |
| Kubernetes probes / RED/USE | readiness 语义、SLI 覆盖拒绝/排队/超时 | D1-3、D7-1、D7-2 |
| TanStack Query v5 defaults | staleTime 0 + focus 重取的放大效应 | D8-6 |

参考链接：

- axum: https://docs.rs/axum/0.8.9/axum/fn.serve.html
- tower-http: https://docs.rs/tower-http/0.6/tower_http/
- nginx gzip: https://nginx.org/en/docs/http/ngx_http_gzip_module.html
- nginx proxy: https://nginx.org/en/docs/http/ngx_http_proxy_module.html
- nginx limit_req: https://nginx.org/en/docs/http/ngx_http_limit_req_module.html
- reqwest: https://docs.rs/reqwest/0.12.16/reqwest/struct.ClientBuilder.html
- deadpool: https://docs.rs/deadpool/0.12.3/deadpool/struct.Timeouts.html
- sqlx: https://docs.rs/sqlx/0.8.6/sqlx/mysql/struct.MySqlPoolOptions.html
- Redis security: https://redis.io/docs/latest/operate/oss_and_stack/management/security/
- Docker stop: https://docs.docker.com/reference/cli/docker/container/stop/
- 飞书频控: https://open.feishu.cn/document/server-docs/api-call-guide/frequency-control
- 飞书事件订阅: https://open.feishu.cn/document/server-docs/event-subscription-guide/overview
- OWASP ASVS: https://owasp.org/www-project-application-security-verification-standard/
- RFC 9700: https://www.rfc-editor.org/rfc/rfc9700
- Kubernetes probes: https://kubernetes.io/docs/tasks/configure-pod-container/configure-liveness-readiness-startup-probes/
- OpenTelemetry HTTP metrics: https://opentelemetry.io/docs/specs/semconv/http/http-metrics/
- TanStack Query defaults: https://tanstack.com/query/latest/docs/framework/react/guides/important-defaults

---

## 八、方法与局限

- **静态审计 + 上游源码逐行核对**：axum 0.8.9、tower 0.5、tower-http、sqlx 0.8.6、deadpool 0.12、reqwest 0.12.16、redis 0.32。
- **未做动态压测**：并发/内存/超时结论来自代码与配置推导；4 GiB、200 MiB 等为模型上界而非实测峰值。
- **未读取生产配置**：`deploy/config.cloud*.toml` 含明文凭据，本次未打开；`trusted_proxy_cidrs`、`environment=test`、`log_inbound_requests` 的现状来自 `deploy/README.md` 与复核记录，**上线前需人工核对**。
- **工作树可能有未提交改动**：个别行号会随重构漂移，语义以文件为准。
- **报告定位**：可发布为审计报告，不是变更授权。任何生产变更仍需按数据库安全规则与发布流程二次确认。

**最简执行建议**：先做 30 天表中的第 1–10 项。它们都是小改动，但直接覆盖了 P0（TLS/Forwarded）、P1（日志泄密、飞书不可逆写、Redis 安全、关停强杀）的主干。结构性改造（schema 门禁、worker 监督、多实例状态）等 quick wins 落地并验证后再排。

# 网络基础设施审计的对抗性复核与执行路线图

- **日期**：2026-10-25
- **复核对象**：`docs/assessments/2026-10-24-network-infrastructure-first-principles-audit.md`
- **代码基线**：`main @ 0adc5ab` + 当前未提交工作树（`src/addon/feishu/domain/*` 约 +1225/−154 行，含未跟踪的 `widget_rules.rs`；框架仓 `D:/code/lib_yang` 干净）
- **方法**：8 个维度逐条代码复核 → 8 路独立对抗反驳 → 内部一致性批评 + 第一性原理框架批评 → 三套路线图（风险优先／关键路径优先／单操作者运维现实）评审合并 → 完整性、技术可行性、执行红队复核
- **边界**：只读静态复核；未做动态压测；未读取 `deploy/config.cloud*.toml` 等生产凭据文件；生产运行值一律标为 UNVERIFIABLE 并给出人工核对命令。**云服务器侧配置（SSH、`config.cloud.toml`、安全组、DNS、证书、shared-gateway）由操作者负责；agent 的代码执行范围仅限仓库代码／模板／文档。**
- **判定口径**：成立＝代码事实与影响链均成立；部分成立＝代码事实成立但影响/定级有条件或被夸大；不成立＝代码事实或因果链错误；不可核＝依赖生产配置或外部系统状态

---

## 一、结论速览

### 1.1 数字

| 项目 | 结果 |
|---|---|
| 逐条判定 | 68/68 条全部有判定 |
| 判定分布 | 成立 47 条，部分成立 21 条，不成立 0 条，过期 0 条 |
| 定级迁移 | 审计 1 个 P0 + 19 个 P1 + 47 个 P2 + 1 个 backlog；复核定级为 1 个 P0 + 6 个 P1 + 40 个 P2 + 21 个 P3 |
| 对抗结论 | 42 条维持、21 条加限定、4 条降级、1 条升格（升格指影响被低估，不是定级上调） |
| 路线图 | 6 个波次、36 个步骤、约 62 人日（单高级开发者），不含发布窗口与人工等待 |

**定级迁移矩阵**：

| 审计＼复核 | P0 | P1 | P2 | P3 |
|---|---:|---:|---:|---:|
| P0 | 1 | 0 | 0 | 0 |
| P1 | 0 | 6 | 13 | 0 |
| P2 | 0 | 0 | 27 | 20 |
| backlog | 0 | 0 | 0 | 1 |

结论不是「审计大面积错误」，而是：**代码事实基本扎实，定级与因果链系统性偏高一档**。审计把「代码里有这个缺口」直接当成「现在就会造成这个损失」，没有区分触发条件、可恢复性、可检测性和生产运行值是否已核实。反过来，审计漏掉的几类问题（无修复通路、无恢复点、匿名写库、种子任务重复拉取、财务机第二实例）比它自己的部分 P1 更接近「真正会出事」。

### 1.2 审计说对了的五件事

1. **P0 明文边缘**（D2-1）：仓库内确实没有 TLS；`deploy/README.md` 自述当前线上是 `0.0.0.0:18654` 明文；登录凭据、会话 Cookie、重置令牌、OAuth 回调都在明文上。这是全文唯一无争议的 P0。
2. **Redis recycle 300s**（D5-1）：`idle_timeout_seconds` 被映射成 deadpool 的 `timeouts.recycle`，而 deadpool 0.12.3 在**借出时**执行 UNWATCH+PING；半死 Redis 下最坏不是 300s，而是 300s × 空闲连接数（后台发布可停摆约 100 分钟）。这一条审计的上游源码核对是扎实的。
3. **默认日志 filter 丢框架日志**（D7-1）：`yang_system=info,tower_http=info,error` 让 `yang_runtime`／`yang_base` 的 INFO/WARN 全部丢失；契约承诺的「Action 执行完成」在生产不会落盘。
4. **前端无客户端超时**（D8-2）：业务请求只透传调用方 signal，连接被黑洞时全屏骨架可以无限等。
5. **蓝绿 schema／netns 生命周期未闭环**（D2-3、D2-4）：前端共享后端 netns，后端重启后前端滞留旧 netns；启动期 `sync_table_definitions` 在旧版本仍服务时改共享库，rollback 只换镜像不换 schema。

### 1.3 审计说错或说过头的七件事

1. **D4-1 的 504 例子是反的**：`outbound.rs:351-358` 把 500..=599 归为 `Retry`，504/502/503 根本不会落 `Fatal{code:0}`。真正不可逆的入口是另外两条审计没写的路径：**任意未列入白名单的非零业务码**，以及 **HTTP 200 + 非 JSON body**（被 `approval.rs:160-164` 映射成 `Fatal{code:0}`）。结论方向对，证据链错。
2. **D1-4 说「排队到 408」是错的**：Timeout 层在 ConcurrencyLimit 外层，而 tower 的 Timeout 在 `call` 才起表；等待信号量发生在 `poll_ready`，计时器还没启动。排队请求不会被应用层 408，而是被 nginx 非活动超时或客户端兜底。另外 256 并发是**每个 path+method** 一份信号量，不是全局。
3. **D2-4 的「删列让旧代码 500」方向反了**：schema 同步**永不删列**；删掉 NOT NULL 列会让**新代码** INSERT 报 1364。rollback 启动旧容器时旧定义会再跑一次同步，可能执行 `MODIFY COLUMN`／`RENAME COLUMN`，所以「rollback 不碰 schema」只在脚本层成立，进程层不成立。
4. **D7-7「仓库内无告警规则」是事实错误**：`ops/prometheus/yang-system.rules.yml` 在基线 `cd1c333` 就存在，含 13 条告警且 CI 用 promtool 做正反用例。真正的缺口是 outbound failed 与 event terminal 两条指标没有规则，以及**没有任何采集端部署**。
5. **D8-8 的「nginx 提到 17m」单独不够**：axum 的 `DefaultBodyLimit` 与 `decode_multipart` 的 Content-Length 预检都按**整个请求体**比较 16 MiB，`http.max_body_bytes` 还被 `src/config/mod.rs:947-949` 硬顶在 16 MiB。只改 nginx，请求仍会在后端 413；必须同时抬 `max_body_bytes`／`max_total_bytes`（例如 20 MiB），或让传输层按 payload+64 KiB 放行。
6. **D8-2 的 15–30s 前端超时违反审计自己的「外层＞内层」**：nginx 默认 60s、应用默认 60s，前端 15–30s 会先杀请求；而且把写请求超时映射成可重试，会让 TanStack 重试那些服务端可能已经成功的 POST。正确阶梯是 client 90s ＞ nginx 75s ＞ app 60s，导入/派发单独 330s ＞ 300s ＞ 300s；只有 GET/HEAD 或显式幂等动作允许超时重试。
7. **D2-1 的「只放行 options 路径」会打断现网**：`dispatch`、`approval/event`、`event/challenge`、`oauth/callback`、门户、密码重置、SPA 历史路由都可能是公网入口；只放行 options 会让审批派发、OAuth、重置链接 404。最小暴露集合必须逐路径枚举并验收，不能只测 options 200。

### 1.4 最值得先做的五件事（与审计的 Top-10 不同）

> **先做的一件事**：S01 里优先核对 `[feishu].log_inbound_requests`。历史运维记录与审计原文都称两台生产机为 true；若属实，这是**正在发生**的机器 Token 明文外泄——先关开关、轮换两个 Token、清理已落盘日志，再谈其他。本次未读生产配置，不能把它当成已确认，也不能当成已排除。

1. **S01 只读核对生产事实**（0.5 人日）：两机的实际端口绑定、`trusted_proxy_cidrs`、`environment`、`log_inbound_requests`、Redis 认证、域名/443 可达性。审计的 P0/P1 里至少 5 条的可利用性由这些值决定；不先核，路线图会排错优先级。
2. **修 Redis recycle 为 1–2s**（0.5 人日，S20）：P1，改动最小，直接消除「半死 Redis 让认证/发布挂数分钟到数十分钟」。
3. **修日志 filter + 传输层访问日志**（S11 的 filter 部分 + S22，约 2 人日）：P1，当前超时/断连请求从 SLI 消失，会变成「静默漏报」。
4. **Feishu 不可逆输出的三件套**（S40 + S43 + S44，约 6.5 人日）：`write_extended` 返回 Result、200+非 JSON／未列业务码不再写回填、refresh token 落库失败保留内存副本。审计的证据链错了，但这条类的修复便宜且不可逆损失最贵。
5. **netns 看门狗 + schema 启动门禁**（S33 + S14，约 4 人日）：P1，前者把「全站 000 等人工 refresh」变成 60s 自愈；后者把「旧版本还在服务时改共享库」挡在 DDL 之前。

**P0 仍然只有一个**：明文边缘。它无法在没有域名/DNS/443 的前提下关闭；如果没有域名，S02 的宿主白名单只是**临时缓解**，不能写成「P0 已关闭」。

### 1.5 一句话结论

审计的**方向**是对的，**骨架扎实、五个系统性缺口**这个判断也大体成立；但它的**定级、因果链和「最值得先做」排序**需要按本报告的 68 条复核结果重排。真正要修的不是五个并列缺口，而是三类：**没有控制的证明**（源码默认值不等于运行时事实）、**没有修复通路**（不可逆写之后没有对账/requeue）、**没有恢复点**（唯一事实源无自动备份）。路线图应按这三类组织，而不是按审计的 P 标签逐条做。

---

## 二、复核方法与判定口径

### 2.1 为什么不能直接信审计的 P 标签

审计的定级方法是「发现了这个缺口 → 给它一个 P 级」，缺少三个乘数：

- **触发可达性**：需要匿名公网、已认证用户、运维动作，还是容器立足点？
- **可恢复性**：人工清一格、重新授权、重新点一次就能恢复，还是永久退出扫描且无对账？
- **可检测性**：有指标/告警/日志，还是完全静默？
- **证据置信度**：源码事实、配置文件、文档转述、历史运维记忆，还是模型上界？

本报告的复核定级按「触发可达性 × 影响 × 可逆性 × 可检测性」重算，并给每条标注证据等级。因此会出现「代码事实成立但降为 P2/P3」的情况，也会出现「P2 但必须进第一批修」的情况——**定级低不等于可以不做**，它只表示不是「现在就会造成不可逆损失」。

### 2.2 证据等级

| 等级 | 含义 | 本报告的处理 |
|---|---|---|
| source | 当前工作树源码／上游 crate 源码逐行核对 | 可作为 P0/P1 的依据 |
| config_file | 仓库内模板／show 配置（非生产） | 只能证明默认值，不能证明线上 |
| doc | README／契约／设计文档的转述 | 只能作为线索，必要时标 UNVERIFIABLE |
| operator_memory | 历史运维记录／上一轮会话的记忆 | 只作假设，必须人工复核 |
| model_bound | 由代码推导的上界（4 GiB、200 MiB 等） | 只作容量模型，不作出事概率 |

### 2.3 本次复核的三个硬限制

1. **未读生产配置**：`deploy/config.cloud*.toml` 含明文凭据，本次未打开。凡是依赖 `trusted_proxy_cidrs`、`environment`、`log_inbound_requests`、实际端口绑定的结论，都标 UNVERIFIABLE，并给出不泄密的人工核对命令。
2. **未做动态压测**：4 GiB、200/400 MiB、1.4 万 ERROR/小时都是模型上界；路线图把「实测」单列为 S59，不与修复混在一起。
3. **工作树有未提交改动**：审计声称基线是 `cd1c333`，但它引用的 `widget_rules.rs:92` 在 `cd1c333` 根本不存在（`git cat-file` 失败），说明它实际读了未提交工作树。本报告按当前工作树核对；Feishu 相关行号会随在制改动漂移。

### 2.4 审计自身的方法缺陷

- **把条件句写成事实**：D2-2 依赖 `trusted_proxy_cidrs` 含 `127.0.0.1/32`，D2-6 同一份文档又说该值未核实；D6-1 的生产开关值也未读，却进了「最值得先做」第 2 位。
- **把「代码存在」当成「运行时会执行」**：D1-2 的 `DefaultBodyLimit` 对 JSON 路径是惰性的（handler 自己 `to_bytes`），照审计改会得到一个静默 no-op。
- **把「一个维度的 P1 数量」当成「维度评分」**：D4 列了三条 P1 却评「中」，D2 同样三条 P1 却评「弱」；标签和发现清单是两条独立叙述。
- **完整性检查停在发现层**：8 维并行 + 完整性批评者能发现「少读了一个文件」，但发现不了「整类不变量没被列入」（恢复、静态秘密、容量、制品身份、对账、保留期、时钟）。
- **告警事实错误**：D7-7 说仓库无规则，实际有 13 条且被 CI 校验；用错前提得出（碰巧仍正确的）「弱」结论，下一轮复核无法区分「已验证」与「碰巧对」。

---

## 三、逐条判定（68 条）

> 读法：`审计` 是原文定级，`复核` 是本报告重算后的定级。`部分成立` 不等于不重要，只表示原文的因果链或影响被夸大。代码路径相对应用仓 `D:/code/lib_yang/project/yang-system`；`crates/` 相对框架仓 `D:/code/lib_yang`。

### 3.1 D1 入站 HTTP 服务端（9 条）

| ID | 审计 | 复核 | 判定 | 核心证据与结论 |
|---|---|---|---|---|
| D1-1 | P1 | P1 | 成立 | `src/addon/feishu/portal/mod.rs:54-58` 只挂 `TokenAuthMiddleware`；全仓只有 `src/addon/account/user/mod.rs:62-64` 挂 `TrustedClientIpMiddleware`。门户限流键因此恒为 `portal:query:{portal_id}:127.0.0.1`，是**每个门户一个共享桶**，不是「全站单桶」。修复必须同时满足：`trusted_proxy_cidrs` 非空，且先做 D2-2 的 `Forwarded` 清洗，否则新键可被伪造。 |
| D1-2 | P1 | P1 | 部分成立 | 16 MiB JSON 上限属实；但「256 × 16 MiB ≈ 4 GiB」的 256 是**每个 path+method** 一份信号量（`axum.rs:318-325` + tower `ConcurrencyLimitLayer`），不是全局；且 axum 的 `DefaultBodyLimit` 对 JSON 路径惰性（handler 在 `axum.rs:560-589` 自己 `to_bytes`），只改它会得到 no-op。真正要改的是 `to_bytes` 的限额与配置项。约 25–35 LOC。 |
| D1-3 | P1 | P2 | 成立 | `frontend/deploy/nginx.conf:68` 把 `/health` 整段反代；`axum.rs:400-422` 每次匿名请求都执行 MySQL `SELECT 1` + Redis `PING`，无预算、无缓存、无鉴权。降为 P2 的理由：每次探测成本很低，nginx 默认 60s 读超时封顶；真正的放大器是缺少边缘限流（D1-9）。修法：边缘 `location = /health/ready { return 404; }`，管理面 9090 保留。 |
| D1-4 | P2 | P2 | 部分成立 | 层序确实是 ConcurrencyLimit 在内、Timeout 在外；tower 的 Timeout 在 `call` 才起表，等待信号量发生在 `poll_ready`，**排队不会产生 408**，只会挂到 nginx 或客户端放弃。256 也是 per-route。修法：tower `GlobalConcurrencyLimitLayer` + `LoadShed` + `HandleError` 映射 503，并豁免 `/health/*`。 |
| D1-5 | P2 | P2 | 成立 | 全仓无 `Router::fallback`／`method_not_allowed_fallback`；404/405 空体、408 空体，均无 `x-request-id`。408 按 RFC 9110 语义也不对，服务端处理超时应为 503/504。约 30–40 LOC。 |
| D1-6 | P2 | P2 | 成立 | `nginx.conf:68-84` 只设 Host／X-Forwarded-*／proxy_pass，无 connect/send/read 超时、无 buffering 分流。`proxy_read_timeout` 是**非活动超时**，不是总时长——审计这一点纠正得对。导入路由要单独 `proxy_request_buffering off; client_body_timeout 300s; proxy_read_timeout 300s;`，且 location 优先级必须能覆盖通用 regex。 |
| D1-7 | P2 | P2 | 成立 | `axum.rs:335` 用 `TraceLayer::new_for_http()`，默认 span/on_response 是 DEBUG；`tower_http=info` 是空指令，不会产生访问日志。另外默认 404/405 根本没被 TraceLayer 包住。要同时改 TraceLayer 级别 + 注册 fallback。 |
| D1-8 | P2 | P3 | 部分成立 | `bootstrap.rs:372` 先 `mark_ready()`，`axum.rs:200` 才 bind；但 bind 失败会立刻退出进程，窗口是亚毫秒级，不存在持续「ready 但不监听」。降为 P3；若要做，需给框架加 `serve_listener_with_shutdown`。 |
| D1-9 | P2 | P2 | 成立 | `frontend/deploy/nginx.conf` 全文无 `limit_req`／`limit_conn`。审计给的 `limit_conn yang 50;` 复用了 `limit_req_zone` 的名字，`nginx -t` 会失败；必须另声明 `limit_conn_zone`。限流只应加在 `/api` location，不能加在 server 级，否则静态资源突发会被误伤。 |

**D1 遗漏（复核新增）**：

- **P2 并发上限是 per-route，不是全局**：`config.show.toml:58` 把 `max_concurrency` 写成「全局并发请求上限」，实际每个 path+method 各一份信号量，资源耗尽保护比文档弱。
- **P3 `DefaultBodyLimit` 对 JSON 惰性**：照审计的 D1-2 修法会静默无效；真正入口是 `to_bytes`。
- **P3 `portal_metadata` 是未限流的匿名 DB 查询**：`src/addon/feishu/portal/actions/portal_metadata.rs:64-76` 没调用 `rate_limit::check_query`，与 query/suggest 不一致。

### 3.2 D2 边缘与部署网络（12 条）

| ID | 审计 | 复核 | 判定 | 核心证据与结论 |
|---|---|---|---|---|
| D2-1 | P0 | P0 | 成立（条件） | `deploy/README.md:34,280-302` 自述 TLS 未配置、当前 `0.0.0.0:18654` 明文、`environment=test`；`frontend/deploy/nginx.conf:46` 只有 8081，无 SSL。**前提是运行时确实公网绑定**；若 `docker inspect` 显示 `HostIp=127.0.0.1`，则降为 P3（仅缺 TLS 规划）。人工核对是第一步。 |
| D2-2 | P1 | P2 | 部分成立 | `nginx.conf:79-82` 不删除客户端自带的 `Forwarded`；`client_ip.rs:159-163` 在对端受信时优先解析它。但「绕过门户口令限速」不成立：portal 没挂受信中间件，根本不解析 Forwarded（D1-1）。可利用性取决于 `trusted_proxy_cidrs` 是否含 `127.0.0.1/32`；README 把填写安排在 TLS 之后，暗示现在可能为空。修法：边缘 `proxy_set_header Forwarded "";`，XFF 由**最外层**覆盖，容器 nginx 继续追加。 |
| D2-3 | P1 | P1 | 成立 | `deploy-blue-green.sh:330-333` 前端 `--network container:<backend>`；脚本自己记录 backend 重启后前端滞留旧 netns（只剩 lo，连不上 127.0.0.1:8080）；自愈只在 refresh 的健康检查阶段执行一次。前端 HEALTHCHECK 打 SPA（恒 200），滞留时仍 healthy。修法：宿主 watchdog 同时看管理面 readiness 与边缘状态。 |
| D2-4 | P1 | P1 | 部分成立 | 启动期 `sync_table_definitions` 在旧版本仍服务时执行属实；但「删列让旧代码 500」方向反了：schema 同步**永不删列**，删 NOT NULL 列会让**新代码** INSERT 报 1364。真正可达的破坏是 `MODIFY COLUMN`／`RENAME COLUMN`（`schema_sync/plan.rs:101-114,252-260`）；rollback 启动旧容器时旧定义会再跑一次同步，可能把类型改回去或重新加列。修法：把已有只读 API `plan_table_definitions` 接成启动门禁（plan/apply 两档），非加性变更需显式放行。 |
| D2-5 | P1 | P2 | 成立 | `deploy-blue-green.sh:337` 的 `docker stop` 没带 `-t`，Docker 默认 10s SIGKILL；应用关闭预算默认 30s。但多数 drain 远小于 10s，实际损失是有界在途请求，且可重试，降为 P2。注意：单改 `-t 40` 不解决 worker 单轮 300s 的问题（见 D7-4）。 |
| D2-6 | P2 | P2 | 部分成立 | `config.show.toml:327` 是 `[]`，example 模板注释掉；`SecuritySettings::validate` 只校验 CIDR 合法性，没有「production + 空列表」告警。但「真实部署已设 127.0.0.1/32」无法从仓库核实，审计与自己的 §八矛盾。修法：production + 空列表打 WARN（不要 bail）；模板注释写明「空列表 = 全站单 IP 桶」。 |
| D2-7 | P2 | P2 | 成立 | `deploy-blue-green.sh:320-334` 与 `compose.infra.yaml` 均无 `--memory/--cpus/--log-opt`。修法要按实测峰值定，不能直接抄 1g；太小会把概率性 OOM 变成确定性 OOM-Kill。 |
| D2-8 | P2 | P2 | 成立 | cutover 是 stop→start，`deploy-blue-green.sh:1256,1265`；新对健康检查失败只提示手工 rollback（`:1324-1326`）。修法优先做「失败自动起回旧对 + 非 0 退出」，不要先上编排器。 |
| D2-9 | P2 | P2 | 成立 | 全文无显式 `proxy_*_timeout`、`proxy_buffering`、`limit_req`、`log_format`；默认 `access_log` 仍启用（官方镜像通常软链到 stdout），所以「没有访问日志」不成立，「停留在默认值」成立。 |
| D2-10 | P2 | P2 | 成立 | `frontend/scripts/verify-deployment-contract.mjs:137-180` 只读源码默认值，注释明说「不检查运行时环境变量」；`deploy.ps1:121` 与 finance 脚本默认 `auto`，会沿用线上 `0.0.0.0:18654`。修法：cutover 后 `docker inspect` 断言实际 PortBindings，公网且无 TLS 时要求显式 `ALLOW_PUBLIC_EDGE=1`。 |
| D2-11 | P2 | P2 | 成立 | `deploy/README.md:312-318` 只写 TLS 1.2/1.3、重定向、覆盖转发头、安全头；无 HTTP/2、certbot、续期、到期告警。注意：公共 CA 不为裸 IP 签发证书，**域名/DNS 是前置**；安全组 80/443 是否可达需人工核对（历史记录与 README 冲突）。 |
| D2-12 | P2 | P2 | 成立 | `frontend/deploy/Dockerfile:33-34` 只 `wget --spider http://127.0.0.1:8081/`，SPA 恒 200。修法：改打共享 netns 内的管理面 `127.0.0.1:9090/health/ready`；不要打 8081 的 `/health/ready`，否则与 D1-3 的边缘 404 冲突。 |

**D2 遗漏（复核新增）**：

- **P1 财务正式机是同一缺陷的第二个实例**：`deploy/deploy-finance-prod.ps1:4-8` 指向 `47.112.16.98`，同一份 `deploy-blue-green.sh`，默认 `auto`。审计只分析了主测试机；若财务机也是明文，P0 的真实承担者是它。
- **P2 TLS 缺域名/DNS 前置**：仓库里所有地址都是裸 IP（README:346、deploy.ps1:102-103），公共 CA 不签发裸 IP；「四步 TLS」把拿证书当零成本，实际第一步是域名、DNS、ICP 与安全组。
- **P2 只放行 options 会切断 OAuth／重置／派发／事件**：公开路由还包括 `dispatch`、`approval/event`、`event/challenge`、`oauth/callback`、portal、匿名账户 API、SPA 历史路由；按审计的 options-only 白名单会 404。
- **P3 Dockerfile 注释仍称只监听 127.0.0.1:8081**：与 `nginx.conf:46` 的 `listen 8081 default_server` 相反，是 D2-1/D2-10 误判的来源。
- **P3 smoke 失败不清理绿对**：`deploy-blue-green.sh:1237-1243` 只打印 `docker rm -f` 提示，绿对带 `--restart unless-stopped`，会长期占用 8155/9155。
- **P3 客户端可控的 `X-Forwarded-Proto` 被继续转发**：`nginx.conf:33-36,82`；当前 Rust 侧无消费者，属潜伏契约缺陷。

### 3.3 D3 出站 HTTP 客户端（7 条）

| ID | 审计 | 复核 | 判定 | 核心证据与结论 |
|---|---|---|---|---|
| D3-1 | P2 | P2 | 成立 | `crates/yang-base/src/http/client.rs:35-63,264-268` 的 `HttpClientConfig` 只有总超时；生产 `HttpClient::new(30)` 让所有 Feishu 调用共享 30s 总预算。reqwest 0.12.16 的 `connect_timeout`／`read_timeout` 只在 `ClientBuilder` 上，`RequestBuilder` 只有 `timeout`——审计建议的 `.read_timeout()` 位置不可实现。修法：加 client 级 `connect_timeout_secs`／`read_timeout_secs`；注意 connect 超时按解析后的地址生效，不覆盖 DNS。 |
| D3-2 | P2/backlog | P3 | 成立 | `request.rs:45-61` 文档说「熔断尚未实现」，但 `send_guarded` 已接 `CircuitBreaker`；`RetryConfig` 无生产调用方，只有测试与文档。这是死代码 + 文档漂移，不是现网风险；建议文档标注 unused／experimental，删除是 ~300 LOC 破坏性变更。 |
| D3-3 | P2 | P2 | 成立（影响被低估） | `response.rs:192-197` 把所有 body 读取错误映射成 `HttpResponseParseFailed`（Client，不可重试）；`request.rs:924` 用 `map_err(HttpRequestFailed)` 绕过 `HttpTimeout`，使该 variant 在生产不可达。Feishu 对不可重试错误一律判 `Fatal`，在 create/get/definition 路径会写回填并 `Terminal`——一次 body 中断可能永久退掉一条记录。修法：按 `is_timeout/is_connect/is_decode` 分类；**同一提交**里把 `HttpTimeout` 加进 `request.rs:805-812` 的可重试分支，否则框架重试会静默失效。 |
| D3-4 | P2 | P2 | 成立 | 50 MiB 附件同时存在四份拷贝：下载 Vec、multipart Vec、`send_with_retry` 的 request clone、`send_once` 的 body clone；`Vec` 增长还会在 50→64 MiB 时再复制一次，真实峰值约 250–270 MiB/附件。修法：body 改 `bytes::Bytes`（克隆 O(1)），峰值降到约 100–120 MiB；完整流式是 150–250 LOC，且 Feishu 上传可能要求 Content-Length，先不做。 |
| D3-5 | P2 | P3 | 成立 | `HttpClient::new(30)` 使 `circuit_breaker` 恒为 None；全仓无生产装配。更关键：现设计只按 host 分键、只看 HTTP 状态码，对飞书「HTTP 200 + 非零业务码」的 main 失效模式失明；一个 endpoint 生病会打开整个 `open.feishu.cn` 的熔断，连 tenant token 刷新一起断。**不要按审计建议直接补指标后启用**；标记 unused 或先改按 endpoint+业务码记账。 |
| D3-6 | P2 | P3 | 部分成立 | 框架成功日志 DEBUG、失败 WARN，target 是 `yang_base::http`，默认 filter 丢弃；但应用层 `outbound.rs:597-654` 已有 WARN 日志带 endpoint/status/business_code/attempt/duration，target 是 `yang_system::*`，会落盘。真正缺的是**成功分母、延迟 histogram、retry/429 计数**，不是「断链」。修法：在 `send_with_retry` 一次埋点，`result=ok/retry/failed` + 固定 endpoint label + duration histogram；不要用原始 URL 做 label（path 里有 token）。 |
| D3-7 | P2 | P3 | 部分成立 | `client.rs:51-52` 注释说 `proxy_url=None` 不使用代理，但 reqwest 默认 `auto_sys_proxy=true`，代码在 None 时既不 `.proxy()` 也不 `.no_proxy()`；redirect 默认跟随 10 次，301/302/303 会把 POST 降为 GET；scheme 白名单允许 http。当前容器无 `HTTP_PROXY` 环境且所有 Feishu URL 是固定 https host，属潜伏策略风险。修法：显式 no_proxy（可配）、显式 redirect 策略（先验证 Feishu 媒体下载是否 302 到 CDN）、生产 `https_only`。 |

**D3 遗漏（复核新增）**：

- **P2 熔断器对飞书业务码失明且按 host 共享**：见 D3-5；按审计建议「per-endpoint keys + metrics 后启用」仍不够，必须先改记账口径。
- **P2 任意非 Transient 的 `BaseError` 都会被折成 `Fatal`**：`outbound.rs:618-629` 的通用规则比 D4-1 更广；`HttpResponseTooLarge`、`ParamInvalid` 等也会触发不可逆回填。修法应保留回填只给已知业务码，未知传输/库错误一律可重试。
- **P3 URL 脱敏只清 query/fragment，飞书资源标识留在 path**：`request.rs:28-43` 只 `set_query(None)`／`set_fragment(None)`；`file_token`、`app_token`、`table_id`、`instance_id` 都在 path。当前框架日志被 filter 丢弃，一旦抬日志级别就会暴露。
- **P3 50 MiB 附件失败后每轮整包重下载**：上传 `idempotent=false`，轮内重试立即放弃，下一轮 worker 重新下载；与无上限重试叠加会放大带宽与飞书配额。

### 3.4 D4 飞书集成网络行为（11 条）

| ID | 审计 | 复核 | 判定 | 核心证据与结论 |
|---|---|---|---|---|
| D4-1 | P1 | P2 | 部分成立 | `outbound.rs:351-358` 的实际分支是 `200..=299 => None`、`500..=599 => Retry`、`_ => Fatal{code:0}`；**504/502/503 会重试，不会落 Fatal**，审计的例子反了。真正不可逆的入口是：任意未列入白名单的非零业务码（`outbound.rs:324-348` 的 else），以及 **HTTP 200 + 非 JSON**（`approval.rs:160-164` 映射成 `Fatal{code:0}`）。`approval_dispatch.rs:1161-1176` 写回填并 `Terminal`，模块注释自认「写进去就不可逆」。修法：引入 `Ambiguous`／`Unclassified`，绝不写回填；**必须与 D4-5 的 attempts 上限同批**，否则未知错误会 30s 一轮永久重试。约 14 LOC + 测试。 |
| D4-2 | P1 | P2 | 成立 | `approval_dispatch.rs:953-957` 的 `write_extended` 返回 `()`，`:1032-1038` 只 warn；`feishu_event_worker.rs:631` 忽略结果、`:666` 返回 `Processed`、`:249-254` 才 `mark_processed`。补充放大器：`:419-431` 在 `instance_status` 已终态后跳过后续事件；`:638-658` 的 `instance_status` 写入也是 warn-only。修法：`write_extended` 返回 `Result`，失败返回 `Failed` 并受 `MAX_EVENT_RETRY_COUNT=10` 约束；3 秒 Feishu ack 不受影响（ingest 在 worker 之前已 200）。 |
| D4-3 | P1 | P2 | 部分成立 | `dispatch.rs:620-638 → 1415-1460` 在 HTTP handler 内 `await dispatch_one`；任务行只在 `:540` 写，晚于 `complete_feedback`（`:519-529`）。附件链路每文件下载/上传各 120s、`PULL_RETRY` 3 次，最坏 > 360s，而入站 `TimeoutLayer` 60s 会丢弃 future。但「永久不可恢复」的窗口很窄：创建成功后到写回填之间只有 1–2 秒，重新点一次可用 uuid 60012 反查恢复。降为 P2 不代表不用改：任何媒体记录只要传输超 60s 就必然 408。修法：单条也走 enqueue-first 202，任务行先写，再 worker 执行。 |
| D4-4 | P1 | P2 | 成立 | `user_token.rs:721-727` 先 `refresh_tokens`（旧 refresh token 立即失效），再 `update_tokens`；失败被映射成可重试的 `UserTokenError::Failed`，新 token 随局部变量丢弃，下次用旧 token 得到 20073 并转 `ReauthRequired`。概率是 100ms 窗口内的 DB 写失败，后果是用户必须重新授权，降为 P2。审计建议的「刷新前写 outbox/事务」不可实现——拿不到 token 就无法持久化。修法：刷新后短重试落库，失败把新 bundle 放进程内 pending 槽并继续服务；**不要放 Redis**（无认证、无备份）。 |
| D4-5 | P1 | P2 | 成立 | `task_table.rs:102` 的 `attempts` 只被列表页读取，`record_outcome` 从不递增；`available_at` 也从未使用；读记录失败（`:1085-1089`）与 `list_all_fields` 失败（`:1028-1034`）一律 `Retryable`，默认 30s 一轮，错误配置会永久烧两路出站调用。已有 `feishu_approval_pending > 0 for 15m` 告警，但不能区分「排队」与「卡死」，所以「没人知道」略过头。修法：claim 时取 attempts、record_outcome 递增、上限 10 转终态 + 指标；`seed_tasks` 的「清空回填格即可重试」必须同时重置 attempts。 |
| D4-6 | P2 | P2 | 成立 | `event_challenge.rs:98-108` 对「解密失败」「解密后非 JSON」返回 `raw_json()`，`ResponseBody::Raw` 映射 HTTP 200；同一 handler 的「未配置密钥」（`:85-90`）与「缺 challenge」（`:143-147`）也是 200。飞书只认 200 为投递成功，所以错误密钥会静默丢事件。修法：这四种错误返回 5xx（`Transient`），只有真正的 challenge 回 200；约 10 LOC。 |
| D4-7 | P2 | P2 | 成立 | `deploy-blue-green.sh:1223-1244` smoke 起绿对时线上仍在跑；同一份 config 挂进两个容器（`:325`）；四个 worker 只受 `can_pull()` 门控，无 `workers_enabled` 开关。事件/审批/拉取都没有跨实例租约。修法：加 `workers_enabled`（默认 true），smoke 传 false；注意 **workers-off 不关 schema sync**，绿容器仍会在旧版本服务时改共享库（应并入 S14 的门禁）。 |
| D4-8 | P2 | P2 | 部分成立 | `receive_approval_event.rs:175` 在缺顶层 uuid 时用空串；`event_table.rs:32-38` 的 `event_id` 是 required+unique，而 required 只拒 null，空串通过，MySQL 只保留一行空串，后续同类事件被当重复并回 200。正常飞书 payload 有顶层 uuid（集成测试 fixture 也是），所以是鲁棒性缺口而非现行 bug。修法：缺 uuid 时用 `event_type|instance_code|status|operate_time` 的确定性哈希；不要全局改 required 语义。 |
| D4-9 | P2 | P3 | 部分成立 | `feishu_approval_worker.rs:1244-1252` 用 `last_error == 本轮原因` 判已通知，`:1271-1302` 先发后写库，注释自认崩溃窗口会重发。失败原因是固定字符串（`approval_feedback.rs:230-241`），比「任意字符串」稳；终态行会离开 claim 集，重复需要亚秒级崩溃窗口。降为 P3。修法：`notified_at` 或 `(task_id,outcome,reason_hash)` 幂等键；若要真正闭环，必须在发送前写意图记录。 |
| D4-10 | P2 | P3 | 部分成立 | 两条路径确实存在，但**不是语义漂移**：`task_state` 与 `outcome_record` 是镜像的，worker 会读 HTTP 请求日志跳过重复终态通知，`record_outcome` 用 `state=pending` CAS 让首个写者赢。唯一可证的缺陷是并发双通知窗口：HTTP 路径先发通知（`:519-529`）、后写任务行（`:540`）、最后写请求日志（`:558`），worker 在这一窗口 claim 同一行会看不到 HTTP 发送。降为 P3。 |
| D4-11 | P2 | P2 | 成立 | `outbound.rs:648-658` 只有 `feishu_outbound_request_total{result=failed}`；retry 分支不计数，无成功、耗时、429、endpoint。审批 worker 已有 pending gauge 与 terminal counter；event worker 只有 terminal counter，无 pending gauge。修法：在 `send_with_retry` 一次埋点，加 `ok/retry/failed` 三态、duration histogram、`feishu_rate_limited_total`、`feishu_event_pending`；label 只能用固定 endpoint 分类。 |

**D4 遗漏（复核新增，按重要性排序）**：

- **P2 任意未列业务码都是 Fatal**：飞书新增的瞬态码不在 `CODES_RETRYABLE` 里就永久钉死记录，直到人工清空回填格。
- **P2 HTTP 200 + 非 JSON 变成 `Fatal{code:0}`**：代理/门户返回 HTML 200 时，实例可能已建成，却写「飞书拒绝本次请求」并终态。
- **P2 审批 seed 每 30s 重拉所有空回填记录**：`feishu_approval_worker.rs:565-597,620-643`，`SEED_MAX_PAGES=20`、每页 500 行；超过 10,000 行直接 Fatal，只有一个 warn，无指标。这是该 addon 最大的周期性出站流量来源，审计完全没提。
- **P2 event worker 的 `instance_status` 写入 warn-only**：失败后仍 `Processed`，任务行可能永久非终态，后续事件反复富化浪费调用。
- **P3 `last_synced_at` 是死字段**：`task_table.rs:101` 声明后全仓无写入。
- **P3 单条派发与轮询的双通知竞态**：见 D4-10 的验证段。
- **P2 smoke 绿容器也跑 schema sync**：`workers_enabled` 只关 worker，不关启动期 DDL。
- **P3 `receive_approval_event` 解密后非 JSON 返回 400，注释却承诺 5xx**：若飞书只对 5xx 重试，这条会静默丢事件（需人工确认飞书重试策略）。

### 3.5 D5 数据层网络（MySQL／Redis，9 条）

| ID | 审计 | 复核 | 判定 | 核心证据与结论 |
|---|---|---|---|---|
| D5-1 | P1 | P1 | 成立 | `crates/yang-db/src/redis/config.rs:228-238` 把 `idle_timeout_seconds` 映射为 deadpool 的 `timeouts.recycle`；deadpool 0.12.3 在 `timeout_get` **借出时**执行 `try_recycle`（UNWATCH+PING），超时返回 `Ok(None)` 后继续下一个空闲槽。默认 300s、默认池 20 连接，最坏单次 `get` 是 300s × 空闲连接数；HTTP 路径会被 60s TimeoutLayer 截断，但 outbox 发布、审批 worker 等非 HTTP 调用方可以停摆约 100 分钟。仓库自身注释把 recycle 写成「归还时」，这正是 300s 长期未被发现的认知来源。修法：**固定 1–2s**（不要新增一个会被线上旧 `idle_timeout_seconds=300` 覆盖的配置项），同步改三处错误注释。 |
| D5-2 | P1 | P2 | 成立（影响被低估） | `compose.infra.yaml:60-72` 只有 `--appendonly yes`，无 `requirepass`／ACL／`maxmemory`。审计只说「可读写／FLUSHALL」，真正的信任模型问题更重：`request_validator.rs:90-95` 命中 Redis 即放行、不再查 MySQL，攻击者可以把旧版本键 `SET` 回去让已撤销 Token 复活（且不带 EX 就能让 5s TTL 失效）；也可 `DEL` step-up 的 `SET NX` 标记重放 proof。挑战者把定级降为 P2：触达 Redis 需要同网络容器执行权，而 backend 容器已挂着含 MySQL 凭据的 config，同一位置可直接改授权事实源。**按 P2 排期，但按 P1 的爆炸半径修**：requirepass + maxmemory 256mb + noeviction 一起做，且先让客户端/healthcheck/flush 支持密码。 |
| D5-3 | P2 | P2 | 成立 | `crates/yang-db/src/mysql/database.rs:209-219` 只设池参数，无 `after_connect`；全仓无 `max_execution_time`／`innodb_lock_wait_timeout`／`lock_wait_timeout`。MySQL 8 默认 `lock_wait_timeout=31536000`（1 年），启动期 schema 同步/DDL 等元数据锁会挂一年。修法：`after_connect` 设 `max_execution_time`（只约束只读 SELECT）、`innodb_lock_wait_timeout`（行锁）、`lock_wait_timeout`（MDL）；值可配，不要拍死 5s；注意 `connect_lazy` 自建池绕过 session 变量。 |
| D5-4 | P2 | P3 | 成立 | `database.rs:251-267` 把 MySQL 的 `waiting` 恒写 0，`telemetry.rs:373-393` 无条件导出假 0；无 acquire 等待 histogram。池饱和已有 `open/max ≥ 0.9 且 available==0` 告警，所以这是可观测性误导而非故障路径。修法最小：MySQL 侧不导出 `state=waiting` 并在文档注明；不要改 `PoolStatus` 成 Option 波及 Redis 侧真实 waiting。 |
| D5-5 | P2 | P3 | 成立 | `config.show.toml:76-77` 把 `mysql.connect_timeout_seconds` 写成「建立新连接的超时」，实际是 sqlx `acquire_timeout`（含等槽+探活+建连）；Redis 的 `min_connections/max_lifetime/test_before_acquire` 是 no-op，模板仍按生效项展示，`CONFIGURATION.md:170-171` 还承诺 Redis 受 `max_lifetime` 约束。只改文档为 low；改字段名必须加 serde alias（`deny_unknown_fields` 会让线上旧键启动失败）。 |
| D5-6 | P2 | P3 | 部分成立 | 5s TTL、miss/error 回源 MySQL 属实；但审计建议里两项**已经实现**：`authz_version_fallback_total`（`request_validator.rs:267-269`）与版本发布主动写缓存（`worker.rs:30-45,82` 经 Outbox）。回源是主键单行查询，256 并发 ÷ 20 连接不构成「风暴」。**不要做进程内正向缓存**——它会绕过 Outbox→Redis 的主动失效，直接延长撤销延迟，是安全倒退；single-flight 在 D5-1 修完后收益进一步下降。先用现有指标量出真实回源率再决定。 |
| D5-7 | P2 | P3 | 成立 | `tools.rs:199-234` 先 MySQL 再 Redis 串行，共用一个 2s readiness 总预算。但 readiness 在这个拓扑里**不驱动任何流量**：nginx 没有主动 upstream 健康检查，Docker HEALTHCHECK 只认 `/health/live`，只有 deploy 的 smoke/cutover/rollback 拿它当门禁。所以「越忙越 not-ready」的后果是误告警 + 发布被挡回，不是摘流量。修法：并发 + 单探针超时；**不要换独立轻量连接**（那会让池打满时仍报 ready，掩盖真实不可用）。 |
| D5-8 | P2 | P3 | 成立 | `ops/prometheus/yang-system.rules.yml` 只有应用侧池 gauge 与 readiness/SLO，无 Redis `used_memory/evicted_keys`、无 MySQL `Threads_connected/InnoDB 锁等待/慢查询`；compose 无 exporter。但「是否有采集端」是生产事实，UNVERIFIABLE；若没有 Prometheus/Alloy/云监控，先补采集端再补 exporter，否则规则空转。 |
| D5-9 | backlog | P3 | 成立 | `crates/yang-db/Cargo.toml:38,40` 的 deadpool-redis 0.22／redis 0.32 均未开 TLS；`Manager::create` 用 `get_multiplexed_async_connection()`，`response_timeout`／`connection_timeout` 都是 None，在途命令无 deadline。审计说「同宿主不受影响」不严谨：单线程 Redis 卡在长 Lua/AOF fsync/swap 时命令同样无限挂起。TLS 对同宿主零收益；`response_timeout` 才是真问题，但 deadpool-redis 不暴露连接配置，要自写 Manager 或改用 `redis::aio::ConnectionManager`。 |

**D5 遗漏（复核新增）**：

- **P1（按爆炸半径）未认证 Redis 可精准改写授权版本键 → 已撤销 Token 复活**：`request_validator.rs:90-95` 命中即放行，键名规则在 `version_cache.rs:102-104`；FLUSHALL 反而只是安全回源。修 D5-2 时必须同时考虑缓存写入完整性（例如接受 version 只增不减、带 TTL），不能只加密码。
- **P3 `config.show.toml:115-117` 的 `redis.idle_timeout_seconds` 注释仍是「连接空闲回收时长」**：与真实语义相反；运维按注释调小会得到「每次借出 PING 预算变短、正常连接被丢弃」。
- **P3 `test_before_acquire=true` 在 acquire 被取消/超时时会丢弃已取到的连接**：正常 RTT 下可忽略，但 20 连接池 + 过载/断连时放大连接抖动；结论仍是保持 true，只是容量评估要算进去。
- **P3 outbox 积压告警与停摆 worker 共用同一条阻塞链路**：D5-1 的 300s 挂起期间，backlog gauge 保留上一次刷新值，`YangSystemAuthorizationPropagationDelayed` 不会响，直到挂起结束才补报。
- **P3 登录/注册对 Redis 是 fail-closed**：`AuthRateLimiter::check` 在 `eval_script` 出错时直接返回 Err；修完 D5-1 后已登录 API 能靠 MySQL 继续，但登录/注册/改密仍然 503。这不是缺陷，是有意设计，路线图要写明，避免误以为「修了 recycle 就扛住 Redis 故障」。

### 3.6 D6 安全信任边界（5 条）

| ID | 审计 | 复核 | 判定 | 核心证据与结论 |
|---|---|---|---|---|
| D6-1 | P1 | P2（若 true 则 P1） | 部分成立 | 代码事实成立：`src/config/mod.rs:644-655` 默认 false；开启后 `request_log.rs:94-116,263-278` 记录 method/path/query/全部 headers/body，三个机器入口的 Token 明文进日志。**历史运维记录与审计原文都称两台生产机为 true**，本次未复核；在 S01 证实前按「若 true 即 P1」对待，不能按默认 false 排期。现网 `environment=test`，审计建议的「production 才启动失败」对现网是**空操作**。当前无日志采集器，暴露面 = 能读 docker 日志的人；同机 config 本就含明文凭据。修法：把闸门放**部署预检**（`deploy-blue-green.sh::preflight_release_config` 读 `log_inbound_requests`，true 且无 `ALLOW_INSECURE_REQUEST_LOG=1` 就 die，与 environment 无关），启动时打一条 WARN；若 S01 证实为 true，立即关开关、轮换两个机器 Token、清理已落盘日志。不要默认脱敏（会废掉该开关的唯一用途：看飞书真实报文）。 |
| D6-2 | P2 | P2 | 部分成立 | 公开端点、token 校验前先查库属实；但「爆破 token」不成立——token 是 32 字节 OsRng + SHA-256 + 常数时间比较。真实影响是匿名流量把 20 连接池压满，认证用户查询在 10s acquire_timeout 后失败；派发创建侧还有全局 90 次/分钟 Redis 令牌桶。修法：边缘 `limit_req` 只加在 Feishu API location；应用层若做限流必须带真实 IP 维度（先挂 TrustedClientIpMiddleware，否则键退化成 127.0.0.1）。 |
| D6-3 | P2 | P2 | 成立 | `email_delivery.rs:143-146` 生成 `{base}/reset-password?token=<raw>`；`frontend/deploy/nginx.conf` 无 `access_log`，落默认 combined（`$request` 含查询串），官方镜像通常软链到 stdout；`docker run` 无 `--log-opt`，日志留存远超 15 分钟。缓解属实：一次性、库存摘要、TTL 900s、`Referrer-Policy: no-referrer`。修法：链接改 `#token=`，前端优先读 fragment 并保留 `?token=` 一个版本；或在 nginx 对 `/reset-password` `access_log off`（只解决日志，不解决浏览器历史）。 |
| D6-4 | P2 | P3 | 部分成立 | `user_token.rs:176-181` 是进程内 `LazyLock<Mutex<HashMap>>`，TTL 600s、一次性 consume。蓝绿会同时跑两个进程，但用户流量只走已发布端口的那个容器；「A 签发、回调命中 B」只发生在授权进行中恰好 cutover/重启的 ≤10 分钟窗口，失败是 fail-closed，重开一次授权即可。降为 P3。修法：state 存 Redis `SET NX EX 600` + `GETDEL`；不要改无状态 HMAC（会丢一次性消费语义）。 |
| D6-5 | P2 | P2 | 部分成立 | 机制在框架源码成立：对端不在受信集则直接用对端；存在 `Forwarded` 时优先于 XFF；从右向左遇到不受信跳即停。容器 nginx 用 `$proxy_add_x_forwarded_for` 追加，TLS 边缘落地后若只信 `127.0.0.1/32`，解析会在 Docker 网关这一跳停止。但触发依赖「边缘只设 XFF、不设 Forwarded」这一未强制条件；若现网 `trusted_proxy_cidrs` 尚未填，今天已经是全站单一 IP 桶，不是「TLS 后才退化」。修法：先填 `127.0.0.1/32`，TLS 落地时补 `docker network inspect` 得到的网桥段；**同时**在容器 nginx `proxy_set_header Forwarded "";`，否则补 CIDR 只是把伪造通道保留下来。 |

**D6 遗漏（复核新增）**：

- **P2 匿名请求可无上限写库**：`dispatch.rs:483-558` 对鉴权前失败也写 `feishu_approval_request_log`（`:558,997-1041`）；`portal_query.rs:552-588` 对每个查询（含 rate_limited 出口）也写 `portal_log`。单行 `request_body` 是 TEXT（约 64 KiB），匿名者可持久放大写入与磁盘占用，并把可控文本写进控制台回显。
- **P3 公开端点用失败码区分「配置不存在」与「token 错误」**：`approval_options.rs:335-361,438-443`、`dispatch.rs` 的 `CONFIG_NOT_FOUND` 与鉴权失败是不同出口，可批量枚举已配置的集成；token 仍不可爆破，信息价值低。

### 3.7 D7 可观测性与运行时生命周期（7 条）

| ID | 审计 | 复核 | 判定 | 核心证据与结论 |
|---|---|---|---|---|
| D7-1 | P1 | P1 | 成立 | 默认 filter `yang_system=info,tower_http=info,error`（`src/config/mod.rs:853-856`）让未列出的 target 只留 ERROR；`yang_runtime`／`yang_base` 的 INFO/WARN 全丢，包括契约承诺的「Action 执行完成」（`yang-runtime/src/observability/logging.rs:174`）。两处纠正：401 **已在**分母里（`TokenAuthMiddleware` 在 ActionLogMiddleware 内侧，认证失败记为 `result=error`）；`tower_http=info` 不产生访问日志（TraceLayer 默认 DEBUG）。真正漏掉的是 **404/408/413**，以及被 `TimeoutLayer` 取消的 Action 请求——超时时 ActionLogMiddleware 的 await 被 drop，指标不记录，全部超时会让 SLI「无样本」而不是「高错误率」。修法：filter 加 `yang_runtime=info,yang_base=warn`，TraceLayer 设 INFO，传输层 counter／histogram。若生产沿用本地 `config.toml` 的更严格写法（没有全局 `error` 兜底），框架 ERROR 也会丢；S01 要核对 `[logging].filter` 实际值。 |
| D7-2 | P1 | P2 | 部分成立 | 业务 `/health/ready` 无 gate、无预算、每次打 DB／Redis 属实；但公开性取决于 `BIND_ADDR` 覆写，仓库默认 loopback；仓库内也没有业务就绪探针消费者（deploy 只探管理面，单机无 LB）。降为 P2：按匿名依赖探测定级；若接入外部 LB／编排器则升回 P1。修法：边缘 404，管理面 9090 保留。 |
| D7-3 | P2 | P2 | 成立 | `bootstrap.rs:393-401` 的 select 只看 serve 与 budget，不看 worker JoinHandle；四个飞书 worker 的 JoinHandle 在运行期无人 await，panic 只留在 handle 里；readiness 只看 Tools 与 gate；全仓无 `worker_alive`。panic 并非绝对静默（stderr 有 panic hook），但没有结构化事件、没有指标、readiness 不变。修法：JoinSet／FuturesUnordered 进 select，失活摘 readiness 或按退避重启；补 `worker_alive{worker}` 与 last-success。 |
| D7-4 | P2 | P2 | 成立 | `shutdown.rs:115,139-151` 超时只 drop phase future；`worker.shutdown(self)` 里的 JoinHandle 随之 drop，tokio Drop 仅 detach；四个飞书 worker 的 Drop 只发 watch（审计只点了 2/4），`run_loop` 只在轮次之间看信号，单轮基准常量 300s 且 `rate_per_minute` 可配到 1，实际可远超。修法：四个 worker 的 Drop 照抄 `authorization/worker.rs:182-188` 的 `task.abort()`，或 shutdown 内 `select!` + grace 后 abort。**顺序**：先让 worker 可取消，再把 docker stop 调到 40s；否则 `-t 40` 只是让进程多活 30s。 |
| D7-5 | P2 | P3 | 成立 | `telemetry.rs:32-34` 桶上界 10.0，`set_buckets` 全局生效；导入/审批的长尾全部落 `+Inf`。对 p95 < 500ms 的 SLO 影响有限，真正受损的是长尾与容量分析。修法：`set_buckets_for_metric(Matcher::Suffix("_duration_seconds"))` 只给 action/import 类加 30/60/120/300；不要给 `authz_propagation_seconds` 加 300s。 |
| D7-6 | P2 | P2 | 成立 | 触发通道 `unbounded_channel`（pull／approval）；interval 默认 `MissedTickBehavior::Burst`；outbox 250ms 每轮失败都 ERROR；审批手动触发每次都跑整轮全局 claim。修法：`mpsc::channel(1)` + `try_send`（或按 config_id latest-wins，注意审批点击携带 applicant，不能直接丢），interval 改 `Delay`，outbox 连续失败退避到 ≤2s 并在状态切换时打 ERROR。 |
| D7-7 | P2 | P3 | 部分成立 | 「仓库内无告警规则」为假：`ops/prometheus/yang-system.rules.yml` 在 `cd1c333` 就存在，含 13 条告警，CI 用 promtool 做正反用例。真正未接规则的是 `feishu_outbound_request_total{result=failed}` 与 `feishu_event_terminal_total`；另外**没有任何采集端部署**，规则存在不等于会响。降为 P3（规则补齐），但「告警链路未闭环」按 P2 处理。 |

**D7 遗漏（复核新增）**：

- **P2 关闭阶段串行共享单一截止时间**：一个 worker 吃满 30s 后，后面的 `Tools::close()` 与 telemetry flush 直接超时；日志又被 D7-1 的 filter 丢掉，运维看不到是哪一阶段耗尽预算。
- **P2 告警规则存在但部署清单没有 Prometheus／Alertmanager**：`compose.infra.yaml` 只有 mysql/redis；`SLO.md` 自己写明接收器需在目标环境单独演练。没有采集端，所有 SLO 告警默认不会响。
- **P3 readiness gate 在 bind 之前置位**：窗口短，但正是发布失败场景；deploy 脚本先探管理面 readiness，可能拿到一次 200 后再连接失败。
- **P3 日志格式文档漂移**：`config.show.toml:379` 说「一行一个 object」，实际 `telemetry.rs:86-102` 是 pretty 多行；接采集器前必须改单行 JSON。
- **P3 两套 Action 指标并存且口径不同**：`yang_action_*`（链尾 handler）与 `yang_system_action_*`（最外层中间件）同时暴露，数值会不同，SLO 只用后者。

### 3.8 D8 前端网络层（8 条）

| ID | 审计 | 复核 | 判定 | 核心证据与结论 |
|---|---|---|---|---|
| D8-1 | P1 | P2 | 成立 | `frontend/deploy/nginx.conf:23-89` 无 gzip／brotli／gzip_static；Dockerfile 整份覆盖 nginx.conf，官方镜像的 conf.d 不被 include；实测 dist 的 JS 闭包 931,670 B → gzipSync 280,007 B（3.33×），CSS 57,115 → 10,664。降为 P2：内部控制台的一行性能改动，不是 P1。修法：http{} 加 gzip 六行；`application/json` 只在配了 `gzip_proxied` 时加，避免和 backend CompressionLayer 双压。contract 脚本加存在性断言。 |
| D8-2 | P1 | P2 | 成立 | `client.ts:27-31,60-63` 只透传调用方 signal，无超时；`AbortSignal.timeout` 只在 auth-session 与 session-controller 的 10s 路径出现；account API 接受 signal 但无人传。TanStack 只在 rejection 后重试，黑洞连接不产生 rejection。降为 P2。修法：统一 `requestWithTimeout`（`AbortSignal.any` 需与 `AbortSignal.timeout` 同样的可用性判断，Chrome 116+／Safari 17.4+）；超时映射可重试 ApiError；**写请求不得自动重试**；预算要大于后端 60s（普通 90s，导入/派发 330s）。 |
| D8-3 | P2 | P2 | 成立 | 五处裸 fetch：`features/account/api.ts:50,156,310` 与 `admin-api.ts:41,80`；`access_ttl_seconds=3600`。审计的「:78 注释」证据误读——那条注释属于 `fetchCurrentUser`，它确实会刷新。修法：五处各套 `requestWithTokenRefresh`（`postAuthenticated` 是一层包装，覆盖大部分写操作），约 2 行/处；顺手给 `listUsers/issuePasswordReset` 加 optional signal。 |
| D8-4 | P2 | P3 | 成立 | `http.ts:16-25` 把任何非 JSON body 变 `ContractError` 并丢掉 Response；`action-response.ts:64-74` 与 `client.ts:40-48` 先 parse 后看 `ok`；2xx 空 body 时 `envelope?.code !== 0` 为 true，会把 204 当失败。nginx 的 413/502/504 HTML 显示「服务端返回了无效 JSON」，status/requestId 全丢。降为 P3：只影响诊断，不影响数据/正确性；204 分支当前不可达。修法：解析失败按 status 构造 ApiError 并保留正文前 200 字符；2xx 空 body 视为成功；`ContractError` 只留给 schema 不符。 |
| D8-5 | P2 | P2 | 部分成立 | `session-controller.ts:85-107` 的 `.catch(() => commit anonymous)` 把所有失败收敛为匿名；`auth-session.ts:108-121` 已区分终态并 rethrow；`restoreState === anonymous` 会短路后续恢复，且无 `online` 监听。审计说「只有整页刷新能恢复」不对：重新登录即可恢复，丢的是**自动恢复**。修法需要第三态（不是只改 catch）：仅终态 commit anonymous，瞬态进入非终态并短退避重试一次 + `online` 监听；auth-gate 加重试/退出按钮。 |
| D8-6 | P2 | P3 | 成立 | `App.tsx:24-26` 只有 `retry:1`；TanStack 默认 `staleTime 0` + `refetchOnWindowFocus`；三处 queryKey 含 access token（`api.ts:139,427`、`use-catalog.ts:20`）；13 个 useQuery 文件里只有 2 个没设 staleTime。降为 P3：纯请求数/效率，无正确性后果；「重试两次」实际是 1 次重试=2 次尝试。修法：`staleTime 15s` + 仅网络/408/429/5xx 重试；queryKey 去 token，身份隔离仍靠 `queryClient.clear()`。 |
| D8-7 | P2 | P3 | 成立 | `action-request.ts:126` 用 `redirect:'manual'`；浏览器返回 `opaqueredirect`（status 0、headers 不可读），`action-response.ts:22-34` 的 `location` 恒 null；`use-presented-actions.ts:165` 无 location 不跳转，`:170-178` 仍提示「操作成功」。当前没有任何 Action 声明 redirect，属潜在缺陷，降为 P3。修法：改 200 JSON 信封携带目标 URL，渲染层按字段 navigate；或删除 redirect response kind，等真需要再设计（框架 openapi 仍把它建模成 3xx，app-only 修不完整）。 |
| D8-8 | P2 | P3 | 成立 | 三层上限都是**整个请求体**：nginx `client_max_body_size 16m`；axum 对 multipart 路由 `DefaultBodyLimit::max(max_total_bytes)`；`decode_multipart` 的 Content-Length 预检与 `spec.max_total_bytes` 比较；`http.max_body_bytes` 还被 `src/config/mod.rs:947-949` 硬顶 16 MiB。恰好 16 MiB 的文件加上 multipart boundary/part 头必然超 16 MiB。**只改 nginx 17m 不够**。修法（三选一）：(a) config 上限、action `max_total_bytes`、nginx 一起抬到 20 MiB；(b) 保持 16 MiB payload，传输层按 `max_total_bytes + 64 KiB` 放行，nginx 17m；(c) 前端预检预留开销。contract 脚本硬编码 `client_max_body_size 16m;`，必须同批改。 |

**D8 遗漏（复核新增）**：

- **P2 登录与邮箱码登录没有客户端超时**：`LoginPage.tsx:73,166` 不传 signal，`lifecycle.ts:35-50` 不加超时；连接黑洞时 `submitting` 永远 true。若超时包装只落在 `engine/http`，登录仍会无限挂。
- **P3 `AbortSignal.timeout` 可用性判断不一致**：auth-session 有 feature detect，session-controller 直接调用；老引擎上会在 fetch 前抛 TypeError，而 `disableAccount` 的 finally 仍清本地会话，造成「账号没停用但界面已登出」。
- **P3 没有中央 401→会话过期漏斗**：`SESSION_EXPIRED_EVENT` 只从 `expireSession` 发；裸 fetch 的 401 只冒泡到组件 catch，SessionController 保持「已认证」，空闲标签页会僵尸。
- **P3 公开门户请求不能 abort／超时**：`portal-api.ts:365-384` 不接 signal、不设超时；门户查询卡住时页面一直 loading。

---

## 四、论证质量与第一性原理批评

### 4.1 七条不变量：哪条成立、哪条写错了

| 不变量 | 复核判定 | 批评与改写 |
|---|---|---|
| 1. 信任边界唯一：只有受信边缘能写转发头；TLS 在凭据之前终结；日志不得成为凭据的第二出口 | 不完整 | 方向对，但它是「配置断言」不是「运行时事实」。deploy 契约自认只保证源码默认值；`EdgeBindAddr` 默认 `auto` 会沿用线上 `0.0.0.0`。它也没有「同一配置键下的发现必须联合评估」的规则，于是把两个互斥前提并列成事实（D2-2 依赖 trusted CIDR 已填，D1-1 无条件成立）。出口也不止 wire/log：`feishu_user_token.access_token/refresh_token` 是普通 Text 列明文落库，AES key 与密文同机，备份加密无强制。改写：边界必须被**运行时证明**；任何通道（响应、日志、错误、数据库静态、备份）都不得二次输出凭据；任一配置状态使边界不可证明（production + 空 CIDR，或公网明文绑定）必须启动失败或至少显式告警。 |
| 2. 超时预算单向收敛：外层超时 > 内层超时 | 过强 | 作为普适不等式不成立。真正要保证的是「一个绝对截止时刻在传播中只减不增」。分层超时可以满足「外层 > 内层」而安全性仍不成立：ConcurrencyLimit 在内、Timeout 在外，排队时间完全不计入 60s，每个请求入场后重新获得完整 60s。分阶段预算是求和关系，不是嵌套：附件链路 3×120s 塞进 60s 入站，按字面修法要抬到 400s，而审计自己在 D4-3 反对这个修法——说明它的实践比原则更对。流式响应不存在总时长可比性；超时不等于取消，sqlx-mysql 0.8.6 全 crate 无 cancellation 原语，丢 future 只放弃客户端等待，服务端语句照跑、连接照占。改写：边缘铸造绝对 deadline，每跳取 `min(阶段预算, 剩余 deadline)`；机械校验 `Σ(阶段上限×重试) ≤ 总预算`；装不进预算的工作一律剥离为持久任务行；流式只用空闲/进度截止；MySQL 侧靠 `max_execution_time` 释放资源，而不是丢 future。 |
| 3. 过载必须卸载：有界队列 + 明确拒绝（429/503） | 过弱 | 一个只有深度上限、没有等待上限、只有单一优先级的实现也能满足它，而现网正是如此：一个 per-route 的 ConcurrencyLimit(256) 同时承载匿名 JSON、公开机器入口、管理动作和 16 MiB 导入；真正的上限是 nginx worker_connections 1024。它没说「在哪一层卸载」——读完 16 MiB body 再卸，堆已经花掉了。改写：准入在仍能廉价回答的最外层；同时约束深度与等待；按匿名/已认证/批量上传分通道，避免批量导入饿死登录；拒绝在指标里双重记账（shed 与失败分母）；队列等待是一等 SLI。 |
| 4. 不可逆动作先确认：外部副作用未确认前不得写终态；不得把「解析不了」当成「确定失败」 | 不完整 | 顺序错了：不可能「先确认再发生」。必须做的是「先落持久意图（幂等键 + 任务行），再尝试，再落结果」。仓内的洞恰好在这里：write_extended 返回 `()`，事件 worker 无视失败返回 Processed，随后才 mark_processed——意图在，结果不在。它也没有声明投递语义（创建是 at-least-once + uuid 幂等，通知是 at-least-once + 字符串去重），评审无法区分 bug 与设计。它只堵了「解析不了当失败」，没堵镜像错误「解析不了当成功」：classify 对任何 2xx 返回 None，包括 200 的 WAF/网关 HTML，create_instance 再把解析失败映射成 Fatal{code:0} 并写回填。改写：三态结果（成功/确定失败/未知），只有前两者可落终态；未知绝不写业务可见状态；非幂等调用的「成功」是 `2xx ∩ 可解析`；每个终态有测试过的 requeue 通路，并有周期性反熵作业对齐外部事实源。 |
| 5. 状态归属明确：单实例机械强制；多实例必须有租约/集中状态/幂等键 | 不完整 | 它提出了要求，但没给「机械强制」任何形态。现状是 10 余处注释宣称单实例，而发布流程每次都短暂违反：smoke 起绿对、同一份 config 挂两个容器、四个 worker 只受 can_pull() 门控。OAuth state 与 user_token 刷新锁在进程内，在这个窗口里就是正确性 bug。它还漏了重启：一次性能力的状态只活在内存里，重启后丢失或 TTL 内可重放。改写：每份可变状态有声明所有者，并由「第二个所有者会失败并大声报错」的机制强制（启动租约/心跳行、smoke `workers_enabled=false`、共享存储锁）；进程内状态只允许用于「丢失对正确性不可见」的缓存；发布流程属于不变量的一部分。 |
| 6. 可观测性覆盖分母：SLI 包含拒绝/排队/超时；规范事件在生产 filter 下真的落盘 | 不完整 | 它停在「采集」，而可运营的链条是 信号→告警→runbook→负责人。它没有「缺席检测」，而这正是仓内后台循环的主导失效模式：四个 worker 的 JoinHandle 不进 select、无 worker_alive、readiness 不含 worker。它说「SLI 只统计 Action」也不全对：401 在分母里；真正漏的是 404/408/413 与被 TimeoutLayer 取消的请求。它还把「落盘」当终点，忽略了仓内已有 13 条 promtool 校验的告警，以及**没有任何采集端**这个更前置的事实。改写：每个承诺要同时有 (1) 含拒绝/排队/超时/路由前流量的分母、(2) 有负责人/runbook/代码化测试的告警、(3) 对一切应持续运行的东西有缺席检测；只存在于计数器而不在告警里的信号不算可观测性；SLO 分母在边缘测。 |
| 7. 交付与预算同口径：预算按 gzip 算，线上就必须 gzip；契约校验运行时 | 过弱 | 它被写成两个例子（gzip、config 默认值），真正的一般性质是**制品身份**：能回答「现在跑的是哪一份源码」。本仓回答不了：框架是 path 依赖，镜像由开发机工作树构建，打可变 tag `:live`，Dockerfile 与部署脚本零 LABEL，CI 用浮动 `LIB_YANG_REF=master`；path 依赖在 Cargo.lock 里没有 revision/checksum。它还自相矛盾：前端 nginx 是 digest 固定，后端运行时与构建镜像只有 tag。改写：每个制品由可重建的源码修订标识（仓库+commit 或镜像 digest），写进镜像与发布证据；每个预算/上限/安全姿态在部署后对运行实例断言，绝不从模板推断；path 或浮动 ref 消费的依赖必须显式记录修订，否则不可发布。 |

### 4.2 七条之外缺了什么（10 条）

| # | 缺失不变量 | 优先级 | 本仓证据 |
|---|---|---|---|
| 1 | 可恢复性必须机械化（备份/RPO/RTO/恢复演练） | P0 | `RUNBOOK_BACKUP.md` 写了 RPO ≤5 分钟/RTO ≤1 小时，但仓内没有备份作业；`deploy/README.md:221` 明说 MySQL 没有备份机制；没有 backup-age/restore-drill 告警。审计自称「append-only 审计」是优点，却让它成为全系统最不能丢、唯一没有自动备份的存储。 |
| 2 | 秘密的静态存放、传输与轮换 | P0 | 飞书 user access/refresh token 是普通 Text 列；AES key 与密文同机、同一份 config；`.mysql-root-password` 只 chmod 600；备份「必须加密」只是 runbook 一句话，无强制。 |
| 3 | 每个控制项的降级模式显式声明（fail-open vs fail-closed） | P1 | 门户限流对 Redis 错误上抛（fail-closed）；授权版本校验在 Redis 错误时降级 MySQL（fail-open + 放大）；readiness 串行探针共用业务池（过载时最需要它时反而 not-ready）。三种策略都没有写进契约。 |
| 4 | 容量与硬上限（headroom） | P1 | compose 与 docker run 都没有 memory/cpu/log 限制；Redis 无 maxmemory 却承载授权版本与限流计数；256 并发对 20 条 MySQL 连接；没有容量模型或压测脚本，4 GiB/200 MiB 都是模型上界。 |
| 5 | 制品身份与供应链完整性 | P1 | 见不变量 7。另：应用 CI 只有 fmt/clippy/test --locked 与 pnpm audit，没有 Rust 漏洞扫描；cargo-deny 只在框架仓。 |
| 6 | 与外部事实源的反熵/对账回路 | P1 | 派发要等 handle_dispatch 返回后才写任务行，408 会留下无本地行的孤儿实例；事件 worker 回写失败仍 Processed；`src/infrastructure/` 下没有任何作业枚举远端审批并 diff。uuid 反查是被动修复，不是对账器。 |
| 7 | 变更管理：启动期 DDL 的版本化与可回滚发布 | P1 | schema_sync 会发 ADD/RENAME/MODIFY/ADD PRIMARY KEY，但无版本记录、无事务、无破坏性闸门；每次启动都执行；rollback 只换镜像。审计把「删列」当破坏性例子，但这条路径根本不可达。 |
| 8 | 单点故障与恢复拓扑 | P2 | 一台机、一个 MySQL、一个 Redis、一个宿主 nginx，加共享 netns 的前端；蓝绿不消除任何一环。审计的 K8s/Cluster「不建议」结论应保留，但要写明这是**接受的单点风险**，不是「没有单点」。 |
| 9 | 数据保留期与增长上限 | P2 | session/outbox 有保留期，`audit_event` 有 retention 索引但没有任何删除作业；request/portal log 无保留期；没有表大小/最老行年龄告警。 |
| 10 | 时钟源与偏差预算 | P3 | outbox 积压年龄用数据库时钟，ADR 明说不以时间戳代替版本；但飞书 token 的 350/365 天比较用应用时钟，全仓没有 NTP/chrony 检查或告警。 |

### 4.3 八个维度评分：重评

原报告用「强/中/弱」，但没有可复现的评分规则，且标签与发现清单互不聚合（D4 列了三条 P1 却评「中」，D2 同样三条 P1 却评「弱」）。复核用「触发可达性 × 影响 × 可逆性 × 可检测性 × 证据置信度」重算，按**最坏链**取整，不出半级：

| 维度 | 审计 | 复核 | 理由 |
|---|---|---|---|
| D1 入站 HTTP | 中 | 中 | 最坏链是匿名 16 MiB JSON × per-route 256 × 无容器内存上限 → 一次性容量事故；但 4 GiB 是模型上界，C=1，封顶「中」。若做一次 300 并发 16 MiB 实测，应改判「弱」。 |
| D2 边缘/部署 | 弱 | 弱 | 公网明文 0.0.0.0:18654 承载登录凭据/会话 Cookie/重置令牌，T×I=12。D2-8 的 cutover 停机被审计主动降为「单实例内部系统可接受」，这个分寸是对的，不该抬成 P1。 |
| D3 出站客户端 | 中 | 中 | 最坏链是附件四份拷贝（约 250–270 MiB 峰值），但触发需要已认证管理员动作；connect/read 未分离是真实的可诊断性缺口。 |
| D4 飞书集成 | 中 | 弱 | 按**维度最坏链**：200+非 JSON（或未列业务码）→ Fatal{code:0} → 写回填 Terminal → 该行永久退出扫描，T=3、I=4、D=1（单次误判不触发终态告警），乘积 12。按**单条定级**它只有 P2（人工清格/重新授权可恢复），两者不矛盾：单条可恢复，但整类没有自动修复通路，所以维度是「弱」。 |
| D5 数据层 | 中 | 中 | 最坏链是半死 Redis + 300s recycle × 空闲连接数，但已有 outbox 积压告警兜底（D=0）；Redis 无认证的真实后果是授权版本可被改写，按外部可达性降为 P2，按爆炸半径按 P1 修。 |
| D6 安全边界 | 弱-中 | 弱 | 「弱-中」不是合法输出；公网明文边缘一条链就足以定「弱」，半级会掩盖它。另外审计完全没提静态凭据明文 + 备份加密无强制。 |
| D7 可观测/生命周期 | 弱 | 弱 | 结论对，理由要换：真正的最坏链是事件/拉取 worker panic 后 JoinHandle 不进 select、无 worker_alive、readiness 不含 worker——静默停止且无人知道。审计的「无告警规则」是事实错误，D7-1 的 filter 缺口是真的。 |
| D8 前端网络层 | 中 | 中 | 最坏链是无客户端超时 + 黑洞连接 → 全屏骨架无限等，T=3、I=2、D=2。D8-1 的零压缩有本地实测；D8-3 的「5 处各套一层，出现第三种形态再抽」是全文最符合最小改动原则的建议。唯一要抬的是 D8-5：启动期把可重试失败当已登出，修复成本一行判断，建议按 P1 处理。 |

### 4.4 五个系统性缺口：哪些成立、哪些证据要换

| # | 审计的系统性缺口 | 复核结论 |
|---|---|---|
| 1 | 信任边界只做了一半：TLS 缺位、Forwarded 可伪造、日志可泄凭据 | **成立**。D2-1 是无争议 P0；D1-1 是无条件 P1；D6-1 是条件性 P2（生产开关值未核）。D2-2 的「当前就可利用」要加条件：`trusted_proxy_cidrs` 含受信 CIDR。 |
| 2 | 不可逆输出：错误分类把瞬态失败写成终态 | **成立，但证据链要换**。审计的 504 例子是错的（5xx 走 Retry）；真正的入口是 200+非 JSON、未列业务码、`write_extended` 忽略失败、通用「非 Transient→Fatal」。按单条定级多为 P2（人工可恢复），但整类没有自动修复/对账通路，所以仍应作为第一批修。 |
| 3 | 超时/背压没有单一链 | **成立，机制要纠正**。不是「排队到 408」，而是排队不计入 Timeout、256 是 per-route、Redis recycle 300s、MySQL 无语句超时、前端无超时。 |
| 4 | 蓝绿发布未闭环：启动期改共享 schema、回滚不回退 schema、前端滞留旧 netns | **成立，机理要纠正**。schema 同步永不删列；删 NOT NULL 列破坏的是新代码（1364）；rollback 旧镜像会再跑同步，可能 MODIFY/RENAME；前端 netns 僵尸是结构性的。 |
| 5 | Redis 无认证、无内存上限，却承载授权版本/验证码/Step-up | **成立，后果比审计更重**。不只是 FLUSHALL/可用性：命中路径不校验键年龄，攻击者可精准 SET 旧版本让已撤销 Token 复活；也可 DEL step-up 标记重放。触达需要同网络执行权，所以外部可达性按 P2，爆炸半径按 P1。 |

### 4.5 十条横向主题：哪些是洞察，哪些是重复

| 主题 | 复核 |
|---|---|
| 1. 信任边界只做了一半 | 成立；但「TLS 缺位 + Forwarded + 日志」三者不是并列，TLS 是 P0，另两条是 P1/P2，且相互有前置关系（D1-1 必须先做 D2-2）。 |
| 2. 超时预算没有单一链 | 成立；要补充「排队不计入预算」「超时≠取消」「每跳归因」三点。 |
| 3. 过载与背压缺位 | 成立；但「排队到 408」是错的，边缘无 limit_req 与 per-route 信号量才是真问题。 |
| 4. 不可逆输出与 ack 语义 | 成立；证据链需重写（见 4.4-2）。 |
| 5. 多实例状态假设不成立 | 成立；但蓝绿 smoke 是**有界窗口**，OAuth state 的实际失败窗口 ≤10 分钟且 fail-closed，按 P3 而非 P2；真正要机械强制的是 `workers_enabled=false` 与「第二个所有者会报错」。 |
| 6. 可观测性分母缺失 | **一半成立**：401 在分母里；漏的是 404/408/413 与被 TimeoutLayer 取消的 Action。告警规则存在且被 CI 校验，但**没有采集端**，这是比「分母少一类」更前置的问题。 |
| 7. 交付与预算口径脱节 | 成立。gzip 实测 3.33×，首屏降幅约 70%；预算是 gzip 口径，线上裸传。 |
| 8. 配置与文档漂移 | 成立，且比审计列的更多：`trusted_proxy_cidrs`、RetryConfig 熔断、Redis recycle「归还时」、`mysql.connect_timeout`、日志单行 JSON、Dockerfile 的 127.0.0.1。 |
| 9. 发布生命周期未闭环 | 成立；schema 同步永不删列、rollback 会重跑旧同步、smoke 绿容器也跑 schema sync，这三点审计没写全。 |
| 10. 依赖组件安全默认缺位 | 成立；Redis 无认证/无 maxmemory；要补「静态凭据明文 + 备份加密无强制」。 |

### 4.6 文档内部矛盾（最重要的 12 条）

1. **三份「先做」清单互不一致**：结论的 Top-10（第 38-49 行）、30 天表（第 535-556 行）、末段「先做 30 天表第 1–10 项」既不同集也不同序。Top-10 的 #4（有界重试）与 #6 的一半属于 60 天项，而 30 天表的 #11/#13/#16/#17 不在 Top-10。
2. **D4-1 与 D4-5 的顺序会放大风险**：30 天先做 D4-1（扩大可重试集合）、60 天才做 D4-5（attempts 上限），中间 30 天会把更多错误变成永久重试，正好放大「坏记录每 30s 重试」的配额燃烧。
3. **D8-8 与 30 天 #14 自相矛盾**：一边说「恰好 16 MiB 的文件必然被 16 MiB 总上限拒绝」，一边把「xlsx 16 MiB 通过」当验收。
4. **D2-5 的 `-t 40` 与 D7-4 的 300s 单轮冲突**：`-t 40` 只保证 30s 预算不被 SIGKILL；worker 在预算耗尽后被 detach，进程退出时仍被中止。两个验收口径「无 SIGKILL」与「drain 完成」不能同时成立。
5. **D1-3 与 D2-12 的 healthcheck 路径冲突**：边缘 404 `/health/ready`，而 D2-12 建议 healthcheck 打「会经过后端的路径」，最自然的就是它；落地后前端容器会永久 unhealthy。正确做法是改打共享 netns 内的 9090。
6. **D2-1 的 options-only 白名单会 404 掉 dispatch/event/challenge/oauth/reset**：审计自己的 D6-2、D4 设计文档、README 都列了这些公开路径。
7. **D2-1 步骤 4 切 production 漏了 `link_base_url` 必须 https**：README 明说 production 下 http 会拒绝启动；按文档顺序切会让容器起不来。
8. **D1-2「multipart 保持 16 MiB」与 D8-8「16 MiB 文件必然超限」不能同时成立**：要么把 16 MiB 解释为 payload 上限并接受原始报文 17 MiB，要么把验收改成 15.9 MiB。
9. **D2-4 说 rollback「不碰 schema」是错的**：旧镜像启动会跑 `sync_table_definitions`，能 MODIFY/RENAME；「删列」在同步路径上不可达。
10. **D5-6 的「短负缓存」与授权撤销语义冲突**：负缓存要解决 Redis 故障回源，但同一故障下 Outbox publish 也失败；恢复后若负缓存压住 MySQL 回源，revoke 的版本递增要等 TTL 才可见。只能缓存**连接/超时错误**（1–2s），不能缓存授权结果，恢复时主动清。
11. **D1-9 的 `limit_conn yang 50;` 会 `nginx -t` 失败**：`yang` 只由 `limit_req_zone` 定义，`limit_conn` 只认 `limit_conn_zone`。
12. **D8-2 的 15–30s 前端超时违反自己的「外层 > 内层」**：nginx/应用都是 60s；而且把写请求超时当可重试，会重试服务端可能已成功的 POST。

### 4.7 审计漏掉的最重要的 10 件事

1. **财务正式机是同一 P0 的第二个实例**（P1）：`deploy-finance-prod.ps1` 指向 `47.112.16.98`，同一份蓝绿脚本，默认 `auto`；只修测试机等于只修一半。
2. **没有域名/DNS/ICP 前置**（P2）：裸 IP 拿不到公共 CA 证书；TLS 四步计划把拿证书当零成本。
3. **审批 seed 每 30s 重拉所有空回填记录**（P2）：超过 10,000 行直接 Fatal，只有一个 warn，无指标；这是该 addon 最大的周期性出站流量。
4. **匿名请求可无上限写库**（P2）：dispatch 与 portal 每次请求各 INSERT 一行，鉴权前失败也写；单行 TEXT 可塞约 64 KiB。
5. **200 + 非 JSON 变成 Fatal{code:0}**（P2）：比审计的 504 故事更真实、更危险。
6. **任意未列业务码都是 Fatal**（P2）：飞书新增瞬态码会永久钉死记录。
7. **event worker 的 `instance_status` 写入 warn-only**（P2）：失败后仍 Processed，任务行可能永久非终态。
8. **关闭阶段共享单一截止时间**（P2）：一个 worker 吃满预算，后面的 Tools close 与 telemetry flush 直接超时。
9. **告警规则存在但没有采集端部署**（P2）：规则 ≠ 会响；这是比「补规则」更前置的缺口。
10. **登录/注册对 Redis fail-closed**（P3）：修完 recycle 不等于 Redis 故障可降级；登录仍会 503。

### 4.8 方法与局限：审计诚实度评估

- **诚实的地方**：明确声明静态审计、未压测、未读生产配置；把 4 GiB/200 MiB 标为模型上界；主动把 D2-8 的停机窗口降为「内部系统可接受」；「明确不建议做」一节的分寸总体很好（特别是删死熔断配置、不引入编排器、不关闭 `test_before_acquire`、不全局降 JSON 上限）。
- **不诚实或至少不一致的地方**：D2-2/D2-6 把 `trusted_proxy_cidrs=127.0.0.1/32` 当已核对事实，与自己 §八「未读生产配置」矛盾；D6-1 的生产开关值同样被当成已知；D4-3 引用的 `widget_rules.rs:92` 在声称的基线 `cd1c333` 不存在，说明实际读了未提交工作树；D7-7 的「无告警规则」是事实错误。
- **静态审计能判定的**：某行存在/不存在、某字段从未被引用、某函数返回 `()`、某配置文件没有某指令、上游 crate 的 recycle 语义。这些是本审计最值钱的部分，约占 P0/P1 权重的八成。
- **静态审计不能判定的**：4 GiB/200 MiB 的真实峰值、生产端口绑定、生产 CIDR、生产日志开关、Feishu 对 4xx 的重试策略、真实导入耗时、宿主是否多租户、是否有采集端。这些必须人工核对或实测，不能靠「代码推导 + 定论语气」。
- **总体判断**：「骨架扎实、五个系统性缺口」方向正确，但更准确的重述是三类：**没有控制的证明**（源码默认值 ≠ 运行时事实）、**没有修复通路**（不可逆写之后没有对账/requeue）、**没有恢复点**（唯一事实源无自动备份）。五分之三的 P1 都能归到这三格。

### 4.9 审计的「最值得先做的 10 件事」逐条判断

| # | 审计的 Top-10 | 复核判断 |
|---|---|---|
| 1 | 宿主 TLS 只放行飞书 options 路径并覆盖 Forwarded，再做全量 TLS | **拆开**：Forwarded 清洗可立即独立做（1 行，不依赖 TLS）；options-only 白名单会 404 掉 dispatch/event/challenge/oauth/reset/portal，必须改为「枚举全部公开路径」。TLS 的顺序（先 options 再全量）可保留，但最小集合要扩。 |
| 2 | 生产禁止 `log_inbound_requests=true`，两台机人工核对 | **目标对，闸门错**：现网 `environment=test`，production-only fail-fast 是空操作。闸门应放部署预检，与 environment 无关；人工核对必须做，但不能替代闸门。 |
| 3 | `Fatal{code:0}` 改为可重试；`write_extended` 失败不得 ack | **目标对，证据错**：5xx 已经重试，504 不会落 Fatal；真正要改的是 200+非 JSON、未列业务码、通用非 Transient→Fatal。必须与 #4 的 attempts 上限同批，否则放宽重试会制造永久燃烧。 |
| 4 | 审批 worker 有界重试 + `attempts` 真正递增 + 死信/告警 | **对**。补三件：`seed_tasks` 会把清空回填格的行拉回 pending，必须同时重置 attempts；读记录失败一律 Retryable，要区分 Fatal；通知去重只在终态日志查，显式接收人+中间态会双发。 |
| 5 | Redis `requirepass`/ACL + `maxmemory 256mb` + recycle 降到 1–2s | **对，但顺序要改**：recycle 固定 2s 先做（独立）；requirepass 必须先让客户端/healthcheck/flush 支持密码，不能把带密码 URL 指向无密码 Redis；`maxmemory` 只限数据集，RSS 还要容器 limit。 |
| 6 | `docker stop -t 40`，并在 worker 关闭上做 abort/监督 | **对，但顺序反了**：先做 worker abort/监督（S15），再调 `-t 40`（S56）；否则 300s 单轮仍会在 40s 后被 SIGKILL，`-t 40` 只是多活 30s。 |
| 7 | nginx 开 gzip + 显式 connect/send/read 超时 + 上传/下载 buffering 分流 + `limit_req` | **对，但细节要修**：`limit_req` 只进 `/api` location，不能 server 级；`limit_conn` 必须另声明 `limit_conn_zone`；详细 access log 要在 reset token 改 fragment/脱敏之后开；17m 必须同批抬后端上限。 |
| 8 | 前端统一 `AbortSignal.timeout`；非 JSON 错误保留 status/requestId | **方向对，预算错**：15–30s 短于 nginx/app 60s，违反外层>内层；普通请求 client 90s > nginx 75s > app 60s，导入/派发 330s > 300s > 300s；写请求超时不得自动重试；登录/门户也要纳入。 |
| 9 | 业务面 `/health/ready` 在边缘 404，只保留管理面 readiness | **对**。补：前端 Docker HEALTHCHECK 必须改打共享 netns 的 `127.0.0.1:9090/health/ready`，不能打 8081 的同路径（会永久 unhealthy）；先确认没有外部探针消费业务 `/health/ready`。 |
| 10 | JSON 路由体上限降到 1 MiB，multipart 保持 16 MiB | **方向对，实现错**：JSON 路径的 `DefaultBodyLimit` 是惰性的，必须改 `to_bytes` + 配置；1 MiB 可能误伤 500 条 `upsert_options`，先做成可配；multipart 16 MiB 的「恰好 16 MiB 文件」仍会因整包上限失败，需 20 MiB 或 `max_total_bytes+64 KiB` 放行。 |

**30/60/90 路线图的处理**：其中的结构性项已吸收进本报告：TLS 完整边缘 → S30/S31/S32；派发入队化 → S42；审批重试闭环 → S40/S41；user_token 补偿 → S44；蓝绿 schema 门禁 → S14；worker 监督 → S15；传输层指标 → S22/S50；容器运行面 → S59；出站可观测 → S40/S50；前端续期收敛 → S12；readiness 并发 → S60；auth 版本缓存 → **不按审计建议做 single-flight/负缓存**（见 4.6-10），只保留现有 fallback 指标并修 D5-1；Redis TLS/response_timeout → 推迟到跨主机/托管 Redis 前；附件零拷贝 → S46；直方图 buckets → S50；gzip_static → 暂不做（先 gzip）。

---

## 五、执行路线图

### 5.1 设计原则

1. **先证明、再修复**：凡是依赖生产运行值的 P0/P1，先把「只读核对」作为第 0 波；不先核就排期，会把 P0 修在错误的机器上，或把 P2 当 P1 做。
2. **按文件所有权分组，不按发现编号分组**：同一份 `nginx.conf`、同一份 `deploy-blue-green.sh`、同一份 `src/config/mod.rs` 的改动必须串行；能并行的只有文件不相交的工作流。
3. **先修不可逆、再修可观测、最后修性能**：不可逆损失没有补考；性能/压缩可以晚一周。
4. **每个变更都带一个「失败前失败、成功后通过」的验收**：不能验收的修复不进路线图。
5. **生产窗口单线程**：同一台机同一维护窗口只做一个高风险变更；S31（TLS 切换）、S57（Redis 认证）、S59（容器限额）不要挤在一个窗口。
6. **不把「临时缓解」写成「关闭 P0」**：没有域名/TLS 时，S02 的白名单只能叫缓解；P0 仍是开放状态。
7. **服务器侧配置 operator-owned**：SSH、`config.cloud.toml`、安全组、DNS、证书、`shared-gateway` 的变更由操作者执行；agent 只改仓库代码／模板／文档，并在验收里写清操作者要做的动作。

### 5.2 波次总览

| 波次 | 步骤 | 人日 | 目标 | 退出条件 |
|---|---|---:|---|---|
| Wave 0 | S00, S01 | 1.0 | 归一路径、收集生产事实 | 工作树干净、两仓有 commit；证据文件记录绑定/CIDR/开关/域名/耗时；TLS 是否 blocked 有结论 |
| Wave 1 | S02, S10, S11, S12, S20, S21, S22, S45, S60, S40, S43, S44, S70 | 21.5 | 文件不相交的修复 + 临时边缘 + 备份 | 每个步骤的验收通过；S02 明确标注为 mitigation；备份可恢复演练一次 |
| Wave 2 | S14, S28, S30, S33, S41, S46, S52, S53 | 13.5 | schema 门禁、门户信任边界、TLS 模板、worker/附件/OAuth 收尾 | staging 里破坏性 DDL 被门禁挡住；门户两客户端两个键；TLS 模板 certbot dry-run 通过 |
| Wave 3 | S56, S15, S42, S71, S74, S50 | 14.0 | 部署生命周期、worker 监督、派发入队、凭据加密、降级策略、可观测闭环 | 坏镜像自动回滚；kill worker 后 readiness 变 0；派发 202；token 列密文；promtool 新增用例通过 |
| Wave 4 | S31, S57, S76, S77, S32 | 9.0 | 测试机 TLS 切换、Redis 认证、对账、保留期、财务机 | 明文端口拒绝；HTTPS 全路径矩阵通过；NOAUTH；对账 dry-run 有数；财务机同样通过 |
| Wave 5 | S59, S75 | 3.0 | 实测容量、制品身份 | docker stats 峰值 < 限额且无 OOM；镜像 label 记录两仓 commit |

总计约 **62 人日**（不含发布窗口、人工等待、VPN 依赖的构建时间）。Wave 1 的 21.5 人日不是「21.5 天并行」，单操作者串行约 4–5 周；两人分工可压到 2–3 周，但要注意 cargo 与 pnpm 不能同机并行。

### 5.3 路线图必须先修的 10 个缺陷（红队／完整性复核）

在执行任何步骤前，先改路线图本身：

1. **S01 的命令必须不泄密**：不要执行 `redis-cli CONFIG GET requirepass`（会打印密码）、不要 `docker inspect` MySQL 容器（env 含密码）、不要把整份 `config.cloud.toml` cat 出来。只允许 `PING`（看 NOAUTH/PONG）、`CONFIG GET maxmemory`、`docker ps --format '{{.Ports}}'`、`ss -lntp`、`grep` 单行布尔/CIDR、`timedatectl`、`getent hosts`。证据文件只记布尔/值，不记凭据。
2. **S02 的端口交接在当前工具链里是断的**：`deploy.ps1` 仍默认 `LIVE_HOST_PORT=18654`，只有 `BIND_ADDR` 被继承；下次普通 deploy 或 rollback 会和宿主 nginx 抢 18654。必须先改默认/参数，并让 rollback 重建容器（端口绑定在 `docker run` 时固定，rename/start 旧容器不会改绑定）。
3. **S02 的白名单必须覆盖全部公开入口**：`options`、`dispatch`、`approval/event`、`event/challenge`、`oauth/callback`、portal query/metadata/suggest、匿名账户 API、SPA 历史路由（`/portal/:slug`、`/login`、`/register`、`/reset-password`、`/select-identity`）。只放行 options 会打断现网。
4. **S02 的 trusted CIDR 填的是后端看到的 peer CIDR**：不是宿主边缘的公网地址。共享 netns 拓扑下是 127.0.0.1；经 Docker 发布端口时是网桥网关段；用 `docker network inspect` 的实际值。
5. **S57 的 Redis 认证不能一步开**：redis-rs 0.32.7 在 URL 带密码时会发 AUTH，而**没有 requirepass 的 Redis 会拒绝 AUTH**，所以「先让应用支持密码」只能做代码/解析能力，不能把带密码的 URL 指向现网。顺序必须是：代码/解析+healthcheck 改造 → 同一窗口设 requirepass + 改 URL + 改 healthcheck/flush → 验证 NOAUTH 与 readiness。healthcheck 用 `REDISCLI_AUTH`（不要 `-a`，会进 inspect/进程参数）；密码只用字母数字（`assert_redis_target` 的 shell 解析遇到 `/` 会断）。
6. **没有 staging，破坏性验收不能打在唯一线上主机**：S10 停 MySQL 看 unhealthy、S20 SIGSTOP Redis、S21 `SELECT SLEEP(10)`、S56 故意坏镜像、S59 300 并发导入——这些都会影响线上。要么先建一个 compose staging（低配即可），要么把验收降级为只读/注入式单元测试，并在维护窗口做人审。
7. **S14 的 ready-after-bind 需要框架 API**：`yang-base` 只有 `serve_with_shutdown`，bind 在内部；要先把 `axum.rs` 加进 `files_touched` 并新增 `serve_listener_with_shutdown`，否则验收无法达成。或把这条拆成独立框架步骤。
8. **S71 的密钥托管要写清楚**：`[feishu].encryption_key` 与密文同机、同一份 config；S70 又把 MySQL 备份到 off-host。若备份包含 key，加密等于没做；要么 key 单独托管/托管到不同信任域，要么明确接受同机托管并写进威胁模型。
9. **边缘自身是 SPOF，且测试机是共享网关**：S02/S30 之后 yang 的公网流量过 `shared-gateway`（与 cch/joymeet 等项目共用）；reload 失败会影响其他项目，必须 `nginx -t` 后 reload、不 restart，先备份 server block；`shared-gateway` 容器本身要有 restart 策略、配置备份、日志轮转与监控。S33 只治前端 netns，不治网关死亡。
10. **告警必须有消费端**：仓内 13 条规则没有部署任何 Prometheus/Alertmanager；S50 补的 worker/edge 告警同样不会响。要么加一个最小采集端步骤，要么在 `do_not_do` 里写明「规则已备、采集端另行部署」并指定负责人。

### 5.4 Wave 0：基线与事实

**S00 · 归一化工作树并记录绿色基线** — 0.5 人日｜低风险｜无窗口｜G-BASELINE

- 依赖：无。
- 收益：把 1225 行在制 Feishu 改动提交/暂存到独立分支，避免后续修复把别人的在制品扫进 commit；同时得到一个可回滚的镜像与 commit。
- 代价：需要先跑 `cargo fmt --check`、`clippy`、`cargo test --locked`、`pnpm test`；当前工作树可能有未通过项，先记录而不是硬修。
- 验收：`git status --short` 干净、两仓有 commit、目标 commit 上测试通过、发布记录写 HEAD 与镜像 tag。
- 回滚：`git revert` 基线 commit；无运行时/schema 变更。
- **修订**：不要用 `git add -A`；按目录显式 add，避免把其他会话的在制品扫进来。若工作树属于另一个未完成任务，先 stash 或建 worktree，不要替它提交。

**S01 · 只读收集生产事实（go/no-go）** — 0.5 人日｜低风险｜无窗口｜G-PREFLIGHT

- 依赖：S00。
- 收益：决定 P0/P1 的真实性：两机实际 PortBindings、`trusted_proxy_cidrs`、`environment`、`log_inbound_requests`、Redis requirepass/maxmemory、域名与 443 可达性、最大 JSON body、最大 xlsx 导入耗时、是否有外部探针消费业务 `/health/ready`。
- 代价：SSH 两台机 + 飞书控制台；命令只读；证据文件只记布尔/值，不记凭据。
- 验收：证据文件包含两机的 `docker ps --format '{{.Ports}}'`、`ss -lntp`（80/443/18654/9154）、`docker network inspect yang-system-net -f '{{json .IPAM.Config}}'`、`getent hosts`、外部 `curl -sS -o /dev/null -w '%{http_code}' --max-time 5` 到 443/18654；`timedatectl`/`chronyc tracking`；操作者只报 `[security].trusted_proxy_cidrs`、`[app].environment`、`[feishu].log_inbound_requests`、`[email.password_reset].link_base_url` 的值/布尔，不贴文件；Redis 只跑 `PING` 与 `CONFIG GET maxmemory`；Feishu 控制台记录当前 redirect_uri、external-options origin、控制台 owner 与变更窗口。
- 回滚：无（只读）。
- **修订**：若无名/443 不可达，S30/S31/S32 标记 blocked，S02 成为唯一 P0 缓解路径；不要把 S02 写成「P0 已关闭」。
- **执行状态（2026-10-25）**：测试机已采集，证据见 `docs/operations/2026-10-25-s01-production-facts.md`；财务机 SSH 免密不可用，仅完成外部探测（80/443 超时、18654 明文 200），内部命令块见该文件第五节。

### 5.5 Wave 1：文件不相交的修复 + 临时边缘 + 备份

**S02 · 无 TLS 的宿主边缘白名单（无名回退／TLS 前收面）** — 1.5 人日｜中风险｜**维护窗口**｜G-EDGE-HOST

- 依赖：S01。
- 收益：把公网明文面缩到「飞书公开路径 + 门户 + SPA 外壳」，把已认证控制台收回 SSH 隧道；在没有证书时是唯一可做的 P0 缓解。
- 代价：宿主 nginx 安装、应用改发布到 loopback 端口、trusted CIDR 回填；端口交接有短中断。
- 验收：宿主边缘监听当前公网端口，`docker inspect` 显示应用 HostIp=127.0.0.1；外部访问登录/控制台 API 返回 404，`/assets/*` 与 SPA 外壳正常；options、dispatch、approval/event、event/challenge、oauth/callback、portal、匿名账户 API、`/reset-password`、`/portal/:slug` 返回预期非 404；两个外部客户端产生不同限流键；边缘覆盖 XFF 并清空 Forwarded；控制台经 SSH 隧道可用。
- 回滚：恢复原宿主配置与原 `BIND_ADDR`，reload nginx，重新发布；无数据变更。**注意**：rollback 必须重建容器，rename/start 旧容器不会改端口绑定。
- **修订**：见 5.3-2/3/4。白名单不要手工枚举 SPA 路由；优先「非 `/api` 走 SPA，按需阻断已认证 API 集合」，并把公开 API 集合逐路径写进验收矩阵。`deploy_window` 应为 maintenance，不是 rolling。

**S10 · 容器边缘一次改完（gzip、超时、限流、头清洗、真健康）** — 1.5 人日｜中风险｜滚动｜G-EDGE

- 依赖：S00、S01。
- 收益：去掉仓库内那半明文边缘风险；静态资源压缩；代理预算显式；匿名飞书路径限流；不再透传客户端 `Forwarded`；健康检查真的经过后端；访问日志不记查询串凭据。
- 代价：一次改 `nginx.conf`／`Dockerfile`／contract 脚本；需要重建前端镜像。
- 验收：固定镜像内 `nginx -t` 通过；`curl --compressed` 对 hashed asset 返回 `content-encoding: gzip`；容器边缘 `/health/ready` 404，而 `127.0.0.1:9090/health/ready` 200；停 MySQL 后前端 health 90s 内 unhealthy 并恢复；伪造 `Forwarded` 不改变登录限流键；options 路径 100 次突发后 429、正常 SPA 加载不受影响；`client_max_body_size` 17m；access log 不含查询串。
- 回滚：回滚 nginx.conf／Dockerfile／contract／README，重建前端镜像；无 schema/data 变更。
- **修订**：`limit_conn` 必须另声明 `limit_conn_zone`；`limit_req` 只进 `/api` location；healthcheck 改 9090（与 D1-3 的边缘 404 不冲突）；`client_max_body_size` 改 17m 必须同批改 contract 脚本的硬编码 16m 与 `config/mod.rs:947-949` 的 16 MiB 上限（否则 S22/S11 的 JSON/multipart 预算仍撞墙）；reset-password 路径 `access_log off` 或改 fragment 后再开详细日志。

**S11 · 一次加完配置面（body 上限、worker 开关、schema 模式、日志 filter、生产告警）** — 1.5 人日｜低风险｜滚动｜G-CONFIG

- 依赖：S00。
- 收益：后续步骤改不相交文件，不再抢 `src/config/mod.rs`；把两个静默生产误配变成启动告警；multipart 预算能真正装下 16 MiB 文件。
- 代价：新增若干 serde default 字段、show-config 同步测试、文档。
- 验收：`cargo test config::` 通过；新测试断言 production + 空 CIDR 告警（不 bail）、production + log_inbound_requests=true 告警、`http.max_body_bytes` 默认 17 MiB 且校验允许到 32 MiB、`json_max_body_bytes` 默认 1 MiB 且不超过 max_body_bytes、新 env `YANG_SYSTEM_FEISHU_WORKERS_ENABLED`／`YANG_SYSTEM_MYSQL_SCHEMA_SYNC` 可用、未注册的 `YANG_SYSTEM_*` 仍启动失败、import_xlsx 的 `max_total_bytes=17 MiB`／`max_file_bytes=16 MiB`。
- 回滚：revert；旧配置因 serde default 仍可解析；无 schema/data 变更。
- **修订**：`max_body_bytes` 抬到 17 MiB 会与 contract 脚本、README、`config.show.toml:52` 的 16 MiB 文档冲突，必须同批；`json_max_body_bytes` 的 1 MiB 可能误伤 500 条 `upsert_options`，先把上限做成可配并在 S01 记录最大合法 JSON body。

**S12 · 一个前端请求层：deadline、401 刷新、错误分类、查询重试、懒加载恢复、redirect 契约** — 2.5 人日｜中风险｜滚动｜G-FRONTEND

- 依赖：S00。
- 收益：消除永久骨架与静默挂起；所有写路径具备刷新能力；保留基础设施错误体与 status；4xx 不再重试；部署后 lazy chunk 失效有恢复页；去掉不可读的 opaque-redirect 分支。
- 代价：改 `engine/http/*`、`engine/session/*`、`shell/*`、`features/account/*`、登录/门户 API；需要 fake timer 单测。
- 验收：never-resolving fetch 在 GET 预算后拒绝为可重试 ApiError，mutation 不自动重试；401→refresh→retry 成功；502 HTML 得到 status 502 + body 片段；瞬态 refresh 失败不 commit anonymous、重试一次，终态才登出；404 查询只取一次，focus 在 staleTime 内不重取；登录与门户请求接受 signal 且有 deadline；lazy import 失败渲染恢复面板；redirect action 返回 JSON target 并由客户端 navigate。
- 回滚：revert 前后端 commit 并重新发布；无 schema/data 变更。
- **修订**：超时阶梯必须是 client 90s ＞ nginx 75s ＞ app 60s；导入/派发 330s ＞ 300s ＞ 300s（框架需 per-action 或 per-route timeout）。写请求超时只提示「结果未知，请刷新」，不自动重试；GET/HEAD 与显式幂等动作才允许重试。`AbortSignal.any` 要与 `AbortSignal.timeout` 同样的 feature detect；auth-gate 的 catch-all 要改成第三态（终态/瞬态/重试中），不能只改 catch。补登录与门户的 timeout（审计漏了）。

**S20 · 停止把 Redis idle_timeout 映射成 deadpool recycle，固定 2s 预算** — 0.5 人日｜低风险｜滚动｜G-FW-DB

- 依赖：S00。
- 收益：半死 Redis 不再让一次借出挂 300s；一次 checkout 在 2s 内失败并试下一个空闲槽，限定登录/readiness 的最坏停顿。
- 代价：改 `crates/yang-db/src/redis/config.rs` 一处；修正三处「归还时」注释。
- 验收：单测断言 `build_pool_config().timeouts.recycle == 2s` 且不随 `idle_timeout` 变；SIGSTOP Redis 后 `RedisClient::get` 在 3s 内返回错误；注释改为「借出时」。
- 回滚：revert；无数据/schema 变更。
- **修订**：**固定 2s，不要新增配置项**——线上已写的 `idle_timeout_seconds=300` 会覆盖新默认值，导致「修了等于没修」。验收要按 `idle_slots × recycle + create` 算，不是「单次 <3s」；`idle_timeout` 与 `recycle` 解耦后，把旧键标记 deprecated 或 alias。

**S21 · 约束 MySQL 服务端工作：语句超时、锁等待、真实池状态** — 1 人日｜中风险｜滚动｜G-FW-DB

- 依赖：S00。
- 收益：失控 SELECT 与锁等待在 5s 被杀，而不是占满 20 连接数分钟；假的 `waiting=0` 不再误导饱和告警；导入的批量 DML 不受影响（不是长 SELECT）。
- 代价：`after_connect` 双/三 sysvar；需要 MySQL 集成测试。
- 验收：应用池里 `SELECT @@SESSION.max_execution_time` 为 5000；`SELECT SLEEP(10)` 约 5s 返回 3024；第二个事务等行锁约 5s 报错；元数据锁等待被 `lock_wait_timeout` 限制；`/metrics` 无 MySQL `state=waiting`；`connect_lazy` 自建池注明不走 session 变量。
- 回滚：revert；无数据/schema 变更。
- **修订**：补 `lock_wait_timeout`（默认 1 年）；`max_execution_time` 只覆盖只读 SELECT；`innodb_lock_wait_timeout` 只覆盖 InnoDB 行锁；值必须可配并按实测 p99.9 定，不要拍 5s；after_connect 失败会让新连接全失败、应用起不来，先在 staging 验证 MySQL 8 专有变量。

**S22 · 传输契约：1 MiB JSON、全局卸载、JSON 404/405、INFO 访问 span** — 2 人日｜中风险｜滚动｜G-FW-HTTP

- 依赖：S00。
- 收益：匿名 JSON 解析内存封顶 1 MiB；不可见排队变成快速 503；404/405 有机器可读信封与 request id；默认 filter 下每次请求有访问证据；4 GiB 模型上界不再可能由单个请求造成。
- 代价：改 `crates/yang-base/src/transport/axum.rs`；tower `load-shed` feature；transport 单测 + 真 router 集成。
- 验收：1.1 MiB JSON 到登录返回 413；17 MiB multipart xlsx 仍通过（依赖 S11）；`max_concurrency=1` + pending handler 时第二个请求立刻 503；`GET /does-not-exist` 返回 code!=0 + `x-request-id`；GET-only 路由上 POST 返回 JSON 405 且保留 Allow；默认 filter 下成功与 404 各一条 INFO。
- 回滚：revert；若 shed 误伤，抬高 max_concurrency 或临时关掉 shed；JSON 上限是配置值，可无代码调高。
- **修订**：必须同时改 `DefaultBodyLimit` 与 `to_bytes`，否则 JSON 路径是 no-op；用 `GlobalConcurrencyLimitLayer`（当前是 per-route）；`LoadShed` 需要 tower `load-shed` feature，并用 `HandleErrorLayer` 映射 503 + `Retry-After`；豁免 `/health/*` 与管理面；404/405 handler 自己设 status 与 `x-request-id`；超时状态改 503/504，不保留 408。

**S45 · 出站 HTTP 客户端：connect/read 预算、body 读取分类、显式 proxy/redirect、URL 脱敏** — 2 人日｜中风险｜滚动｜G-FW-HTTP

- 依赖：S00。
- 收益：DNS/建连/慢响应在预算与错误类型上可区分；body 读取超时不再永久 Fatal；容器 proxy env 不再静默改道 Feishu 流量；Feishu 资源 token 不再经日志 URL 泄漏。
- 代价：改 `client.rs`／`request.rs`／`response.rs`；需要本地 stall、connect-timeout、半 body、死代理测试。
- 验收：accept 不写的服务器在 read_timeout+slack 内报错；连接 10.255.255.1 在建连超时内报错，不等总预算；headers + 半 body 后 abort 得到可重试 `HttpRequestFailed`；坏 JSON 仍是 client decode；redacted URL 隐藏 `/medias/<token>` 与 `/approvals/<code>`；死 `HTTP_PROXY` 被显式 no-proxy 忽略；跨 host redirect 默认不跟随（先实测 Feishu 媒体下载是否 302）；`rg '尚未实现'` 无命中，RetryConfig 标 unused/删除。
- 回滚：revert；只改错误路径；无数据/schema 变更。
- **修订**：`read_timeout` 只在 `ClientBuilder`，不要在 `RequestBuilder` 上找；connect_timeout 不覆盖 DNS；read_timeout 是每次读重置，必须大于 50 MiB 下载的最大 chunk 间隔；修 D3-3 时**同提交**把 `HttpTimeout` 加进 `request.rs:805-812` 的可重试分支，否则框架重试会静默失效；redirect 默认不要直接 `none()`，先验证 Feishu 媒体下载是否 302 到 CDN（同 host limited 更安全）。

**S60 · readiness 探针并行 + 单探针超时** — 0.5 人日｜低风险｜滚动｜G-FW-HTTP

- 依赖：S00。
- 收益：MySQL 与 Redis 各自在预算内慢时，readiness 不再误报 not-ready；缩短部署等待与误告警。
- 代价：改 `crates/yang-base/src/tools.rs`；fake resource 单测。
- 验收：两个 fake resource 各 sleep 1.5s、总预算 2s → ready；一个挂 5s → not-ready 约 500ms；旧的串行实现在第一种情况超时。
- 回滚：revert；无运行时数据变更。
- **修订**：不要换独立轻量连接（会让池打满时仍报 ready，掩盖真实不可用）；只做并发 + 单探针 timeout。

**S40 · Feishu 未知结果绝不变终态回填；有界重试** — 4 人日｜中风险｜滚动｜G-FEISHU

- 依赖：S00、S01。
- 收益：关闭唯一会永久钉死业务记录、并用瞬态/歧义错误改写可见回填格的路径；事件回写失败不再 ack；重试有上限且可见；出站指标有成功分母。
- 代价：改 `outbound.rs`／`approval.rs`／`approval_dispatch.rs`／`approval_feedback.rs`／两个 worker；脚本化 transport 矩阵 + 事件 ack 测试 + 失败仓库集成。
- 验收：502/504/503/429 保持可重试且不写回填；200+HTML/非 JSON → Ambiguous，不写回填；4xx 无业务码 → Ambiguous，受 attempts 限制；401/403 触发一次 refresh，仍 401 才可 Fatal；已知配置码仍 Fatal；`write_extended` 返回 Result，失败让 event worker 返回 Failed 且 `processed_at` 为 NULL、`retry_count` 递增；`instance_status` 写失败不返回 Processed；attempts 递增、上限 10、seed 不复活 capped 行；出站指标 `ok/retry/failed` + duration histogram + 固定 endpoint label；WARN 不含完整 URL。
- 回滚：revert；任务行保留 attempts/last_error；无需 schema 变更（attempts 已存在）。若 Ambiguous 导致重试噪声，降低上限或临时把那些码判 Fatal，但保留 ack 修复。
- **修订**：把 attempts 上限/seed 语义并入 S41（避免 S40/S41 双写 `feishu_approval_worker.rs`）；S40 只做分类/ack/回填；**先做 S40 的分类再把 D4-1 放宽**，否则会制造永久重试；`write_extended` 的 Fatal 写失败会 churn 10 轮，可把 Fatal 归 Skipped+alert。

**S43 · 飞书 webhook 诚实失败：解密失败重试、缺 uuid 派生 id** — 1 人日｜低风险｜滚动｜G-FEISHU

- 依赖：S00。
- 收益：错误 Encrypt Key 变成飞书 URL 验证失败，而不是静默 200；没有顶层 uuid 的事件不再坍缩成同一个 dedupe key。
- 代价：改 `event_challenge.rs`／`receive_approval_event.rs`；garbage ciphertext 与重复 payload 单测。
- 验收：`encrypt=garbage` + 已配置 key 返回可重试非 2xx，不是 200 错误体；无顶层 uuid 存确定性派生 id；同 payload 两次 dedupe，同形但 `operate_time` 不同不坍缩；注释与实际 status 一致。
- 回滚：revert；无数据/schema 变更。
- **修订**：`receive_approval_event` 的 decrypt-invalid-JSON 目前返回 400，注释却承诺 5xx；若飞书只对 5xx 重试，这条会静默丢事件。先确认飞书重试策略，再决定 4xx 还是 5xx。

**S44 · refresh token 落库失败不丢新 token** — 1.5 人日｜中风险｜滚动｜G-FEISHU

- 依赖：S00。
- 收益：不再因为一次 DB 写失败迫使人工重新授权；新 refresh token 保留在有界进程槽，短重试期间继续服务；刷新锁只在小重试预算内持有。
- 代价：改 `user_token.rs`；仓库首次更新失败的单测。
- 验收：首次 update 失败的仓库下，第二次 `user_access_token` 不再发第二次 refresh、返回新 access token、并重试 update；锁持有有界；重启后未落库的 pending 记录文档化要求重新授权，而不是静默发死 token。
- 回滚：revert；无 schema 变更。
- **修订**：不要「刷新前写 outbox」；不要在 Redis 存 refresh token（无认证、无备份）；DB 重试预算要小，避免全局 REFRESH_LOCK 被 MySQL stall 拖住。

**S70 · MySQL 自动备份、加密、off-host 副本、恢复演练** — 2 人日（建议 3–4）｜低风险｜无窗口｜G-RECOVERY

- 依赖：S01。
- 收益：把唯一无备份的 append-only 审计库变成有恢复点；这是不可逆写类的安全网；给出真实 RPO/RTO。
- 代价：备份脚本/ service/timer/runbook；off-host 存储与凭据；恢复演练；binlog 归档可能需要改 MySQL 配置。
- 验收：夜间逻辑 dump + binlog 归档（或等价方案）能在文档 RTO 内恢复到 scratch MySQL；记录实测 RPO 并与 runbook 一致；备份加密且复制到 off-host；有 backup-age 告警；演练日志；生产数据不被演练修改。
- 回滚：停用/删除 timer 与脚本；无生产数据变更；删除 scratch 实例。
- **修订**：2 人日偏乐观，先做「逻辑 dump + off-host 加密 + 演练」不做 binlog，或加到 3–4 人日；需要明确 off-host 存储、凭据与批准的 RPO/RTO；`compose.infra.yaml` 若需 binlog retention 要加入 `files_touched`。

### 5.6 Wave 2：启动门禁、门户、TLS 模板、worker/附件/OAuth 收尾

**S14 · 启动门禁：schema 模式、ready-after-bind、worker 所有权、回滚重同步真相** — 3 人日｜中风险｜**维护窗口**｜G-STARTUP

- 依赖：S11。
- 收益：新容器不能在旧版本仍服务时把 rename/type change 应用到共享库；readiness 不再在 listener bind 之前变绿；smoke 可以 workers-off，不会成为 Feishu 任务的第二个所有者；runbook 终于写明 rollback 不是 schema-free。
- 代价：改 `bootstrap.rs`、`SCHEMA_ROLLBACK.md`、`deploy/README.md`；需要 staging fixture（改名/加宽列 + 端口占用）。
- 验收：staging 里改列宽/加 `renamed_from` 让 guarded 模式在 DDL 前中止并打印 plan；additive `ADD COLUMN` 仍应用；plan 模式无 DDL；占用业务端口时管理面 readiness 永不 200；`workers_enabled=false` 的 smoke 能 ready 且无 Feishu 出站调用/指标；首启前有 mysqldump 快照；runbook 写明 rollback 脚本不迁移但旧镜像启动同步会 MODIFY/RENAME，且 DROP COLUMN 不可达。
- 回滚：revert 镜像；若已应用 DDL，恢复发布前快照或按 expand-contract 反向；schema 同步永不删列，旧列可以留着不用。
- **修订**：ready-after-bind 需要框架 `serve_listener_with_shutdown`，把 `crates/yang-base/src/transport/axum.rs` 加入 `files_touched`，或拆成独立框架步骤；`workers_enabled` 不关 schema sync，smoke 仍会改共享库，必须把 schema 门禁一起做；DDL 非事务，RENAME/MODIFY 不能自动回滚，必须备份 + 人工剧本。

**S28 · 门户自己的信任边界 + 匿名 DB 工作有界** — 1.5 人日｜中风险｜滚动｜G-PORTAL

- 依赖：S10、S11。
- 收益：恢复按真实客户端 IP 的门户限流，而不是一个 `127.0.0.1` 共享桶；每个匿名读先过 limiter 再做 DB；429 不再写新日志行；关闭匿名 DB 写放大。
- 代价：改 portal 模块、query/metadata/suggest、options、rate_limit；两客户端 IP、伪造 Forwarded、突发 DB 行数测试。
- 验收：`trusted_proxy_cidrs` 配好后，两个不同 XFF 客户端产生不同 portal Redis key；伪造 `Forwarded` 不改变 key；不存在的 slug 也被限流；429 不产生新 portal log 行；metadata/suggest 被限流；options 在 token 校验前不再区分配置不存在与 token 错误，且常量时间比较；验收走宿主与容器两层边缘。
- 回滚：revert；无 schema 变更。若挂了中间件但 CIDR 为空，行为退回旧的 TCP peer 桶（即变更前状态）。
- **修订**：中间件必须在 D2-2 清洗 Forwarded 之后挂，否则新键可被伪造；限流点要在 slug 查询之前；429 的审计行要挡住；`portal_metadata` 目前没有限流，一并补；选项失败码不要枚举已配置集成。

**S30 · TLS 边缘模板、证书自动化、显式公开路径契约** — 2.5 人日｜中风险｜无窗口｜G-TLS-DESIGN

- 依赖：S01、S10、S02。
- 收益：P0 修复可复用到两台机，而不是一次性手改；枚举所有飞书/门户实际调用的公开路径，白名单不会静默打断审批；证书自动续期。
- 代价：**测试机复用已有 `shared-gateway`（nginx:1.27-alpine，80/443 + certbot + reload hook），新增 yang server block 与证书，不新装宿主 nginx**；财务机 80/443 外部不可达，需先决定开 443 还是走 18xxx TLS；DNS、证书、飞书控制台协调；staging certbot dry-run。
- 验收：`nginx -t` 通过；`certbot renew --dry-run` 成功；staging `curl --http2 -I https://domain/` 200；`openssl s_client -tls1` 失败；每个公开路径（options、dispatch、approval/event、event/challenge、oauth/callback、inbound options upsert/delete、portal query/metadata/suggest、assets、SPA shell）返回预期非 404；带 `:443` 的 Origin/Host 通过同源校验；客户端 `Forwarded` 被覆盖、XFF 被覆盖而非追加；30/14/7 天到期告警。
- 回滚：移除/禁用宿主 server block，保留 S02 边缘直到 S31；证书可吊销/删除；无应用数据变更。
- **修订**：域名/DNS/ICP 是前置；S01 已证实测试机 80/443 开放且已有 `shared-gateway`（容器，`restart unless-stopped`），不需要 systemd enable；财务机 80/443 外部不可达，需先决定开 443 还是 18xxx。`shared-gateway` 与其它项目共用，必须 `nginx -t` + reload、不 restart，先备份 server block，并给它补配置备份、日志轮转与监控（5.3-9）；`listen 443 ssl; http2 on;` 是 nginx 1.30.4 的写法。

**S33 · 前端 netns 僵尸的宿主看门狗** — 1 人日｜低风险｜滚动｜G-EDGE-WATCHDOG

- 依赖：S01。
- 收益：把「全站 000 等人工 refresh」变成 60s 内自动恢复；这个故障在共享 netns 拓扑里每次后端重启都会复发。
- 代价：宿主 systemd timer/脚本；重启后端观察恢复。
- 验收：`docker restart yang-backend; sleep 8; curl` 到宿主边缘返回 2xx/3xx（修复前 000）；计划内停起不会误重启（脚本同时要求管理面 readiness 200 而边缘非 2xx）；每次动作一行日志、幂等。
- 回滚：停用/删除 timer 与脚本；无应用数据变更。
- **修订**：S02 之后「宿主边缘」是宿主 nginx；如果宿主 nginx 挂了而管理面 readiness 200，看门狗会无限重启前端。探测**应用 loopback 发布端口**，或增加「前端 netns 真的坏了」的条件（在 frontend 容器内 `curl 127.0.0.1:8080`）；分别演练「宿主边缘停」与「后端重启」。

**S41 · 审批 worker 背压、通知去重、seed 成本、真实积压 gauge** — 2 人日｜中风险｜滚动｜G-APPROVAL-WORKER

- 依赖：S40。
- 收益：停止重复 IM 与「每 30s 永久重试」的配额燃烧；限制 seed 扫描，避免大表整轮失败；pending gauge 反映真实积压而不是一个 batch。
- 代价：改 approval/event/pull worker 与 task_table；注入更新失败、大 seed fixture、快速触发测试。
- 验收：发送成功+更新失败后下一轮不再发；10k+ 空回填 fixture 在轮预算内 seed 不报错；两次快速 pull/dispatch 触发合并或返回 busy，不排两整轮；interval 用 `MissedTickBehavior::Delay`；pending gauge 在卡住行存在时非零；事件更新任务时写 `last_synced_at`；新列 additive 且 schema-sync 安全。
- 回滚：revert；additive 列可以不用；无破坏性 schema 变更。
- **修订**：`failed` 重试上限/seed 语义放这里（S40 不重复）；seed 的「清空回填格重试」必须重置 attempts；manual trigger 每次都跑整轮全局 claim，运维连点代价是 N 次全表扫描 + 飞书创建配额，不只是日志噪音。

**S46 · 附件 body 改引用计数，不再四份拷贝** — 2 人日｜中风险｜滚动｜G-ATTACH

- 依赖：S40、S45。
- 收益：50 MiB 附件峰值从约 200–270 MiB 降到约 100–120 MiB；幂等重试复用同一 buffer，避免整包重下载。
- 代价：`approval_attachment.rs`／`outbound.rs`／`request.rs` 改 `Bytes`；分配共享单测 + 一次 staging 上传。
- 验收：克隆 body 共享分配（`as_ptr` 相等）；现有附件测试通过；staging 上传保留 Content-Length；50 MiB 附件在幂等重试间复用下载 buffer；multipart 仍复制一次，文档写明这是下限。
- 回滚：revert；无 schema/data 变更。
- **修订**：完整流式是 150–250 LOC 且 Feishu 上传可能要求 Content-Length，先做 `Bytes`，不要一次上 `Body::wrap_stream`；上传 `idempotent=false` 的轮内重试仍会放弃，下一轮仍会重下载（要配合 S41 的退避）。

**S52 · OAuth state 从进程内移到 Redis** — 1 人日｜中风险｜滚动｜G-OAUTH

- 依赖：S44。
- 收益：部署/重启前发起的授权仍能完成；state 在 smoke 与 live 容器间只消费一次；修掉蓝绿流程自己制造的正确性 bug。
- 代价：`user_token.rs`、`oauth_authorize_url.rs`、`oauth_callback.rs`；重启后回调和并发回调测试。
- 验收：重启后访问重启前 mint 的回调 URL 成功；同一 state 两个并发回调只第一个成功；key 有前缀与 600s TTL；过期/未知 state 拒绝；功能关闭时 happy path 不依赖 Redis。
- 回滚：保留一版内存 fallback；清 oauth state key 前缀；无 schema 变更。
- **修订**：用 `SET NX EX 600` + `GETDEL`（redis 0.32 有 GETDEL）；不要改无状态 HMAC（会丢一次性消费）；mint/consume 变 async 会波及同步单测。

**S53 · 密码重置 token 改 URL fragment** — 0.5 人日｜低风险｜滚动｜G-OAUTH

- 依赖：S10。
- 收益：一次性凭据不再进 nginx access log、docker logs、Referer。
- 代价：改 `email_delivery.rs` 与 `ResetPasswordPage.tsx`；链接形状单测 + 手动重置 + 日志 grep。
- 验收：`reset_url` 含 `#token=` 且不含 `?token=`；前端优先读 fragment，且保留 query token 一个版本；手动重置后 `docker logs frontend | grep <raw token>` 为 0。
- 回滚：revert；已发出的链接在 15 分钟内仍可用。
- **修订**：fragment 不进日志但仍进浏览器历史；若不想动前端，至少对 `/reset-password` `access_log off`（只解决日志）；详细访问日志（S10）必须在这之后开。

### 5.7 Wave 3：部署生命周期、worker 监督、派发入队、凭据加密、降级策略、可观测闭环

**S56 · 部署生命周期：stop 宽限、自动回滚、运行时绑定断言、配置闸门** — 2 人日｜中风险｜滚动｜G-DEPLOY

- 依赖：S11、S14（**补 S02**）。
- 收益：不再在 30s 关闭预算前 SIGKILL；坏镜像 cutover 失败自动起回旧对，不再「站点 down 且没人 rollback」；公网绑定成为发布条件而不是源码默认值；`log_inbound_requests` 这个已知泄密开关进不了生产。
- 代价：改 `deploy-blue-green.sh`／`deploy.ps1`／`deploy-finance-prod.ps1`／README；故意坏镜像 + 失败注入演练。
- 验收：`docker inspect` 显示 `StopTimeout=40`（由 `--stop-timeout` 在创建时设置，不只是 `docker stop -t`）；坏镜像 cutover 自动恢复蓝对、边缘 2xx/3xx、退出非 0；`BIND_ADDR=0.0.0.0` 无 `ALLOW_PUBLIC_EDGE=1` 失败并打印实际绑定，带 flag 才通过；smoke 失败 remove 绿对；`log_inbound_requests=true` 无 `ALLOW_INSECURE_REQUEST_LOG=1` 在起容器前 die，只打印键名；smoke 传 `workers_enabled=false`。
- 回滚：revert 脚本/README；无应用数据变更；下次 deploy 立即用旧脚本。
- **修订**：依赖补 S02（S02 重写了同一份脚本/contract）；`StopTimeout` 必须在 `docker run` 创建时设，事后 `docker stop -t` 不会改 `HostConfig.StopTimeout`；自动回滚要区分「瞬时 readiness 抖动」与「真坏镜像」（重试/退避）；回滚复用旧容器时端口绑定与 schema 兼容性都要检查；`-SyncConfig` 可能覆盖线上日志开关，预检要同时查。

**S15 · worker 监督 + 关闭可取消** — 3 人日｜中风险｜滚动｜G-SHUTDOWN

- 依赖：S14、S41、S40。
- 收益：worker panic 不再「readiness 绿但事件静默停」；一个慢关闭阶段不再饿死后面阶段与 Tools close；管理面 serve task 死亡能被发现；第二次 Ctrl-C 可强制退出。
- 代价：`bootstrap.rs`、四个 worker、`yang-runtime/shutdown.rs`、`yang-base/lifecycle.rs`；failpoint panic、慢 worker、双 signal 测试。
- 验收：test-only failpoint 让 worker panic，readiness 秒级转 0 或进程退出，`worker_alive{worker}=0` + 一条 ERROR；慢阶段不阻止后续阶段与 Tools close，每阶段记 success/timeout；管理面 serve task 被杀能被观察；第二次 SIGINT 立即退出；正常 SIGTERM 不误报 worker 失败。
- 回滚：revert 应用/框架 commit；无数据/schema 变更。
- **修订**：四个飞书 worker 的 Drop 都要 abort（审计只点了 2/4）；关闭阶段共享单一 deadline，必须改成每阶段独立预算或「主预算 + 每阶段上限」；`shutdown()` 消费 `JoinHandle` 与重新 await 的语义要避免二次消费；正常关闭也会让 JoinHandle 完成，必须用 shutting-down 标志区分。

**S42 · 单条派发改 enqueue-first（202）+ 收敛两条路径** — 3 人日｜**高风险**｜滚动｜G-DISPATCH

- 依赖：S40、S41。
- 收益：把「每附件 120s 重试」从 60s HTTP 预算里拿掉；消除「客户端超时但远端实例已建、任务行不存在」的孤儿；收敛同步点击与 worker 两条路径。
- 代价：`dispatch.rs` + 设计文档；60s+ 慢 transport、连点、飞书工作流迁移；**需要操作者改飞书工作流**（当前读 `serial_number`）。
- 验收：单条 POST 返回 202 且回复前任务行已存在；不返回 `serial_number`；脚本化 transport sleep 60s+ 仍 202；飞书工作流改为轮询 bitable/通知并看到最终 serial；连点创建一个实例、发一条通知；被拒的匿名请求最多一条有界 request-log，失败码不再在校验 token 前枚举配置；点击时 applicant open_id 持久化供重试。
- 回滚：旧同步路由留一个 release 的 config flag；工作流迁移后 revert flag；无 schema 变更。
- **修订**：这是**破现网契约**的变更（飞书工作流读同步响应），必须与飞书控制台 owner 同窗口；异步路径要先复现 `dispatch.rs:1038-1073` 的 applicant open_id 解析与持久化；rate-limit acquire 要移到 worker；`write_request_log` 仍要在 202 前写一条有界记录，但不能每个匿名请求都写。

**S71 · Feishu 用户凭据静态加密 + 密钥轮换** — 4 人日｜**高风险**｜**维护窗口**｜G-SECRETS

- 依赖：S70。
- 收益：把最大的活凭据仓库（access/refresh token 的普通 Text 列）从明文行与备份里移走；补上今天不存在的轮换路径。
- 代价：`user_token_table.rs`／`crypto.rs`／`user_token.rs`／CONFIGURATION；staging 轮换演练、备份恢复、token refresh；密钥托管与迁移审批。
- 验收：新/更新行存带 key id 的密文；读取路径解密且 refresh 仍工作；staging 轮换把所有行重加密到新 key，旧 key 可退休；恢复的备份用托管的 key 能解密；明文字段**保留不删**直到验证完成，代码双读；发布前有快照。
- 回滚：双读/双写保留明文字段；revert 代码继续读旧列；**永不删列**；密文不可读时恢复迁移前快照。
- **修订**：不要复用只做加密的 AES-CBC options crypto，要用带认证的 envelope（AEAD）；key custody 必须解决：key 与密文同机、备份又 off-host，若 key 进备份等于没加密；要么单独托管，要么明确接受同机托管并写进威胁模型；先确认 `encryption_key` 轮换后历史密文还能解（UNVERIFIABLE，需 scratch 环境实测）。

**S74 · 为每个控制项写下并测试降级策略** — 0.5 人日（偏乐观）｜低风险｜无窗口｜G-DOCS

- 依赖：S28。
- 收益：阻止下一次改动把门户限流静默改成 fail-open、或把授权校验改成 fail-closed；给操作者一张表预测 Redis 故障对登录/门户/authz/Feishu 的影响。
- 代价：`docs/contracts/DEGRADATION.md`；两条行为测试；若要做行为测试，需要给 rate-limit 与 request-validator 注入 cache 错误的 seam。
- 验收：表列出每个依赖（Redis/MySQL/SMTP/Feishu）× 每个控制（登录限流/门户限流/authz 版本/outbox/审批派发）的 fail-open/closed 与用户可见效果；现有测试钉住两种现状；有意变更必须在同 commit 带测试。
- 回滚：文档，revert。
- **修订**：0.5 人日只够写文档；`rate_limit.rs` 现有测试只覆盖 key 形状与判定，`request_validator` 也只覆盖缺端口/来源决策，要加 cache-error 注入 seam 或 test-only cache，否则「行为测试」交不了；**不要**把 `eq` 的「authz 版本 Redis 错误回源 MySQL」写成 fail-open 的推荐，它是有意的降级，但要配 fallback ratio 告警。

**S50 · 在仓库内闭环可观测性：桶、worker/边缘缺席、告警测试、序列存在性** — 1.5 人日｜低风险｜滚动｜G-OBS

- 依赖：S40、S21、S15（顺序：S15 先于 S50）。
- 收益：延迟长尾、worker 缺席、边缘 down 而 backend ready 可见且可测；审计「无告警规则」是错的，真正缺口是规则只覆盖 Action 流量、假设一个没人部署的外部 prober；这一步补规则、钉住序列存在性、写明 probe 契约。
- 代价：`telemetry.rs`、rules/test、OBSERVABILITY；promtool 正反用例；采集端 owner。
- 验收：`promtool check rules` 与 `test rules` 通过，新增 worker 缺席（用 S15 的 `worker_alive` 或 last-success-age）、edge-down/backend-ready、outbox age/stall 的正反用例；合成 12s 观测落在 ≤30s 桶，`+Inf == _count`；每个表达式引用的序列在 `/metrics` 存在；每条新告警有 runbook 与 owner；文档写明哪个 prober 从哪调管理面 `/health/ready`。
- 回滚：revert 规则/telemetry；无运行时数据变更；噪声大可单独删规则。
- **修订**：S15 必须先落 `worker_alive`，否则 S50 的规则无法完成；告警必须有人消费：仓内没有 Prometheus/Alertmanager，S50 要加「最小采集端」或明确 defer + 负责人；`authz_version_fallback_total` 也应有 fallback ratio 告警（D5 遗漏）。

### 5.8 Wave 4：TLS 切换、Redis 认证、对账、保留期、财务机

**S31 · 测试机切 TLS、收回公网应用端口、切 environment=production** — 1.5 人日（建议 2.5–3）｜**高风险**｜**维护窗口**｜G-TLS-GOLIVE

- 依赖：S30、S10、S11、S14、S56、S02。
- 收益：关闭 P0：公网边缘不再有明文凭据/会话 Cookie/重置令牌；限流与审计终于看到真实 IP；production-only fail-fast 生效；飞书外部选项与 OAuth 因路径全枚举而继续工作。
- 代价：**测试机在 `shared-gateway` 增加 yang server block（与其它项目共用边缘，必须 `nginx -t` 后 reload，不 restart；先备份原 server block）**；DNS、证书、安全组、飞书控制台 redirect_uri 与 external-options origin、OAuth 重注册、全路径矩阵、回滚演练；窗口内有已知短中断。测试机 80/443 已开放；**财务机 80/443 外部不可达**，需先决定开 443 还是走 18xxx TLS。
- 验收：外部 curl 明文公网端口被拒；`https://domain/` 200；options/dispatch/approval-event/event-challenge/oauth-callback/inbound-upsert-delete/portal/assets/SPA 全路径预期状态；两个客户端得到不同 `login_event.ip` 与 portal key；`environment=production` 只在 `link_base_url=https` 时启动；飞书 redirect_uri 重新注册且回调完成；外部选项表单在飞书 3s 内加载；证据进发布记录。
- 回滚：恢复 S02 宿主配置，用保存的配置备份按原 `BIND_ADDR` 重新发布，`environment/link_base_url` 一起回；旧 OAuth redirect URI 保留到新 URI 验证通过；**安全组要重新放开旧公网端口**。维护窗口操作，有已知短中断。
- **修订**：前置是域名/DNS/ICP/安全组 80-443（S01 go/no-go）；`link_base_url` 必须先改 https，否则 production 起不来；不要只测 options 200；不要在没有 staging 的情况下把破坏性验收打在唯一线上主机；`-SyncConfig` 可能用开发机配置覆盖服务器，切换前确认已停用或核对键级差异。

**S57 · Redis 认证 + 内存上限（三步凭据迁移）** — 2 人日｜中风险｜**维护窗口**｜G-INFRA-REDIS

- 依赖：S20、S56。
- 收益：关闭「同网络任意容器可改写授权版本键 → 已撤销 Token 复活」的 P1 爆炸半径；限制 Redis 内存，避免拖垮同机 MySQL 与应用。
- 代价：`compose.infra.yaml`、`deploy-blue-green.sh`、README；每个 URL 消费者、healthcheck、reset 路径、readiness；密码分发与重启。
- 验收（严格三步）：
  1. **代码/解析能力**：应用 URL 解析、healthcheck、flush 支持密码；此时 URL 仍不带密码，指向无 requirepass 的 Redis。
  2. **同一窗口**：设 `requirepass`，同时改 `[redis].url`、`redis-cli` healthcheck（用 `REDISCLI_AUTH`）、`-ResetDb` 的 FLUSHALL；重启应用。
  3. **验证**：`docker exec yang-redis redis-cli PING` 返回 NOAUTH；应用 readiness 200；`REDISCLI_AUTH` 可用；`-ResetDb` FLUSHALL 成功；`CONFIG GET maxmemory` = 268435456，policy = noeviction；错误密码失败；日志无明文密码。
- 回滚：在操作者确认下 `CONFIG SET requirepass ""`，恢复旧 URL，重启 Redis 与应用；密码在两处保留到应用确认健康；数据在 `yang_redis_data` 卷里。
- **修订**：**不能把带密码的 URL 先指向无 requirepass 的 Redis**——redis-rs 0.32.7 会发 AUTH，无密码 Redis 拒绝 AUTH，应用直接起不来；`redis-cli ping` 在 requirepass 下打印 NOAUTH 但退出码仍 0，healthcheck 会「绿着但不校验」，所以 healthcheck 要改 `REDISCLI_AUTH` 并显式匹配 PONG；`assert_redis_target` 只认 `redis://`，不要在同一步上 `rediss://`；密码只用字母数字（shell 解析遇到 `/` 会断）；`maxmemory` 只限数据集，不限 RSS，容器 limit 仍要 S59。

**S76 · 飞书对账作业：本地行 vs 远端实例，修复分叉** — 3 人日｜中风险｜滚动｜G-RECON

- 依赖：S40、S41、S42。
- 收益：把「远端已建、本地无行」与「acked 但没写」从永久分叉变成可自愈；这是审计完全缺失的修复通路。
- 代价：新 `feishu_reconcile_worker.rs`、`bootstrap.rs`、`task_table.rs`；staging 删本地行/改远端状态；只读 Feishu 凭据 + dry-run 审核。
- 验收：删掉本地任务行但远端存在 → 作业重建/关联且不重复创建审批；远端状态与本地 `instance_status` 不一致 → 一轮内收敛；dry-run 只报孤儿数不写；作业读多写少，歧义状态绝不写可见回填格。
- 回滚：停 worker；无破坏性写；新行 additive，任务表无新 required 列且无默认值。
- **修订**：先做 dry-run 计数器/只读报告（在 S40/S41 里），让风险在 Wave 1 就可量化；真正的写修复放到 S42 之后；对账需要飞书只读凭据与限流，别把对账变成新的配额燃烧源。

**S77 · 追加型请求日志/门户日志的保留期（需 owner 批准）** — 1 人日｜中风险｜滚动｜G-RETENTION

- 依赖：S70、S01。
- 收益：阻止唯一无备份 MySQL 上的无界增长；session/outbox 已有保留期，request/portal log 没有。
- 代价：`session_cleanup.rs`、新 `request_log_retention.rs`、rules、RUNBOOK；合规批准 + 首次运行前备份。
- 验收：dry-run 打印每表影响行数且不删；书面批准后分批删除超过窗口的行并打指标；`audit_event` 在单独批准前明确排除；首次 live 前有备份。
- 回滚：停 worker；已删行不可恢复，必要时从运行前备份恢复；无 schema 变更。
- **修订**：`src/bootstrap.rs` 要加入 `files_touched`（worker 构造与关闭阶段）；删除是破坏性操作，必须按用户全局数据库安全规则二次确认；保留窗口需书面答案，不能 agent 自定。

**S32 · 财务机镜像 TLS/loopback/production 切换** — 1.5 人日（建议 2.5–3）｜**高风险**｜**维护窗口**｜G-FINANCE

- 依赖：S31。
- 收益：关闭 `47.112.16.98` 上同样的明文边缘；只修测试机等于只修一半 P0。
- 代价：`deploy-finance-prod.ps1`、README、TLS runbook；财务域名外部探测、飞书流程检查、独立脚本/凭据/飞书 app 状态。
- 验收：财务明文端口被拒；财务 HTTPS 200；财务重置链接 https；财务 Feishu options/dispatch/event/OAuth 预期状态；production 只在 https link_base_url 下启动；证据与测试机分开记录。
- 回滚：恢复财务宿主配置与保存的配置，按原 bind 重新发布；旧财务 redirect URI 保留到新 URI 验证通过。
- **修订**：S32 必须是 **Wave 4b**，等 S31 的全路径矩阵与回滚演练通过后再开始；不要与 S31 共用同一个维护窗口；两台机共用 `deploy-blue-green.sh`，S02/S56 的脚本改动会在财务机下次 deploy 时生效，先确认兼容。

### 5.9 Wave 5：实测容量与制品身份

**S59 · 容器资源上限、日志轮转、实测容量基线** — 1.5 人日｜中风险｜**维护窗口**｜G-CAPACITY

- 依赖：S57。
- 收益：把一次噪声导入/匿名 JSON 突发从宿主级 OOM 风险变成有界容器失败；用实测替换审计的 4 GiB/200–400 MiB 模型上界；日志轮转防止写满磁盘。
- 代价：`deploy-blue-green.sh`／`compose.infra.yaml`／README；300 并发 JSON + 17 MiB xlsx 导入压测、docker stats、无 OOM。
- 验收：两应用容器与 MySQL/Redis 都有 memory/memory-swap/cpus/pids/log rotation；压测峰值低于限额且无 OOM；记录实测 peak RSS 与 p99；日志 5×50m 或等价。
- 回滚：抬高/移除限额并重启；无数据变更；卷保留。
- **修订**：必须在 S57 之后（Redis maxmemory 已设，容量账才完整）；限额按实测定，不能直接抄 1g；压测在 staging 或维护窗口，不要打唯一线上主机；`--memory` 与 `--memory-swap` 同时设，避免 swap 抖动。

**S75 · 制品身份：镜像 source revision、基础镜像 pin、Rust 漏洞扫描** — 1.5 人日｜低风险｜滚动｜G-SUPPLY

- 依赖：S56、S32。
- 收益：回答「现在跑的是哪份源码」，这是事故、回滚、诊断的前提；今天镜像用移动 `:live` tag、path 依赖、无 revision label，lockfile 在这条链上不提供保障。
- 代价：`docker/app/Dockerfile`、`frontend/deploy/Dockerfile`、CI、`deploy.ps1`、finance 脚本；inspect label + 新 CI gate。
- 验收：`docker inspect` 的 `org.opencontainers.image.revision` 等于应用 commit，另一个 label 是框架 commit；CI 缺 revision arg 时构建失败；`cargo deny check` 在应用 CI 运行；基础镜像尽量 digest pin；发布记录写两个 revision。
- 回滚：revert Dockerfile/CI；无运行时数据变更；label 是 additive。
- **修订**：应用仓没有 `deny.toml`，要加；CI 仍用浮动 `LIB_YANG_REF=master`，框架 label 必须从实际 path dependency checkout 派生，或 release 构建 pin ref；生产镜像由开发机 `deploy.ps1` 构建，label 只覆盖 CI 构建是不够的，两个构建路径都要打。

### 5.10 关键路径与并行工作流

**关键路径**：`S00 → S01 → S11 → S14 → S56 → S31 → S32`。这条链决定 P0 何时关闭；其余步骤可以围绕它并行，但不能跳过 S01（否则 S31 可能建在无域名/无 443 的沙上）。

**可并行的 8 个 Wave 1 工作流**（文件不相交）：

| 并行组 | 步骤 | 文件所有权 | 注意 |
|---|---|---|---|
| G-EDGE-HOST | S02 | 宿主边缘配置/install/verify、deploy 脚本、TLS-EDGE.md | 与 S10 的 contract 脚本耦合，先落 S02 或同批改 contract |
| G-EDGE | S10 | `frontend/deploy/nginx.conf`、`Dockerfile`、contract、前端 README | 与 S02 共享 deploy/contract，不能完全并行 |
| G-CONFIG | S11 | `src/config/mod.rs`、`config.show.toml`、CONFIGURATION | 配置键的唯一所有者；S20 若需新键必须让 S11 先加 |
| G-FRONTEND | S12 | `frontend/src/engine/http/*`、`session/*`、`shell/*`、account API | 纯前端；与后端步骤并行无文件冲突 |
| G-FW-DB | S20, S21 | `crates/yang-db/*`、`yang-runtime/telemetry` | 两个步骤改不同文件，可并行；共用 cargo 构建，测试串行 |
| G-FW-HTTP | S22, S45, S60 | `crates/yang-base/src/transport/axum.rs`、`http/*`、`tools.rs` | S22 与 S45/S60 文件不同，可并行；cargo 测试串行 |
| G-FEISHU | S40, S43, S44 | `src/addon/feishu/domain/*`、`src/infrastructure/feishu_*.rs` | S40 与 S41 都改 `feishu_approval_worker.rs`，必须串行或合并 |
| G-RECOVERY | S70 | `deploy/backup/*`、RUNBOOK | 独立；需要 off-host 存储与凭据 |

**不能并行的组合**：

- S02 与 S10：都改 `deploy-blue-green.sh`／contract，契约耦合；先 S02 或同批扩展 contract。
- S20 与 S11：若 S20 新增配置键，会与 S11 的 `src/config/mod.rs` 独占冲突；本报告建议 S20 固定 2s、不加键，冲突消失。
- S31 与 S32：S32 依赖 S31 的已验证流程，且共用 TLS-EDGE.md；S32 放 Wave 4b。
- S31 与 S57：同一台机的两个维护窗口变更，失败归因会混；明确先 S31 后 S57（或反之），不要同窗口。
- S12 与任何 cargo 步骤：本机不能同时跑 pnpm 与 cargo（页面文件耗尽、target 毒化）；测试/构建阶段串行，或把前端放另一台机/CI。

**单操作者顺序视图（约 14–16 周，含等待）**：

1. 第 0 周：S00、S01（1 天）+ 读证据、定 go/no-go。
2. 第 1 周：S20、S22、S11、S43、S44（约 6 人日）+ 单元测试。
3. 第 2 周：S40、S41（6 人日）+ Feishu 集成测试。
4. 第 3 周：S45、S60、S70（4.5 人日）+ 备份演练。
5. 第 4 周：S12（2.5 人日）+ 前端 e2e。
6. 第 5 周：S10、S02（3 人日）+ 宿主边缘演练。
7. 第 6 周：S14、S28、S33（5.5 人日）+ staging。
8. 第 7 周：S46、S52、S53、S30（6 人日）。
9. 第 8 周：S56、S15（5 人日）+ 坏镜像/双 signal 演练。
10. 第 9 周：S42、S71（7 人日）+ 飞书工作流迁移、密钥演练。
11. 第 10 周：S74、S50（2 人日）+ 采集端确认。
12. 第 11 周：S31（维护窗口）+ 全路径矩阵 + 回滚演练。
13. 第 12 周：S57、S76、S77（6 人日）。
14. 第 13 周：S32（财务机维护窗口）。
15. 第 14 周：S59、S75（3 人日）+ 容量实测。

这个顺序假设每周只有 3–4 个有效工作日用于该路线图（单操作者还要处理日常运维）；如果全职投入，可压缩到 6–7 周。

### 5.11 代价／收益总表

| ID | 步骤 | 人日 | 风险 | 窗口 | 收益（一句话） |
|---|---|---:|---|---|---|
| S00 | 归一化工作树、绿色基线 | 0.5 | 低 | 无 | 在制品不被扫进修复 commit；有可回滚基线 |
| S01 | 只读收集生产事实 | 0.5 | 低 | 无 | 决定 P0/P1 真实性与 TLS go/no-go |
| S02 | 无 TLS 宿主白名单 | 1.5 | 中 | 维护 | 无证书时唯一 P0 缓解；收回已认证控制台 |
| S10 | 容器边缘一次改完 | 1.5 | 中 | 滚动 | gzip、显式预算、限流、头清洗、真健康 |
| S11 | 一次加完配置面 | 1.5 | 低 | 滚动 | 后续步骤不再抢 config；两个静默误配变告警 |
| S12 | 前端请求层 | 2.5 | 中 | 滚动 | 消除永久骨架、401 刷新、错误分类、重试收敛 |
| S20 | Redis recycle 固定 2s | 0.5 | 低 | 滚动 | 半死 Redis 不再挂 300s×连接数 |
| S21 | MySQL 语句/锁超时 | 1.0 | 中 | 滚动 | 失控查询/锁等待 5s 被杀；池状态不撒谎 |
| S22 | 传输契约 | 2.0 | 中 | 滚动 | 1 MiB JSON、快速 503、JSON 404/405、INFO 访问 |
| S45 | 出站客户端 | 2.0 | 中 | 滚动 | connect/read 可区分、body 错误可重试、proxy/redirect 固定 |
| S60 | readiness 并行探测 | 0.5 | 低 | 滚动 | 依赖各自慢时不再误报 not-ready |
| S40 | Feishu 未知结果不终态 | 4.0 | 中 | 滚动 | 关闭永久钉死记录的主路径 |
| S43 | webhook 诚实失败 | 1.0 | 低 | 滚动 | 错误密钥显式失败；缺 uuid 不再坍缩 |
| S44 | refresh token 不丢 | 1.5 | 中 | 滚动 | DB 写失败不再迫使人工重新授权 |
| S70 | MySQL 备份/加密/演练 | 2.0（建议 3–4） | 低 | 无 | 唯一事实源有恢复点；给出真实 RPO/RTO |
| S14 | 启动门禁 | 3.0 | 中 | 维护 | 旧版本服务时不再改共享 schema；ready 在 bind 后 |
| S28 | 门户信任边界 | 1.5 | 中 | 滚动 | 真实 IP 限流；匿名 DB 工作有界；429 不写库 |
| S30 | TLS 模板/证书/路径契约 | 2.5 | 中 | 无 | P0 修复可复用；公开路径全枚举；证书自动续 |
| S33 | netns 看门狗 | 1.0 | 低 | 滚动 | 全站 000 从人工 refresh 变 60s 自愈 |
| S41 | 审批 worker 背压/去重 | 2.0 | 中 | 滚动 | 停重复 IM 与 30s 永久重试；seed 有界 |
| S46 | 附件 body 引用计数 | 2.0 | 中 | 滚动 | 50 MiB 峰值从约 200–270 降到约 100–120 MiB |
| S52 | OAuth state 进 Redis | 1.0 | 中 | 滚动 | 重启/cutover 不再丢授权；一次性跨实例 |
| S53 | 重置 token 进 fragment | 0.5 | 低 | 滚动 | 一次性凭据不再进 access log/docker logs |
| S56 | 部署生命周期 | 2.0 | 中 | 滚动 | stop 宽限、坏镜像自动回滚、运行时绑定断言 |
| S15 | worker 监督/关闭可取消 | 3.0 | 中 | 滚动 | panic 不再静默；慢阶段不饿死 Tools close |
| S42 | 单条派发 enqueue-first | 3.0 | 高 | 滚动 | 60s 预算不再卡附件；消除孤儿实例；双路径收敛 |
| S71 | Feishu token 静态加密 | 4.0 | 高 | 维护 | 最大活凭据仓库离开明文行与备份；有轮换路径 |
| S74 | 降级策略文档+测试 | 0.5 | 低 | 无 | 下一次改动不会静默翻转 fail-open/closed |
| S50 | 可观测闭环 | 1.5 | 低 | 滚动 | 长尾/worker 缺席/edge-down 可见且可测 |
| S31 | 测试机 TLS 切换 | 1.5（建议 2.5–3） | 高 | 维护 | 关闭 P0；真实 IP；production fail-fast 生效 |
| S57 | Redis 认证+内存上限 | 2.0 | 中 | 维护 | 关闭授权版本篡改→撤销绕过；内存有上限 |
| S76 | Feishu 对账作业 | 3.0 | 中 | 滚动 | 孤儿实例/acked-not-written 可自愈 |
| S77 | 日志保留期 | 1.0 | 中 | 滚动 | 阻止唯一无备份库的无界增长 |
| S32 | 财务机 TLS 切换 | 1.5（建议 2.5–3） | 高 | 维护 | 关闭财务机同一个 P0 |
| S59 | 容器限额/日志轮转 | 1.5 | 中 | 维护 | 宿主 OOM 变有界容器失败；实测替换模型上界 |
| S75 | 制品身份/供应链 | 1.5 | 低 | 滚动 | 能回答「跑的是哪份源码」；CI 有漏洞扫描 |

> 合计约 62 人日（按修正后的 S70/S31/S32 约 66–68 人日），不含发布窗口、人工等待与 VPN 构建时间。**S02/S31/S32 三个高风险步骤不能压缩到同一天**。

---

## 六、明确不建议做（对审计 8 条的逐条判断）

| 审计的「不建议做」 | 复核意见 |
|---|---|
| Kubernetes／服务网格／新数据库／Redis Cluster/Sentinel：单实例模块化单体的证据不支持，运维成本远大于收益 | **同意**。但要补一句：不引入 HA 等于接受 MySQL/Redis/宿主 nginx 三个单点；这是有意的风险接受，应写进运维契约，而不是「没有单点」。 |
| 为熔断器补指标后强行启用：唯一上游 + 有界重试已覆盖 fast-fail；优先删除死配置 | **同意，且理由要更强**：现设计只看 HTTP 状态码、只按 host 分键，对「HTTP 200 + 非零业务码」失明；一个 endpoint 生病会打开整个 `open.feishu.cn`，连 tenant token 刷新一起断。启用前必须改记账口径；更划算的是标记 unused／删除。 |
| 为零停机引入编排器：先用 workers_enabled 与 docker stop -t 消除强杀 | **同意**。但顺序要改：先让 worker 可取消（S15），再调 `-t 40`；只调 `-t` 不会让 300s 单轮自然结束。宿主双 upstream 可延后，先量化停机窗口是否真的消耗 SLO。 |
| 关闭 `test_before_acquire`：同宿主 RTT 约 0.1ms，收益可忽略，还会失去 MySQL 重启后的透明自愈 | **同意**。补一个副作用：acquire 被取消/超时时 sqlx 会丢弃已取到的连接（test_before_acquire 引入 await 点），过载/断连时放大连接抖动；不改变「保持 true」的结论，但容量评估要算。 |
| 全局把 JSON body 降到 1 MiB：会打断 xlsx 导入；只对 JSON 路由降 | **同意**，但审计的实现方式（只加 `DefaultBodyLimit`）对 JSON 路径是 no-op；必须改 `to_bytes` 与配置项。1 MiB 还可能误伤 500 条 `upsert_options`，先把上限做成可配。 |
| 做 per-action body 上限配置系统：先按路由降 JSON，确有大批量 Action 再加覆盖 | **同意**。先一个 `json_max_body_bytes` + 一个全局 multipart 预算，不要一开始做每 action 配置系统。 |
| 进程内 LRU 缓存 `source_key → token_hash`：用常驻内存换 P2 缓解不划算；边缘 limit_req 足够 | **同意**。常量时间比较 + 边缘限流 + 应用层带 IP 维度的桶即可；不要缓存凭据。 |
| 在 TLS options 入口就绪前收回 18654 loopback：飞书审批要求公网可访问，先收回会打断现网 | **同意方向，但「options 入口」这个范围是错的**：只放行 options 会 404 掉 dispatch/event/challenge/oauth/reset/portal。最小集合必须逐路径枚举；S02 的验收矩阵要覆盖全路径。 |
| （审计未列）负缓存/进程内正向缓存授权版本 | **新增不建议**：任何正向缓存都会绕过 Outbox→Redis 的主动失效，直接延长撤销延迟；只能做按用户 in-flight 合并，且先修 D5-1。 |
| （审计未列）在唯一线上主机跑破坏性验收 | **新增不建议**：S10 停 MySQL、S20 SIGSTOP Redis、S21 `SELECT SLEEP(10)`、S56 坏镜像、S59 300 并发导入，都会影响线上；先建 staging 或改注入式测试。 |
| （审计未列）只看 nginx 16m→17m 就宣称 16 MiB 文件可用 | **新增不建议**：axum `DefaultBodyLimit`、Content-Length 预检、`http.max_body_bytes` 都是整包 16 MiB；必须同批改，否则只是把 413 从 nginx 移到应用。 |
| （审计未列）把 `docker stop -t 40` 当关闭问题的完整修复 | **新增不建议**：不修 worker abort/监督，`-t 40` 只是让进程多活 30s；先 S15 再 S56。 |
| （审计未列）在 Redis 客户端/healthcheck/flush 未支持密码前先开 requirepass | **新增不建议**：redis-rs 会 AUTH，无密码 Redis 拒绝 AUTH；healthcheck 会「绿着但不校验」，`-ResetDb` 会失败。按 S57 三步走。 |

---

## 七、需要人工确认的清单（按优先级）

> 以下全部是**只读**检查；不要打印凭据。可由操作者执行后只回报布尔/值。

| # | 问题 | 为什么重要 | 安全命令/方式 |
|---|---|---|---|
| 1 | 两台机的应用边缘实际绑定是 `0.0.0.0:18654` 还是 `127.0.0.1`？ | 决定 D2-1 是 P0 还是 P3 | `docker ps --filter name=yang-backend --format '{{.Ports}}'`；`ss -lntp` 看 18654 |
| 2 | `[security].trusted_proxy_cidrs` 是否含实际 peer CIDR？ | 决定 D2-2 可利用性与 D1-1 是否单桶 | `grep -E '^\s*trusted_proxy_cidrs' config.cloud.toml`（只回值）；`docker network inspect yang-system-net -f '{{json .IPAM.Config}}'` |
| 3 | `[feishu].log_inbound_requests` 是 true 吗？历史运维记录与审计原文称两台机都是 | 若属实，正在泄机器 Token；立即关开关、轮换两个 Token、清理已落盘日志 | `awk '/^\[feishu\]/{f=1} f&&/^log_inbound_requests/{print;exit}' config.cloud.toml`（只回布尔，不贴文件） |
| 4 | `[app].environment` 与 `[email.password_reset].link_base_url` 是什么？ | 决定 production fail-fast 是否生效、TLS 前能否切 production | 同 #2 的 grep 单行；不要 cat 整份 |
| 5 | 是否有域名、DNS 控制、443 可达、80 可达（HTTP-01）、ICP 备案？ | TLS P0 的硬前置 | `getent hosts <domain>`；外部 `curl -sS -o /dev/null -w '%{http_code}' --max-time 5 https://<domain>/` 与 `http://<domain>/` |
| 6 | 安全组放行 80/443 还是只有 18000-19000？ | 决定 TLS 能否 443、还是只能 18xxx 带端口 | 已实测：测试机 80/443 开放（shared-gateway）；财务机 80/443 外部超时、仅 18654 可达。财务机需云控制台确认并决定 443/18xxx |
| 7 | 飞书控制台当前 redirect_uri、external-options origin、owner、变更窗口？ | S31/S32 会改这些；漏改会断 OAuth/外部选项 | 控制台人工查看；只记 URL/owner，不记 secret |
| 8 | 是否有外部探针/LB 消费业务 `/health/ready`？ | 边缘 404 前必须改指向 9090 | 云监控/`docker ps`/外部配置；仓库内无消费者 |
| 9 | Redis 实际 requirepass/maxmemory 状态？ | D5-2/S57 的起点 | `docker exec yang-redis redis-cli PING`（NOAUTH/PONG）；`CONFIG GET maxmemory`；**不要** `CONFIG GET requirepass` |
| 10 | 是否有 Prometheus/Alertmanager/云监控采集端？ | 13 条规则是否有消费者 | `docker ps | grep -iE 'prometheus|alertmanager|alloy|grafana'`；云控制台 |
| 11 | 备份的 off-host 存储、凭据、批准 RPO/RTO？ | S70 不能自定 RPO/RTO | 运维/合规人工确认 |
| 12 | 最大合法 JSON body 与最大 xlsx 导入实测耗时？ | 决定 1 MiB JSON 与 300s 导入预算 | staging/维护窗口实测；不要在生产打大包 |
| 13 | 飞书对 4xx 是否重试？ | 决定 `receive_approval_event` 的 400 是否丢事件 | 飞书开放平台文档 + 控制台重试记录；仓库内不可证 |
| 14 | `[feishu].encryption_key` 是否轮换过？旧密文还能解吗？ | S71 的迁移前提 | scratch 环境用旧行实测；不要动生产 |
| 15 | 两台机的时钟偏移？ | TTL/dedupe/告警 `for:` 都假设有时钟 | `timedatectl`；`chronyc tracking` |
| 16 | `config.cloud.toml` 是否被 `-SyncConfig` 用开发机副本覆盖过？ | 决定 trusted CIDR/日志开关/production 状态是否可信 | 比对部署记录/备份；不要 cat |
| 17 | 财务机是否使用同一个飞书 app/tenant？ | S32 的 redirect_uri/外部选项是否要单独注册 | 飞书控制台人工确认 |
| 18 | 保留期窗口的法务/合规要求与批准人？ | S77 删除不可恢复 | 书面批准；先备份 |

---

## 八、执行建议（按可用时间与前提）

### 如果只有一周

按以下顺序，不要做 TLS：

1. S00 + S01（1 人日）：先把生产事实读出来；若 `log_inbound_requests=true`，立刻在部署预检加闸门（S56 的预检部分，约 0.5 天）。
2. S20（0.5 人日）：固定 Redis recycle 2s。
3. S22 + S11 的 filter/上限部分（约 3 人日）：JSON 1 MiB、全局 shed、404/405、日志 filter；注意 `DefaultBodyLimit` 是 no-op，改 `to_bytes`。
4. S43 + S44（2.5 人日）：webhook 诚实失败、refresh token 不丢。
5. S10 的 gzip + Forwarded 清空 + 17m（1 人日）：先不碰 healthcheck/limit_req 的复杂部分，避免一次改太多。

### 如果有一个月

在「一周」基础上加：

- S40 + S41（6 人日）：Feishu 不可逆输出 + 重试上限 + seed 有界 + 通知去重。
- S14 + S33（4 人日）：schema 门禁 + netns 看门狗（S14 需要 staging）。
- S12（2.5 人日）：前端请求层。
- S70（2–4 人日）：备份 + 恢复演练。
- S28（1.5 人日）：门户真实 IP 限流（先做 D2-2 的 Forwarded 清洗）。
- S30（2.5 人日）：TLS 模板与证书 dry-run；如果 S01 证明无域名/443，**不要**进入 S31，S02 标为缓解。

### 如果没有域名/443

- S30/S31/S32 **冻结**；P0 保持开放。
- S02 作为临时缓解，但必须覆盖全部公开路径（不只是 options），并明确写「这是 mitigation，不是 closure」。
- 优先做 S10/S20/S22/S40/S41/S44/S70 这些不依赖 TLS 的修复；S57（Redis 认证）可以在维护窗口单独做，但要先修 S56 的部署预检与 healthcheck。
- 评估飞书是否支持按源 IP 限制；若不支持，公网明文就是接受的残余风险，必须有书面记录与补偿控制（短会话、强制重置、日志监控）。

### 如果有两个月以上

按 5.5–5.9 的 6 个波次执行；每个维护窗口只做一个高风险步骤；S31 与 S32 分开；S57 与 S31 不同窗口；S59 在 S57 之后；S75 最后做。

---

## 九、复核产物与可追溯性

- 本报告的逐条判定来自 8 个维度的并行代码复核 + 8 路独立对抗反驳；每条判定的证据是当前工作树或上游 crate 源码的 `file:line`。
- 68/68 条全覆盖；没有 `REFUTED`/`STALE`，说明审计的代码事实基本可复核，问题集中在**影响链、定级与生产前提**。
- 对抗复核的原始结构化数据（每条的 evidence/fix_scope/hidden_cost/verification，以及挑战者的 outcome/final_severity）保存在本次会话的 workflow 转录中，可按需导出为附录。
- 本报告不构成变更授权。任何生产变更（尤其 S31/S32/S57/S71/S77 与任何数据库写操作）仍需按用户全局数据库安全规则与发布流程二次确认。

---

## 十、结语

这次复核最有价值的结论不是「审计错了 13 个 P1」，而是：

1. **审计的代码事实可信度很高，但它的风险叙事把「可能性」写成了「现实性」**。68 条里 0 条被推翻，说明读代码是扎实的；但 13 个 P1 降级、20 个 P2 降 P3，说明「发现缺口」与「现在就会损失」之间还有很长的因果链。
2. **真正危险的是一条类，不是五个并列缺口**：200+非 JSON→Fatal、回写失败仍 ack、408 孤儿实例、无对账、无备份——它们共同构成「数据正确性依赖上游永不失常，且出事之后没有修复/恢复通路」。
3. **路线图的第一优先级不是修 TLS 配置，而是 S01 的只读核对**：没有它，你不知道 P0 在哪台机、`trusted_proxy_cidrs` 是否为空、日志开关是否在泄 Token、有没有域名可用。第二优先级是那些**便宜且不可逆损失最贵**的修复：Redis recycle、日志 filter、Feishu 回填/ack、refresh token 保留、schema 门禁、netns 看门狗。
4. **P0 仍然是明文边缘，且只有域名/TLS 能真正关闭它**。如果拿不到域名/443，S02 只是把暴露面缩小并收回已认证控制台；不要在报告里写「P0 已关闭」。

**建议的下一步**：先执行 S00 + S01（1 人日），把 18 个待确认项变成事实；然后按 5.5 的 Wave 1 选 3–4 个文件不相交的步骤并行推进；在每个维护窗口只做一个高风险变更，并在变更前把本报告对应的验收与回滚抄进发布记录。


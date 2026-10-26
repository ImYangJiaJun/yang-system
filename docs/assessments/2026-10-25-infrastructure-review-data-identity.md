# 基础设施对抗复核：数据、身份与恢复

日期：2026-10-25。范围：D5-1～D5-9、D6-1～D6-5，及数据/身份相关路线图。生产代码只读审核；没有修改运行代码、执行生产命令或访问生产数据库。动态验证仅使用本轮临时创建的隔离 Redis 容器。

## 1. 证据边界与主要裁决

- 应用开始基线 `0adc5ab1cbd0729dfdf264fe617ae07a029ee585`；审核期间另一个会话提交后，复核基线为 `5c0ea9cf8c1afe09f8a2cebe1354456c9c703bea`。框架 `D:/code/lib_yang` 基线 `e27a2d3c127e0bea6c40bc5fa584b2fdb2162705`。行号对应本次实际读取的工作树；本附件不声称是某个不可变提交的全量安全扫描。
- 输入是 `2026-10-24-network-infrastructure-first-principles-audit.md`、`2026-10-25-network-infrastructure-audit-adversarial-review-and-roadmap.md`。`docs/operations/2026-10-25-s01-production-facts.md` 仅作历史现场证据；本次没有重连两台服务器，没有读取实际 `config.toml` 或 `deploy/config.cloud*.toml`。
- 先用 CodeGraph 定位并读取实现，未覆盖的行、配置和第三方依赖用定向读取补充。`FW:` 以下指 `D:/code/lib_yang/`；`REG:` 指本地 Cargo registry 的对应锁定版本源码。
- 原报告不能继续保留“没有 REFUTED/STALE”总括：**Redis 故障时已登录 API 仍可靠 MySQL 工作**不成立；**当前应用已装配 Step-up**不成立；**Redis 数据丢失只需重新登录且不会扩大权限**不成立；`S20` 的单次 `<3s` 与其自己的多槽循环证据相矛盾。
- 最重要的新问题是授权缓存的**过期后旧事件回灌**、单会话撤销的**事务外黑名单不可补偿**，以及 Redis 空恢复对一次性状态的影响。它们需要独立修复/恢复验收，不能由“固定 recycle 2s + requirepass”关闭。

## 2. D5 逐项裁决

| 编号 | 事实、因果与反证 | 定级与最小修法裁决 | 必要验收 |
|---|---|---|---|
| D5-1 | 成立。`FW:crates/yang-db/src/redis/config.rs:228-235` 把 idle 映射到 recycle；`REG:deadpool-0.12.3/src/managed/pool.rs:103-149,161-188` 先取 semaphore，再逐槽 recycle，超时返回 `None` 后继续；`REG:deadpool-redis-0.22.0/src/lib.rs:155-170` 在借出时 UNWATCH+PING。等待槽位的 wait timeout 不包住整个 get。 | 保留 P1 可用性风险；“约100分钟”是默认20个空闲槽全部半死的上界模型，不是现场时延。固定2s是有效缓解，但20槽仍可能约40s加创建预算，且借出后的命令仍无截止时间，不能写成登录/readiness整体已封顶。 | 模拟1槽和多槽半死连接，分别断言 recycle 和整个 checkout 的预算；另测已借出连接命令悬挂。同步更正“归还时”注释。 |
| D5-2 | 无认证/maxmemory的源码事实成立：`deploy/compose.infra.yaml:60-72`。历史 S01 记录测试机 PONG、maxmemory=0；不是本次线上验证。任意写 Redis 可伪造版本缓存，`request_validator.rs:90-95` 命中相等即放行。但当前应用未装配 Step-up，不能把删除 proof 标记列为现网可达链。 | P2，限定攻击前提。不能把“能访问Redis”与“已经能读backend配置”混为一谈：其他同网进程/网络误接入并不自动持有MySQL凭据。requirepass/ACL+最小网络边界成立；256MiB缺容量证据。Lua只增不减和EX已实现，不能阻挡绕过Lua直接SET的攻击者。 | 无认证访问失败、应用与维护客户端成功、拒绝错误密码；容量含黑名单90天TTL、验证码/限流基数、AOF/RSS余量；不能在唯一生产库运行FLUSHALL作验收。 |
| D5-3 | 成立：`FW:crates/yang-db/src/mysql/database.rs:209-219` 无session超时；schema advisory lock的30s只约束 `GET_LOCK` 等锁（`schema_sync/sync.rs:140-145`），并不约束后续DDL/MDL。`lock_wait_timeout` 默认值不是“一定挂一年”的现场证明。 | P2。after_connect配置三类预算可行，SELECT、行锁、MDL必须分别说明。不得把“SELECT语句超时”承诺成DML整体取消；innodb锁超时通常不自动回滚整个事务，调用方仍需显式rollback/drop。 | scratch MySQL分别测试只读长查询、行锁、MDL、异常连接初始化；验证事务失败路径确实回滚。数值按实际长任务与发布预算确定，不能同时写“固定5s”和“按p99.9定”。 |
| D5-4 | 成立：`FW:.../mysql/database.rs:260-266` waiting恒0；`FW:.../observability/telemetry.rs:373-389` 原样输出。已有open/available不能提供真实排队时长，但足够看基础饱和。 | P3观测缺口；删除MySQL waiting序列或明确unsupported即可，不必让共享PoolStatus大改。不要说整个池容量不可观测。 | 断言MySQL不再发布假waiting；Redis waiting继续真实发布；现有告警不被删坏。 |
| D5-5 | 事实成立：MySQL connect_timeout其实是acquire_timeout；Redis min/max_lifetime/test_before_acquire无映射（`FW:.../redis/config.rs:225-227`）。但`client.rs:74-82`已对非默认不支持值发WARN，所以不是完全无声。 | 文档更正P3；无需为改名破坏现网配置。旧键继续兼容；idle键不能alias到新的recycle语义，否则保留300s问题。 | 文档与实际build_pool_config对照；旧配置仍解析；若弃用要说明“接受但不生效”。 |
| D5-6 | **部分推翻**。5s缓存和fallback指标存在，但TokenAuthMiddleware先执行 `FW:.../auth/middleware.rs:188-196` 的 `verify_token_checked`，其Redis pipeline在`FW:.../token/revocation.rs:334-341`失败即返回；到不了authz的MySQL回源。因此Redis全失效时受保护API同样失败关闭。已有主键查询并不能证明真实负载下一定无回源放大；也没有负载证据证明“风暴”。 | 原始“性能改进”P3；认证依赖模型必须改正。不要加绕过主动撤销的进程正缓存。更严重的5s硬上限破坏见DI-01，不能用已有单调Lua草率关闭。 | Redis不可用时整条真实router认证测试应失败关闭；仅authz key miss时MySQL回源成功；独立测旧事件在TTL后回灌。 |
| D5-7 | 成立：`FW:.../tools.rs:199-234`串行共用池；管理面`telemetry.rs:232-239,299`有总预算。只有仓库中的发布检查消费者已确认，外部LB/探针本次未调查。 | P3。并发+每资源预算有价值，但两个依赖都必须成功；并发不会让真正池饱和“变健康”。“不驱动任何流量”应改为“仓库未配置自动摘流量”。 | 一项慢不吞另一项诊断；总预算内失败，恢复后变绿；保留业务池饱和信号。 |
| D5-8 | 应用指标有、Redis/MySQL服务器指标缺失成立；规则不能代替采集。历史S01记录测试机无采集容器，但不能由此证明无宿主agent/云监控，更不能推财务机。 | P3。先确定采集归属、报警接收与凭据，再部署最小exporter/云监控。不可仅加YAML后称监控闭环。 | 实际触发一项受控异常，确认采集时间戳、规则触发、通知送达；生产破坏性注入另需窗口。 |
| D5-9 | TLS feature缺失与在途命令无deadline成立：`FW:.../redis/client.rs:259-269`直接await query；deadpool Manager使用默认multiplexed连接，redis 0.32.7 `AsyncConnectionConfig`两个timeout是Option默认None。 | **拆分**：同宿主TLS仍可P3/backlog；response deadline是同宿主故障也会触发的P2可用性边界，不能等跨主机才做。无需先引入ConnectionManager架构；应先评估现有execute/pipeline/eval入口统一预算及取消后的连接处置。 | checkout成功后暂停Redis，GET/pipeline/Lua在预算内返回；超时后连接不会污染后续请求；后台任务停止有界。超时写操作标为结果未知，不能盲重试不可幂等命令。 |

## 3. D6 逐项裁决

| 编号 | 事实、因果与反证 | 定级与修法裁决 | 必要验收 |
|---|---|---|---|
| D6-1 | `request_log.rs:94-115,263-277`原样记录headers/query/body，日志先于认证；body展示上限64KiB不防短token泄漏。默认false；历史S01测试机false，财务机未知，原文“两台true”不能保留为当前事实。 | 条件P1，默认关闭环境为P2误配风险。环境无关的部署预检合理，但“不能默认脱敏，否则失去看报文用途”过强：token/Authorization/Cookie指纹或字段遮盖仍保留linkage和结构诊断；原文捕获应独立、短时、限制访问。部署预检必须跟有效配置优先级一致，不能只查TOML。 | 开启时人工明确选择原始捕获；普通日志没有机密；测试文件/env/secret覆盖路径和拒绝路径；只记录键名不回显秘密。 |
| D6-2 | options handler查source/field/datasource后验token（`approval_options.rs:382-464`）；nginx没有limit_req。token是随机摘要常量时间比较，现实缺口是认证前DB工作放大，不是可行的256位爆破。dispatch后段的飞书创建桶不能保护入口查询。 | P2。优先入口工作预算/边缘限流，应用层基于已验证真实IP。在二层代理后，容器nginx的remote_addr可能只是网关；不能直接套每IP桶后声称用户已隔离。`/api`全局limiter与“仅Feishu”是不同范围。 | 两真实用户各自桶、伪造头无效；不存在source与错误token均受限；共享NAT/飞书出口高峰正常调用可用。 |
| D6-3 | query-token成立：`email_delivery.rs:144-145`、`ResetPasswordPage.tsx:17-19`；nginx未覆盖access_log。默认combined是否实际写stdout应由固定镜像运行证明，本次仅源码。一次性、摘要存储、默认900s与no-referrer均降低但不消除日志泄漏窗口；reset TTL实际允许60..3600，不总是15分钟。 | P2。fragment+前端读取、旧query过渡可行；读取后history.replaceState移除凭据可以减少历史残留。必须包括管理签发链接生成者和模板fixture。`access_log off`仅是局部缓解。 | 旧query、新fragment均可重置且只消费一次；浏览器后退/刷新策略明确；容器及外层网关日志不含凭据。 |
| D6-4 | `user_token.rs:176-208`进程内Mutex HashMap、600s一次性消费成立。部署切换/重启会丢state，失败关闭；这不是持续多实例负载均衡事实。随机state失去后不会自动变成可接受的state。 | P3。Redis原子SET NX EX+GETDEL合理，但会增加Redis依赖，并要处理消费后token交换失败的重试体验；不应改为可重放的无状态HMAC。 | A签发B消费、并发仅一次、过期拒绝、Redis故障拒绝；不在日志输出state/code/token。 |
| D6-5 | 框架`FW:.../transport/client_ip.rs:152-184`确实先判TCP对端可信，再优先Forwarded，逆序剥除可信hop。TLS新代理若未列入可信链可能退到代理IP，触发依赖拓扑与实际转发头。历史S01的127.0.0.1不能自动适配未来共享gateway。 | P2条件性。先清客户端Forwarded/XFF污染，再只信实际代理来源；“补整个网桥段”可能过宽，在多项目宿主尤其如此，优先最小固定代理地址/专用网络。 | 真外层→内层→应用完整两跳测试；来自同网非代理容器的伪造头不能提升信任；维护端口直连保持安全退回TCP地址。 |

## 4. 新增缺口与反证

### DI-01：过期/清空后的旧授权事件重新建立陈旧授权缓存（P2，源码 + 隔离Redis复现）

证据：`src/infrastructure/authorization/version_cache.rs:12-24` 在current不存在时接受incoming并重置5s TTL；`worker.rs:79-88`发布事件携带的历史版本，不回查当前MySQL版本；`request_validator.rs:90-95`相等直接通过。`docs/architecture/authorization-freshness-adr.md:217-220`明确承诺数据库提交后5s硬上限，:282还承诺Redis重启/eviction/乱序失败关闭。

触发链：用户历史版本v2事件延迟；撤销提交v3，v3事件已发布；5s键过期；迟到v2事件（旧worker恢复、重试/过期租约）再SET v2 EX5；仍有效的v2 Access Token重新通过。无需攻击者写Redis。多个旧事件分批重放可在数据库撤销很久之后反复打开窗口。当前单调测试只测**键存在时**旧值被忽略（`version_cache.rs:213-222`），不足以证明硬上限。

反证与范围：不是任意伪造JWT；仍须有效签名、未过期、未黑名单、对应旧授权版本的token。全账号凭据变更若另有subject水位/会话撤销，可提供额外屏障；普通权限撤销不会使refresh失效。

最小修法：禁止Outbox把历史事件值当作新缓存真相；优先评估事件只使缓存失效、由MySQL校验填充，或用当前事实快照/持久水位保护。仅“加requirepass”“给SET加EX”“同值不延TTL”均不能解决。数据库回查后的填充也需限制从读取开始算的剩余新鲜预算，避免慢查询/暂停后回填旧快照再次延长窗口。

本轮动态证据：`python docs/assessments/evidence/2026-10-25-redis-version-replay-check.py` 已执行并 exit 0。脚本直接读取当前源码 `PUBLISH_SCRIPT`，在 `redis:7-alpine` 临时容器的 DB15 执行：v3键存在时拒绝v2；等待5.2s使键自然过期后，接受旧v2并建立新5s TTL。容器使用 `--network none`、无卷、无宿主端口，执行后已删除。该实测证明真实Redis执行当前Lua时会在过期后回退缓存版本，**没有执行HTTP认证或MySQL/Outbox集成链**；旧token被接受的完整影响仍由上述源码路径支撑。

修复验收：真实独立Redis + scratch MySQL中，v3提交超过5s，删除/等待缓存过期，再重放v2/重复事件，旧token始终拒绝；并发新旧worker、发布成功但ack失败、Redis恢复均覆盖。该端到端故障注入尚未执行。

### DI-02：单设备撤销提交成功后，黑名单失败/竞态可留下仍可用的Access Token（P2，源码确定）

证据：`revoke_session.rs:38-43`只查询未撤销会话的JTI，:51-75先提交revoked_at和成功审计，:77-88随后Redis拉黑；失败重试会在“已撤销”出口返回。`session/repository.rs:193-217`先读JTI并未持有贯穿refresh的锁。Access认证链只看Token撤销存储和用户authz版本，没有逐session revoked事实。该Action明确不递增用户版本（:79）。

触发链一：MySQL提交成功→Redis写失败→恢复Redis→旧Access不在黑名单，用户版本未变，继续可用到exp。触发链二：撤销先读取旧JTI，refresh并发签发/更新新JTI，撤销成功但只拉黑旧JTI。甚至无故障，已发生refresh留下的旧Access也不一定是session行当前JTI。

反证：`context.rs:537-554,589-598`已对后续refresh读MySQL会话状态，撤销行能兜住续期，因此不能宣称永久续期或DB撤销失效；受影响是仍未过期的Access（默认3600s，实际取配置）。现有`session_revocation_integration.rs:391`测试只证明refresh不依赖黑名单，未证明旧Access立即失效。

最小修法：把Access认证的稳定session撤销事实接入现有声明验证链，或实现可重放的持久撤销投递并处理所有在途/既有Access；仅重试当前JTI仍不能覆盖refresh竞态。保留逐设备语义；全用户authz递增会使其他设备Access也刷新，应明确接受后再选。成功审计应表达“会话事实已撤销/传播待完成”，不能混同已完成所有外部副作用。

验收：Redis首次写失败、返回后重试、refresh与撤销交错、多个旧Access同时仍在TTL内；目标设备全部旧Access拒绝，其他设备正常，后续refresh拒绝。

### DI-03：Redis不是可任意清空的纯缓存，空恢复能复活部分一次性能力（P2，恢复条件成立时）

证据：`FW:.../token/revocation.rs:288-295`轮换消费靠Redis SET NX；:334-352黑名单和subject水位缺失代表未撤销；`FW:.../token/manager.rs:899-918`消费后签发新对。应用refresh只验证会话仍active/凭据版本，不核当前refresh_jti是否等于来令牌JTI。`RUNBOOK_BACKUP.md:10-21`及`deploy/compose.infra.yaml:20`却把Redis称为纯加速层/丢了只需重新登录。

触发链：有效会话R1已换成R2，R1只在Redis被标记已消费→Redis丢卷/恢复为空或AOF尾部丢失→R1签名仍有效、MySQL session active、credential_version相同→再次SET NX成功，旧refresh可再用。subject撤销若仅在Redis也会丢；TOTP消费标记丢失可在容差窗口内重放同码。**不意味着所有撤销都绕过**：MySQL revoked session、禁用状态、credential/authz版本仍保护其各自路径；Redis网络不可达时也会失败关闭，问题是恢复为可达空状态。

最小修法：恢复剧本必须在重新放流前建立可验证的“失去消费历史即使旧能力失效”策略，例如恢复世代/轮换签名钥并移除旧钥、或持久化必要撤销/refresh-current事实。不是简单给Redis加备份；恢复旧备份也会回退一次性状态。将TOTP短窗口、旧JWT最长存活期写入恢复验收。

验收：隔离环境先消费refresh/TOTP并保存旧凭据，再重建空Redis；旧凭据拒绝、新登录可用；会话/授权不受影响的路径也要作正例。生产不得用FLUSHALL演示。

### DI-04：TOTP digits配置允许7/8位，但校验固定6位（P2，配置条件性）

证据：`config/mod.rs:1623-1625`允许6..8；`totp_setup.rs:52,59`把该值发到URI和响应；`totp_activate.rs:76-79`与`context.rs:231`用默认Verifier；`FW:.../auth/mfa.rs:82,111`写死6位。用户选择有效配置7/8，认证器正常产生的码无法通过激活。默认6不受影响，不能称为现网已锁号。

最小修法：当前未提出7/8位业务需要时，将配置收窄为6并修文档；若保留配置则Verifier、生成、激活、登录、前端格式契约一起支持。验收覆盖所有被允许配置的setup→activate→login闭环，不能仅测配置能解析。

### DI-05：TOTP密钥轮换说明把不可登录误写成可自行重新setup（P2，操作条件性）

证据：`mfa.rs:38-79`只有单把AEAD key且密文无key-id；`login.rs:102-109`先解密，失败直接返回，恢复码/邮箱分支在其后；`CONFIGURATION.md:261-262`却写“下次登录被要求重新setup”。setup是已认证Action，丢失原key后不能靠这段文档恢复。

反证：当前未证明用户已轮换/丢key；备份只恢复MySQL也不必然失败，若原key仍在即可。修法优先补备份范围与维护剧本：旧key单独受控备份，轮换前验证已有密文可解，在维护窗口重加密并验收；不要在没有迁移工具时声称直接换key安全。验收包括旧行、恢复码路径、备份恢复后的真实登录。

### DI-06：TOTP密钥域隔离与secret覆盖承诺不完整（P3，配置护栏）

`CONFIGURATION.md:252-254`承诺与Token/Step-up/邮箱密钥隔离，但`Settings::validate` :978-989只比较email.mfa/login与totp，`validate_totp_settings`只验证长度/占位/重复；未比较Token/Step-up/注册/换绑邮箱域。`config/source.rs:474-494`固定secret绑定没有totp.aead_key、email.change/mfa/login secret；既有文件<env<secret优先级本身正确，但不能说所有敏感配置都支持secret目录。

最小修法：复用已有跨域验证方法补缺失对照；补被正式承诺的secret绑定与默认/错误测试。未证实线上复用，不升级为可利用漏洞。目录权限、实际文件666仅属历史S01；新增部署预检必须检查最终生效来源，否则TOML检查与运行配置不同。

### DI-07：Schema锁正常错误会释放，取消路径没有同等保证（S14新增前置）

`FW:.../schema_sync/sync.rs:126-157`借池连接→GET_LOCK→await sync→RELEASE_LOCK；普通返回Err仍执行释放，是正证据。若Future在持锁await中被取消，普通PoolConnection drop归还池而不是确保关闭MySQL会话，显式释放段不会执行。MySQL advisory lock绑定连接；重新借到同会话还可能重入锁。当前生产启动未加该外层timeout，本轮不把“线上频发锁泄漏”当事实；但S14/S21若加启动deadline必须处理取消安全，不能直接timeout包sync。

验收要增加：隔离MySQL，在锁持有期间取消同步Future，另一独立连接能取得同一锁；部分DDL已提交的重启仍能收敛。最小方案是独立同步连接并在取消时确保销毁，或等价的取消安全锁持有者。

### DI-08：Redis连接日志可能输出认证URL，当前应用默认不可达（框架P3）

`FW:.../redis/client.rs:108-110`在enable_logging=true时原样打印url。框架默认false（redis/config.rs:71）；应用`Settings::redis_config` :930-938没有开启它。因此不是现网泄密结论。S57引入密码时仍应将框架该日志删为固定连接成功消息或使用可靠脱敏，测试用户名/密码/特殊字符不出现。不要为此新增应用配置。

## 5. 已有正确防线与排除项

- 业务事实、用户版本、Outbox事务原子性存在：`authz_version.rs:110-134,171-197,323-339`持锁/条件更新并同事务insert；版本溢出拒绝。Outbox有SKIP LOCKED认领、租约、worker_id确认及重试；不是“全部worker没有跨实例领取机制”。旧事件TTL后的问题仍独立存在。
- 审计成功写复用调用方事务（`audit/repository.rs:163-172`）；失败/安全决策允许独立审计（:175-187）。不存在证据支持把所有审计都称作异步尽力写。append-only是应用API/架构检查约束，并非MySQL超级权限无法篡改的WORM保证。
- Refresh并发轮换有Redis SET NX，TOTP按用户+解码secret+时间步原子消费（`FW:auth/mfa.rs:119-171`），恢复码有用户行FOR UPDATE（`repository.rs:415-465`）；不能重复报告历史“同码无限重放”已修问题。空恢复与正常运行是不同前提。
- **Step-up未装配**：本次`rg -n 'StepUp|step_up\(|with_proof_store|RedisStep' src --glob '*.rs'`只有配置类型/构造及测试数据，没有生产挂载调用。`account/user/mod.rs:62-69`是TrustedClientIp+TokenAuth；`totp_deactivate.rs:25-94`输入为空，直接走已登录身份。不能以框架具备RedisProofStore推定当前应用受Step-up保护。是否重新要求敏感操作重认证属于产品/授权口径，本审核不擅自恢复此前移除的功能；必须先修文档与防线清单。
- **头像没有文件持久卷/路径遍历缺口**：`infrastructure/schema.rs:271-291`明确user_avatar存MySQL Base64，非容器文件路径。纳入MySQL备份即可；不要新增对象存储/挂载作为本次修法。
- 配置源采用固定allowlist、敏感值Debug脱敏、单行/大小约束（`source.rs:592-609`），不是任意环境变量覆盖。未知YANG_SYSTEM_*拒绝启动是有用护栏；部署脚本预检不能绕开这套有效配置计算。

## 6. 相关S步骤逐条执行裁决

| 编号 | 裁决与必须修改的交付/验收 |
|---|---|
| S10 | 条件可执行。与数据身份有关的交付：凭据日志范围包含外层gateway，Feishu限流按真实代理链验收；不能把17m本身当安全收益。停MySQL测试只能隔离环境。将“仅Feishu”与“全部/api”选择统一。 |
| S11 | 可执行但不能称低风险纯配置新增：deny_unknown_fields使旧镜像读新TOML键失败，影响回滚。配置文件、环境绑定、show-config、文档、后端消费与旧版兼容必须同批列出；secret/env优先级要进入预检设计。 |
| S13 | **输入报告没有S13定义**；不可凭其他文档相似编号补造。若指特定步骤，应先修路线图引用。 |
| S14 | 方向成立，执行条件不足。同步已有GET_LOCK+全部预检，不需重复造锁。guarded分类要逐DDL类型定义，ADD COLUMN并非天然旧版兼容（required无default、表重建、锁时长）；计划检查和apply必须在同锁内重算，避免plan/apply竞态。补取消安全、非事务DDL部分成功恢复、旧版回滚反向MODIFY、schema与worker开关独立性。ready-after-bind需框架listener入口。 |
| S20 | 保留短recycle但重写验收：2s只是每槽，不是一次get；修完仍不能宣称Redis请求有界。将D5-9命令deadline同波排期或明确未关闭的风险。不要把idle旧键alias为新recycle。 |
| S21 | 三种session预算可行；“SELECT SLEEP(10)一定3024”需按实际MySQL验证，不把单条语句当全部工作取消证据。固定5000和按p99.9定二选一；模板/配置/非框架自建池范围需写清。MySQL等待指标更正可独立小改。 |
| S43 | 此步骤是飞书webhook，不是身份refresh。错误密钥/畸形JSON应诚实非成功，但400或5xx是否重试由上游合同确定；缺uuid的派生键应覆盖真实事件ID字段、实例/租户/时间等语义，不能仅哈希任意重排序JSON就承诺业务去重。详细链由飞书附件复核。 |
| S44 | 内存pending token只能缓解DB短错，不能关闭进程崩溃/双worker所有权问题。操作scope限飞书refresh，不能混同本系统JWT rotation。需要应用级持久化失败报警、不得把新token写普通日志，且业务保证明确为“仍需重新授权的剩余窗口”。 |
| S52 | state迁Redis有价值，P3；新增Redis依赖、消费后code交换失败的体验与配置隔离需覆盖。无状态HMAC不是等价替代。 |
| S53 | 可执行，但与S10相互依赖写法有环：S53依赖S10，S10又要求S53后再开详细日志。改为同批或S10先使用不含query的log_format。补读后清URL、管理签发、前端fixture、旧链接过渡按最大TTL3600s验证。 |
| S57 | 必须修改后执行。认证三步合理，密码用字母数字只是绕现有shell解析的临时约束，正式应URL解析/编码而不是永久缩窄密码。maxmemory=256MiB没有证据，与S59“按实测”冲突；noeviction达到上限时写会失败，登录/限流/MFA/撤销受影响，必须纳入故障矩阵。requirepass不解决DI-01/02/03。**删除在生产用ResetDb/FLUSHALL验收的要求**：只能在scratch验证维护命令；线上验证只读PING/readiness与权限边界。 |
| S59 | 容量评估应为S57内存值的前置输入，而非“先固定256MiB、之后测”。区分数据集maxmemory与RSS、AOF fork/COW、碎片、非淘汰安全键长期基数。限制后的OOM/写失败要有报警与回滚。 |
| S60 | 可执行；总预算和每探针预算明确，仍使用业务池。不把“慢依赖不再误报”当绝对收益：慢到服务不可用时正确结果仍是not-ready。 |
| S70 | 必须做，但备份范围需包含数据库加密凭据对应密钥的独立受控托管；MySQL+原key才能恢复TOTP/飞书密文。不能沿用Redis纯缓存论；空Redis恢复要配套DI-03旧能力失效策略。恢复验证不是仅SELECT计数，而是登录、权限撤销、MFA、审计、头像、飞书状态一致性。 |
| S71 | 凭据静态加密与key-id/轮换方向合理；不要复用飞书报文encryption_key作存储主钥，二者生命周期不同。密钥与备份同地不等于完全“加密没用”，但不能防同信任域整体失陷；说明实际防护目标。先确认已存密文迁移路径、双读单写/回滚窗口、旧key备份，再写批量迁移验收。 |

## 7. 本轮验证与交付边界

执行了CodeGraph/定向源码读取、上述rg范围核查、Git HEAD/status检查；对锁定依赖deadpool 0.12.3、deadpool-redis 0.22.0和redis 0.32.7读取了实际registry实现。另有本轮统一执行的 `python docs/assessments/evidence/2026-10-25-redis-version-replay-check.py`（exit 0）：真实隔离Redis + 当前生产Lua复现DI-01过期回退，没有运行HTTP/MySQL集成。附件静态核查确认14个D项、8个DI发现，且无行尾空白。所有结论明确区分实现事实、条件性触发链、历史现场记录和未验证环境。

为避免多个代理争用同一Cargo输出目录，本附件没有另跑Cargo、集成测试、生产故障注入或浏览器验收；若主报告有统一测试结果，应按原命令原样归档，不得把通过已有测试解释为新故障场景已覆盖。没有新增/修改生产代码，没有commit，没有修改现有脏文件。

最高优先验收顺序：DI-01过期后乱序重放 → DI-02单会话撤销与refresh并发 → DI-03空Redis恢复 → D5-1/D5-9完整Redis时延预算 → S14部分DDL/取消恢复。它们均可在隔离环境完成，且不依赖生产凭据。

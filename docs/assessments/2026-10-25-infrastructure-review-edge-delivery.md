# 基础设施对抗复核：入站、边缘、部署与恢复

日期：2026-10-25。审核对象为 `2026-10-25-network-infrastructure-audit-adversarial-review-and-roadmap.md`、其引用的 2026-10-24 原审计和当前实现。应用起始 HEAD `0adc5ab1cbd0729dfdf264fe617ae07a029ee585`，收尾 HEAD `5c0ea9cf8c1afe09f8a2cebe1354456c9c703bea`（其他会话正常提交飞书在制改动）；框架 HEAD `e27a2d3c127e0bea6c40bc5fa584b2fdb2162705`。没有回退或编辑其他会话内容。本文新增文件仅为报告和隔离复现材料，未 commit。

代码位置默认相对应用仓；`crates/` 相对 `D:/code/lib_yang`。生产事实文件仅作历史证据，本轮未 SSH、未访问部署端点、未读取生产配置或凭据。源码默认值、历史记录、隔离本机复现分别标明，不能互相替代。

## 结论

原复核不是可直接照抄的执行计划。动态复现证明：**当前并发排队受外层 Timeout 约束，会返回 408；默认 404/405 也经过 TraceLayer。** 把 LoadShed 分开叠进 Router 层会静默失效。另有三项修法风险必须先改：0600 配置可能拒绝 UID 10001 的容器读取；9090 单探针不能证明 nginx 可用；独立只读 schema plan 后再调用 sync 存在重新计划窗口，不能保证拦住 DDL。

备份、部署身份、停止失败后仍清库、探针忽略 curl 失败等遗漏，比重复列举 nginx 默认参数更值得进入前排。无需据此把所有项目升为 P1；下文按触发条件定级，未把潜在漏洞写成已发生事故。

## D1、D2 全量裁决（21/21）

这里的裁决针对原复核的事实、因果和修法整体，P 级为本轮建议；生产可利用性另有历史证据而无本轮现场确认。

| ID | 裁决 / 建议级别 | 证据、反例与最小修正 |
|---|---|---|
| D1-1 | 部分成立 / P2，有公网业务争用时可升 P1 | `src/addon/feishu/portal/mod.rs:54` 仅认证中间件；框架 `transport/client_ip.rs:205` 无扩展即用 TCP peer。因此当前 nginx→backend 拓扑下按门户共享 IP 桶成立，直连部署不必是 127.0.0.1，更非全站一个桶。补可信 IP 装配，同时验真实代理链与头清洗，不能只注入 XFF 测试。 |
| D1-2 | 部分成立 / P2，容量数据决定是否 P1 | `crates/yang-base/src/transport/axum.rs:560` 显式 `to_bytes(...max_body_bytes)`，只改 DefaultBodyLimit 无效；`:324` per-path+method 独立 semaphore，经本机跨路由/同路由双方法复现。4 GiB 是单层原始缓冲模型，JSON 对象等还有额外分配，未实测 OOM。先盘点合法 upsert body，再分 JSON/multipart 限额；全局并发与限额同测。 |
| D1-3 | 部分成立 / P2 | `nginx.conf:68` 暴露业务 readiness；`axum.rs:400` 调共享池健康检查，无独立 readiness budget/cache/auth。却并非绝对“无预算”：`:326` Router Timeout 也包它，默认 60s，外部 nginx 非活动超时不是服务器取消工作的保证。可关闭边缘精确路径，保留管理面，先盘点外部探针。 |
| D1-4 | **关键因果不成立** / 背压问题仍 P2 | 当前先 `with_state` 再分别 `.layer(ConcurrencyLimit)`、`.layer(Timeout)`（`axum.rs:312-330`）。axum Route 永远 poll_ready=Ready，call 内 oneshot 才等 permit。实测两个 450ms handler、并发 1、650ms timeout，第二请求 658ms 返回 408。快速卸载仍有价值；HandleError→LoadShed→GlobalConcurrencyLimit 必须在同一 ServiceBuilder 内构造；分开 Router 层实测仍排队到 200。 |
| D1-5 | 成立 / P2 | `axum.rs:327` 设置空体 408，Action request id 仅在 `dispatch_request` 内生成（`:448`）；404/405 不经 Action。统一信封应保留 HTTP status、Allow、request id；超时选 503/504 必须定义清楚，取消 Future 不等于外部副作用回滚。 |
| D1-6 | 事实成立，修法需限定 / P2 | `nginx.conf:68-83` 无显式代理预算，默认值仍有作用。`proxy_request_buffering off` 会让慢客户端长时间占 backend/body 解码槽，不能当通用导入优化；先衡量磁盘缓冲与内存/连接预算。300s 非活动超时并非总 deadline，上传 body 与响应阶段分开验。 |
| D1-7 | **部分成立，fallback 断言错误** / P2 | 默认 Trace DEBUG 与 filter 不匹配成立；但 axum 0.8.9 `routing/mod.rs:311-315` 明确 layer fallback_router，复现 404/405 均进入 TraceLayer（计数 2）。无需为了让 Trace 覆盖 fallback 才注册 handler；INFO 访问日志与统一错误信封是独立需求。日志需控制 query/token 泄露。 |
| D1-8 | 成立但时间结论无证 / P3 | `src/bootstrap.rs:372` 提前 ready，框架 `transport/axum.rs:200` 后 bind。bind 失败会退出不足以证明窗口“亚毫秒”；还受调度、构路由和清理影响。先 bind 再 ready 是合理不变量，但可与其他启动修复拆开，不应阻塞 TLS。 |
| D1-9 | 成立 / P2 | `nginx.conf` 无 limit_req/limit_conn；需要各自 zone，默认拒绝状态未必 429，须显式配置。只对 API 加粗暴每 IP 限流也会合并飞书出口 NAT 流量；不同入口预算与突发业务应验收，静态资源另保留正常突发。 |
| D2-1 | 条件成立 / 历史公网明文证据支持 P0，非本轮现场结论 | `nginx.conf:46` 无 TLS；S01 历史记录 `:68-84` 两机 HTTP 200/TLS 握手失败。不应因 Docker 仅 loopback 就自动降 P3：外层可能仍明文代理或不安全隧道。按实际端到端传输判定；HSTS 在 HTTP 上不能补救。 |
| D2-2 | 条件成立 / P2 | `nginx.conf:79-82` 未清 Forwarded；框架 `client_ip.rs:159` 优先处理受信 peer 传入的 Forwarded。门户未装 resolver，因此目前受影响主要是已装 resolver 的账号等入口。最外层清洗、内部按实际可信代理链追加；只写“XFF继续追加”不足以证明解析能穿越两层。 |
| D2-3 | 风险成立，必然性夸大 / P1 条件项 | `deploy-blue-green.sh:330` 共享 netns；`:722` 有历史滞留解释。实际 Docker/重启路径需演练，不宜断言每次 restart 都失败。先监测 backend readiness 与应用直接发布端口，确认 netns 身份再有限次修复；Docker unhealthy 本身不会触发 unless-stopped 重启。 |
| D2-4 | 成立，修法不完整 / P1 条件项 | `bootstrap.rs:256` 无条件 sync；框架 `schema_sync/sync.rs:62-84` 只读 plan 无锁，`:140-152,179-195` sync 获锁后重新规划并执行。外面审一次 plan 不能保证实际执行仍符合策略；门禁必须作用在锁内真实计划。加 NOT NULL 无默认列、唯一/FK 约束也未必兼容旧写者，不能把所有 ADD 都当安全。 |
| D2-5 | 成立，但“可重试”过强 / P2，外部不可逆写时 P1 条件项 | `deploy-blue-green.sh:337` 默认 stop，镜像未配置 STOPSIGNAL/stop timeout。30s 关闭预算可能被 Docker 10s 截断；已提交 DB/已创建飞书实例的请求不能凭“可重试”降险。创建时设置 stop timeout，关闭链独立验证；worker 300s 不是自动要求 Docker 等 300s。 |
| D2-6 | 部分成立 / P2 | 空 CIDR 是安全默认且支持直连，不能一概文案“全站单 IP 桶”；在反代拓扑才发生该退化。历史 S01 `:44` 写测试机 loopback 受信，但财务未知。本轮不确认现值。告警应条件化，避免所有 production 直连都报误配。 |
| D2-7 | 仓内配置事实成立 / P2 | `start_pair:320-333` 与 compose 无显式资源限额/日志选项；不能推导运行时无日志轮转，S01 历史 `:62` 有 daemon 100m 设置。先限制日志与测容量可独立进行，不必排到 Redis 认证/TLS 全结束后。低内存限额可能制造故障，MySQL/Redis 需分别定额。 |
| D2-8 | 成立 / P2 | `cmd_cutover:1254-1265` 停蓝后重建新 live，健康失败 `:1324` 仅提示。应覆盖任一 Docker 操作失败的恢复状态机，而非只最后 health_check 分支；自动回滚也不能盲目跨不兼容 schema/config。 |
| D2-9 | 事实成立，属重复维度 / P2 | 与 D1-6/D1-9 高度重复；默认 nginx access log 存在，包含 `$request` query 的风险比“无log_format”更具体。分离预算/缓冲/限流/脱敏四个验收，不能把显式写默认值算可靠性改善。 |
| D2-10 | 成立 / P2 | contract 检查源码，`resolve_bind_addr` 支持沿用旧绑定。post-cutover 断言太晚，先计划拒绝再 verify；IPv6 `::`、具体公网 IP、绿色 smoke 端口、回滚绑定均要覆盖，不能只匹配 0.0.0.0。 |
| D2-11 | **部分成立，域名硬前置错误** / P2 | 证书续期/告警应完整，HTTP/2 是性能能力而非 TLS 完成条件。TLS 可用 IP SAN/私有受信证书/非443端口，公共 CA 的产品约束应查当期支持，不能一概断言“不签裸IP”。域名、DNS、ICP 不是协议统一前置；具体飞书/浏览器兼容性另验。 |
| D2-12 | 盲点成立，**推荐修法不充分** / P2 | `frontend/deploy/Dockerfile:33` 只查 SPA，后端坏可假绿；改为只查9090又会在 nginx 坏时假绿。保留8081前端证据，加内部代理/后端证据；若对外封ready，可设内部专用代理健康路径或双探针，验nginx停、后端停、依赖停、netns滞留四种情况。 |

## 路线图逐条裁决

按原文实际编号审阅；没有虚构 S34–S39、S55、S61–S69、S72–S73，原文没有这些步骤。它们不能在覆盖率中按“已审”虚增。涉及其他域的步骤仅裁决交付依赖与恢复部分，业务语义由对应域报告负责。

| 步骤 | 裁决 | 必须调整的依赖、风险或验收 |
|---|---|---|
| S00 | 修改 | 共享脏树不应替他人 stash/commit，隔离 worktree 并记录两仓 commit/dirty manifest 即可；“git revert 基线 commit”可能撤销本来业务成果，不能作无差别回滚方案。 |
| S01 | 保留并补证据边界 | 不依赖全量绿色基线即可先查事实；历史无监控容器/用户cron不等于无宿主服务、云备份或外部采集。补备份存储、有效配置来源、镜像 digest、进程UID/文件UID/ACL；仅PING不能证明requirepass配置，但可证明当前调用者是否匿名获准。 |
| S02 | 重写范围 | “阻断登录”与“匿名账户API允许”矛盾；OAuth code、门户口令、dispatch token仍在明文。公开路径凭据须逐项定义，SSH隧道/VPN/TLS可并行，不能声称白名单是唯一缓解。测试机历史已有共享网关，先解决它的容器网络到loopback服务可达性。 |
| S10 | 分解验收 | 9090单探针不算前端健康；补双探针。与 S11/S22 同步 body限额；设置明确429/Retry-After策略并实测飞书NAT突发。request_buffering不默认关闭；gzip不是“去掉明文风险”。 |
| S11 | 修改 | 新 serde default 只保证新代码读旧配置，旧二进制 deny_unknown_fields 未必能读新键，回滚必须带匹配配置。schema策略/worker开关不能只加配置而遗漏调用点；1MiB上限需合法大JSON样本。 |
| S12 | 条件通过 | 90/75/60是不同计时域而非严格数学阶梯，上传先缓冲、nginx按非活动、body流读取均要覆盖；330/300/300内两层相等有竞争。取消POST不能证明服务端取消；长派发应优先入队，不能无条件扩大300s同步预算。 |
| S14 | **门禁方案须改** | plan与sync之间TOCTOU必须消除；锁内分类待执行DDL。不能泛称ADD安全；default/nullable/index/FK和旧写者兼容性分测。plan模式若仅打印后启动旧schema应用会运行失败，应明确仅用于离线预检或验证无差异后启动。snapshot恢复会丢发布后新写，不能写成自动无损回滚。 |
| S22 | **修法组合须改** | 动态证据确认排队会408、fallback已Trace覆盖；LoadShed须同一ServiceBuilder连接limit，不能再分Router层。跨两个不同path及同path不同method均应断言全局限额；健康豁免要控制匿名昂贵readiness。 |
| S28 | 条件通过 | 清洗须先于可信IP装配；登录门户和匿名门户流量契约分别验，最外层真实源地址是否可信需现场确认。 |
| S30 | 重写硬前置 | 可先做TLS模板，不强制S02先落或整个S14完成。当前共享网关是独立netns，`127.0.0.1:18654`不指向宿主；连接yang网络会改变Redis/MySQL可达边界，须设计并验。网关实际1.27、应用镜像1.30需各自 nginx -t；公开路径非404并不足，要校验方法、预期鉴权和业务信封。 |
| S31 | 保留但前移 | 不应让一个已认定P0等11周所有非阻塞功能完成。production切换、新cookie安全属性、OAuth redirect、Host/Origin/端口、Forwarded/XFF及旧链接迁移逐项验；证书与端口可选并非只能443。回滚优先保留TLS，重新开放明文不得称低风险。 |
| S32 | 保留独立窗口 | 复用已验机制但不能复制测试机运行事实；财务外部80/443超时不能推出只因安全组，无监听/路由/主机策略也可能。无需将制品身份等低风险工作硬依赖财务TLS完成。 |
| S33 | 修改 | 看门狗须部署互斥锁、有限重试/退避、失败告警、维护抑制；管理面200+宿主网关失败不能直接重启前端。先尝试最小双容器重启约束或独立网络服务发现，再决定长期watchdog；“每次必复发、60s自愈”待现场演练。 |
| S52 | 条件通过 | Redis state扩大共享和可写信任面，应先满足Redis认证/隔离及原子consume预算；600s TTL、前缀须含deployment隔离，不能仅一个全局前缀。重启成功不证明跨实例安全，需并发consume和Redis故障fail-closed测试。 |
| S53 | 通过并补限制 | fragment仍可在历史/浏览器扩展中出现；既有query链接需限期兼容并及时清URL。上游网关/CDN/logShipping同样要清query。 |
| S56 | **扩大覆盖** | 需发布互斥、immutable image IDs、匹配配置快照、停止失败即停止破坏操作、完整恢复状态机；StopTimeout字段是 `.Config.StopTimeout`，不是注释中的 HostConfig。smoke失败回收绿对并不能撤回其已做DDL/worker副作用。 |
| S57 | 修改验收安全性 | 认证迁移顺序合理；生产验收不能为证明FLUSHALL而执行ResetDb，改隔离Redis测试。256MiB为待测值；noeviction饱和会使验证码/限流写失败。只加requirepass防不了持有应用密码的同网络主体，scope/ACL另定。 |
| S59 | 分离并前移日志轮转 | 日志保留不必等S57；历史有daemon100m需先查有效值。swap、pids、CPU配额可能造成延迟/线程创建失败，需各自压力边界；暂停/限额动作只能隔离环境或授权窗口执行。 |
| S60 | 修改验收 | “两个1.5s可ready”与“单个5s在500ms失败”若共用固定500ms每探针预算互相矛盾；先定义单探针预算与总预算再测试。池饱和应not-ready但不必驱动流量，部署探针重试要有总deadline。 |
| S70 | **提高优先级并修运行手册** | 把失败输出、credential传递、原子备份发布、加密校验、off-host、恢复隔离一并验证；逻辑日备不能宣称RPO5分钟。备份还需版本匹配配置、密钥托管、网关证书/配置、镜像身份；恢复应用先禁worker并限制出网。 |
| S71 | 修改回滚声明 | 长期双写明文不满足“活凭据离开明文仓库”；明确迁移过渡截止与旧备份保留。旧快照的OAuth refresh token可能已被后续刷新失效，恢复快照不保证凭据可用；密钥与备份独立托管仍需真实恢复演练。 |
| S74 | 保留 | fail-open/closed必须按具体控制动作定义，Redis并非纯缓存；恢复/清Redis时proof消费和会话撤销影响也应纳入。只写文档不能算行为验证。 |
| S75 | **前移并补可验证身份** | 不必依赖S32；两仓HEAD label不标dirty仍可能说谎，本机build未经过CI gate。绑定源码内容/clean commit+依赖commit、最终digest、测试记录及配置版本；smoke和cutover同digest。actions也pin SHA，基础镜像可移动tag与Rust advisory扫描分别治理。 |
| S76 | 条件通过（交付部分） | 恢复后的对账应先dry-run，禁止scratch环境凭生产凭据创建审批/发通知；安全修复不能依赖尚未实现的对账承诺。 |
| S77 | 条件通过（交付部分） | 保留期与备份恢复边界须批准，删除后不能用整库覆盖恢复而忽略期间新写；先限批/计数/恢复抽样。 |

## 新增或原文未闭合的发现

### E01：配置权限修法会使非 root 后端失去读取权（P1 条件项）

- 证据：`docker/app/Dockerfile:43-44,71` 固定 UID/GID 10001；`deploy-blue-green.sh:325` 单文件只读 bind mount；S01 历史文件 `:51,116` 建议 yjj:yjj 的666直接chmod600并声称容器不受影响。只读挂载不绕过Unix DAC。
- 触发→行为→后果：宿主属主不是10001、无有效ACL/idmap → 改0600后新容器读配置被拒 → restart/cutover crash-loop。反证：若owner就是10001或存在适当ACL则不触发；本轮未查线上数字UID，不能声称已故障。
- 最小修复/验收：先只读确认容器有效UID、宿主数字owner/group、ACL/userns；选择受控组0640或精确ACL/secret挂载，让部署者可写、应用只读、其他本地用户不可读写；scratch按相同映射启动，并以实际UID测试read，禁止用root读成功代替。
- 现有脚本也有缺口：`:835,845,1407` chmod失败被忽略，`:856` 原地写保留正式文件已有666；暂存通过scp上传到远端才chmod（`deploy.ps1:476`），传输中断会留文件。需创建时即受控权限，失败即中止，并保护正式文件/备份/暂存各自访问主体。

### E02：健康探针丢弃 curl 失败，半响应也可“通过”（P2）

- 证据：`deploy-blue-green.sh:355-358` 用 `|| true` 丢掉rc并接受先收到的200；`:383-407` 用它验收发布。隔离stub验证curl exit28且输出200，probe返回200。
- 触发→行为→后果：服务先返回200头但body挂死/截断，或本地输出写失败 → probe仍成功 → cutover/rollback可假绿。反证：拒绝连接通常输出000，因此不是所有故障都漏检；当前ready body很小降低概率但不改变错误契约。
- 最小修复/验收：同时要求curl rc0及状态符合，ready校验响应body语义；注入200后半body、连接重置、超时、正常200；所有传输失败均判失败。修复也会纠正原注释为规避假阴性而造成的假阳性。

### E03：应用没停下来仍继续 DROP DATABASE（P1 条件项）

- 证据：`stop_pair:337` 吞停止失败，`stop_all_pairs:629-630` 只warn；`cmd_refresh:1446-1450` 随后调用reset，`:1063`执行DROP。隔离函数替身验证live仍running时返回0。
- 触发→行为→后果：daemon停止调用部分失败/容器自动重启 → 旧应用继续访问或重新同步schema → 清库期间写入/外部调用和重建竞态。反证：已有RESET确认、MySQL/Redis目标护栏有效，可防未确认或错误host；它们不能证明所有写者已停。
- 最小修复/验收：破坏动作前强制所有受管写者为停止状态，不可确认即失败；必要时DB写凭据/所有者维护闸门，不能仅按容器名断定无其他写者。stub停止失败应断言没有调用mysql_exec；真实隔离环境验证停止状态复核。

### E04：smoke 验过的制品不等于 cutover 启动的制品（P1 条件项）

- 证据：`start_pair:327,333` 使用可移动镜像变量；`cmd_smoke:1231`起绿；`cmd_cutover:1248-1265`只验绿后再次用当前tag建live；`deploy.ps1:583-594`按工作树build/export；CI `:12,22-36`依赖浮动master并无发布制品交接；部署脚本无操作互斥锁。
- 触发→行为→后果：smoke后另一发布docker load/tag、脚本或配置替换 → 上线未经smoke的镜像/配置；并行cutover还能交叉删除蓝/绿。反证：单操作者串行且不改tag时不触发；SSH传输加密解决不了身份与并发竞态。
- 最小修复/验收：发布锁覆盖smoke到cutover的状态变更；记录绿容器实际image ID和配置内容版本，cutover按同ID构建，验证ID一致；记录两个仓版本和dirty事实。故意改变live tag后cutover应仍用已测ID或拒绝；双发布应拒绝第二操作。

### E05：镜像回滚没有恢复匹配配置，破坏旧版启动契约（P1 条件项）

- 证据：`deploy-blue-green.sh:325`所有版本挂同一路径；`install_staged_config:856`原地覆盖；`cmd_rollback:1330-1375`只改镜像/容器名，无配置回退；`src/config/mod.rs`多处deny_unknown_fields。刷新失败恢复备份不是release回滚配置绑定。
- 触发→行为→后果：新版本配置增加旧版不认识的键、改DB/密钥/OAuth参数 → 回滚旧镜像读取新配置 → 启动失败或身份/数据目标错误。反证：无配置变化、双版本兼容键时镜像回滚可工作；已有refresh备份不自动等于可选正确release版本。
- 最小修复/验收：发布manifest绑定两镜像ID、配置版本与schema兼容状态；回滚前用旧版校验匹配配置，保留受限备份；新键配置→旧版rollback演练，保证readiness及目标库身份，不能只看容器running。

### E06：备份手册可能生成“成功但空”的备份（P1 恢复前提项）

- 证据：`RUNBOOK_BACKUP.md:40-47`宿主export MYSQL_PWD，但compose exec没有显式传入，且dump|gzip没有pipefail；`:63-65`binlog命令缺持续跟随开关、输出默认留容器工作目录；`:99`恢复后直接启动应用。本机false|gzip|gzip -t返回0复现了失败掩盖。
- 触发→行为→后果：密码没进入容器/权限不足/连接断 → dump失败、gzip仍生成可校验空文件且pipeline成功 → 自动任务记录成功，灾难时无恢复点。反证：容器已有合适客户端配置或导出变量时认证可成功；它不能保证pipeline错误传播。`--single-transaction --source-data`还可能短暂全局读锁，不能宣传绝对“不锁表”；DDL并发亦须避开。
- 最小修复/验收：显式受控credential注入，pipefail、临时文件+成功后原子发布、SQL非空/完整性检查、加密后验证和off-host成功确认；故意坏密码/中断dump/上传失败均不得更新last-success。恢复到scratch并校验业务行/关键状态，默认禁worker与出网，避免恢复环境向真实飞书发通知/创建审批。日备与持续binlog的RPO分开承诺。

### E07：恢复资产范围和证据不足（P2；无可用备份时影响P1）

- 证据：`RUNBOOK_BACKUP.md:9-12`仅列MySQL/Redis；`:72-74`只描述加密/off-host要求，无实现；compose `:25,61`移动tag；部署镜像无revision label，网关证书/配置在历史S01 `:33-34`位于应用外部。头像已核实存数据库（`upload_avatar.rs:1,67-70`），所以没有虚构“头像本地卷遗漏”。
- 触发→行为→后果：主机/磁盘丢失 → 仅SQL无法重建正确二进制、签名/加密密钥、飞书凭据配置和网关证书 → RTO不可证明或部分密文不可读。反证：外部托管可能已有这些资产；本轮未现场验证，结论是仓内恢复合同未覆盖，不是断言线上不存在。
- 最小修复/验收：资产清单包含两仓版本、镜像digest、非密配置模板与独立托管密钥、证书恢复、DNS/端口/网关配置、应用状态；在新宿主形态的隔离环境完整恢复。恢复旧DB会回退撤销/凭据事实，令牌epoch/密钥失效策略必须单列，不能把“Redis丢失只需重新登录”当安全证明。

### E08：复用容器 shared-gateway 与应用 loopback 发布之间缺一跳（P2 设计阻塞）

- 证据：S01历史 `:32`网关不在yang网络；`:22`应用当前发布18654。S30拟复用该容器，S02/S31拟把应用改仅宿主127.0.0.1；`start_pair:322`确认发布由backend容器拥有。
- 触发→行为→后果：照宿主nginx模板在gateway容器proxy_pass 127.0.0.1:18654 → 访问网关自己的loopback而非宿主 → TLS握手正常、所有应用请求502。改host-gateway也不能自动访问只绑宿主loopback的端口。
- 反证：真实gateway若采用host网络或额外代理桥可达，但历史记录显示独立Docker网络，本轮未现场验证。
- 最小修复/验收：明确选一种可达拓扑：受控容器网络直达backend:8081、host网络或受限宿主入口。若将gateway接入yang-system-net须重新评估到Redis/MySQL的可达性与应用信任CIDR；从真实gateway netns完成HTTP/API/TLS/头链验收，不能用宿主curl替代。

### E09：日志和错误输出的秘密边界尚未完整（P2 条件项）

- 证据：`nginx.conf:68-83`没有query脱敏log_format；默认$request包括OAuth code/reset query；`deploy-blue-green.sh:233`非法MySQL scheme错误直接打印整个URL；配置暂存/备份权限失败被忽略（E01）。
- 触发→行为→后果：OAuth回调/旧重置链接或含userinfo的错误URL → access/deploy日志保留敏感串 → 被日志读者获得。反证：一次性code短TTL、日志可能受限，不能把已泄露后可持续接管当无条件事实；默认日志不记录body，不应泛称所有token都漏。
- 最小修复/验收：URI日志只记安全path与固定分类、非法URL只报键名/错误类别、gateway同样脱敏；用假token/假密码跑错误路径搜集日志并断言不出现，禁止以真实凭据验收。

## 实际验证与未验证边界

可重跑源码、锁定依赖和命令/完整输出见 [evidence-edge-delivery-2026-10-25/README.md](evidence-edge-delivery-2026-10-25/README.md)。Rust隔离复现覆盖timeout排队、LoadShed层组合、404/405 Trace、per-path/per-method限额，以及take-then-await被timeout取消后Drop仍不能终止任务的反例。shell替身覆盖curl失败假绿、停止失败仍返回成功、备份管道错误掩盖。部署contract（含8个变异）、CI runner自检和bash语法检查通过。

没有跑全量应用/框架测试、真实MySQL/Redis集成、Docker故障演练、生产压测、TLS签发/续期、飞书端到端或现场配置验证；以上本机结果只证明相应机制，不证明当前生产故障频率。未修改生产实现或部署状态，新增报告与证据仍待主审核汇总。

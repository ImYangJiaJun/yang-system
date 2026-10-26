# 全基础设施复核附录：运行时、可观测性与浏览器会话

- 日期：2026-10-25；应用最终基线 `5c0ea9cf8c1afe09f8a2cebe1354456c9c703bea`，框架 `e27a2d3c127e0bea6c40bc5fa584b2fdb2162705`。
- 对象：同日 network-infrastructure-audit-adversarial-review-and-roadmap 的 D7、D8、相关步骤及未编号结论；同时检查新增基础设施面。本文只提交审计材料，不实施修复。
- 源码通过 CodeGraph 与缺口定向读取核对；动态证据只包括本地无网络自检。生产现状引用 S01 时属于历史采集记录。
- 裁决中“保留”指代码缺口存在，不表示原文所有定级、数值、解决方案均正确。框架路径相对 `D:/code/lib_yang`。

## 1. D7 逐条复核

| ID | 裁决 | 证据、反驳与修正 |
|---|---|---|
| D7-1 | 部分保留 | `src/config/mod.rs:853` 的默认 filter 确实压掉框架 INFO/WARN；`crates/yang-runtime/src/observability/logging.rs:162-164` 在 `next.run().await` 后记数，取消会漏分母。401 进入该中间件，不应排除。**404/405 不经过 Action 不等于不经过 TraceLayer**；本轮边缘附录的精确 Router 层序复现证明两者会触发 trace。增加 INFO 前还须检查 URI/span 脱敏，不能用更多访问日志换取凭据泄漏。 |
| D7-2 | 保留但限定 | 业务探针与管理面预算/gate 不一致，详边缘附录。源码只证明入口，S01 才记录当时公网可达；本轮未刷新该事实。对外封闭 readiness 可独立做，不能因此把前端自检换成仅直连 9090。 |
| D7-3 | 保留并扩展 | `src/bootstrap.rs:393-400` 仅监督 HTTP/关闭预算；应审 **7 个 worker 加管理面任务**，不止 4 个 Feishu worker。授权 outbox、权限到期、会话清理同样没有运行期 JoinHandle 监督。`telemetry.rs:163-174` 管理面 task 仅保存。panic 通常仍有 stderr，不能说完全无日志。worker_alive 的常量 1 也不能发现挂起，另需最后成功时间/进度。 |
| D7-4 | 原问题保留，原修法不充分 | 4 个 Feishu Drop 只发退出信号确实不保证中断单轮；更深问题是所有 worker 的 `shutdown` **先 `self.task.take()` 再 await**。外层取消 shutdown future 时，`self.task` 已为 None，复制已有 `Drop::abort` 仍无效。详 R-01。异步 abort 也不能中止已开始的 `spawn_blocking`。 |
| D7-5 | 保留，修正 matcher | `telemetry.rs:32-34,109-111` 全局桶最大 10 秒。原建议 `Matcher::Suffix("_duration_seconds")` 不是“仅 action/import”，也会命中 readiness/shutdown 等同后缀指标；应按具体指标名或明确分类配置。传播延迟维持短桶；12 秒观测应验证对应 metric 的桶，而非只验证 `+Inf == count`。 |
| D7-6 | 部分保留 | pull/approval 的 unbounded channel 成立（`feishu_pull.rs:120`、`feishu_approval_worker.rs:156`）；Burst 属于 approval/event 的 `interval`（`:340`、`feishu_event_worker.rs:157`）。pull 已使用完成后 `sleep_until(now+interval)`（`feishu_pull.rs:275-293`），OAuth reminder 也如此，不能全称“所有 interval 都 Burst”。合并触发必须先持久化 applicant/config 语义，不能把用户请求静默丢弃。outbox 的退避不能破坏授权 5 秒硬契约。 |
| D7-7 | 保留纠错，收紧外部事实 | 文件实有 **13 条 alert**；没有仓内采集部署不等于全环境没有外部云监控。S01 只查了容器、部分 timer/cron，不能排除系统服务或 off-host scraper。规则之外还缺 scrape target down、worker 无进展、探针本身不运行等缺席证据，详 R-04。 |

## 2. D8 逐条复核

| ID | 裁决 | 证据、反驳与修正 |
|---|---|---|
| D8-1 | 保留 | 完整 nginx 配置没有 gzip，镜像覆盖配置；现有 gzip 预算不能证明部署压缩。原文 931670/280007 字节与 3.33 倍是旧构建快照，本轮不重新引用为当前实测。测试应同时确认 `Content-Encoding`、`Vary: Accept-Encoding` 和不可压缩/已压缩响应，不能只做配置字符串存在断言。 |
| D8-2 | 保留，否决固定阶梯作为通则 | `engine/http/client.ts:27-31,60-63` 仅透传 signal；`engine/session/lifecycle.ts:35-50` 与 portal 公共请求也无自身有限 deadline。浏览器/TCP 最终可能断连，因此准确表述是“没有应用承诺的最长等待”，不是所有连接永不失败。90/75/60 或 330/300/300 不是正确性证明，后两层相等还会竞争；连接池等待、刷新锁、body 读取须计入一次端到端预算。未知结果写操作不可自动重试；显式幂等 POST 需按操作语义决定。详 R-03。 |
| D8-3 | 保留 | `features/account/api.ts:50,156,310` 和 `admin-api.ts:41,80` 五处未包 refresh；其中 postAuthenticated 影响多种写操作。`fetchCurrentUser:79` 已包装。修复复用已有 wrapper，保留 Step-up/凭据变更协议；401 重发应只在鉴权先于副作用的合同下进行，并保留调用方 signal。 |
| D8-4 | 现状保留，修法部分否决 | `http.ts:16-25` parse 失败丢失 status；`action-response.ts:64-74` 204 被拒属实。当前 Action 合同要求 JSON 信封，**不能把任何 2xx 空体全局视为成功**，那会把截断/错路由响应吞掉。仅声明允许无体的操作接受 204；保留 status/request ID，响应 body 摘要须限长、脱敏且以文本呈现，不能直接展示任意网关 HTML/秘密。 |
| D8-5 | 保留 | `session-controller.ts:85-107` 把瞬态异常固化 anonymous，后续恢复短路；`auth-session.ts:108-120` 本已区分终态。需要可重试状态和 UI，不是一行 catch。原文 §4.3 又称“一行修复/P1”与 D8-5、S12 自相矛盾。另有更直接的迟到响应覆盖会话问题 R-02。 |
| D8-6 | 部分保留 | `shell/App.tsx:25` queries retry:1 会重试确定失败；mutation 未被该设置全局重试。token queryKey 会带来轮换重拉，但它同时提供身份隔离；去掉必须用稳定 user/session scope 替代并取消旧请求，不能只依赖某时刻 `queryClient.clear()`。staleTime 15s 不是所有业务统一适用的刷新合同。 |
| D8-7 | 保留条件性缺口 | `action-request.ts:126` manual redirect 与 `action-response.ts:22-34` 试读 Location 不匹配浏览器 opaqueredirect。当前生产无此 Action，不属于现网业务故障。可延后，或将 JSON target 作为显式后端契约变更，重新生成 OpenAPI/type 与 fixture，并验证目标同源/允许集合，不能把未受信字符串直接 navigate。 |
| D8-8 | 保留 | multipart 文件+边界大于文件本身，四层上限须一致。16→17 MiB/20 MiB 是设计选项，不能在 S10/S11/S22 中混用成三个验收标准。更强资源缺口见 Feishu 附录：ZIP 体积并不约束解析展开内存，增大 body 不能代替解析配额。 |

## 3. 新补充发现与证明

### R-01：取消 shutdown 后仍可继续运行的任务，已有 Drop 实现不能直接照抄（P2，源码 + 本轮边缘附录本地复现）

- **触发链**：`bootstrap.rs:406-450` 用同一总截止逐个 await shutdown → `authorization/worker.rs:172-179` / `session_cleanup.rs:46-52` / `grant_expiry_worker.rs:47-53` / 4 个 Feishu worker 都先 take JoinHandle → 总预算超时 → 局部句柄被 drop（detach）→ owner 的 Drop 已无 handle 可 abort → task 在 Tools 关闭后仍可能继续工作，直到 runtime 退出。
- **反证**：直接 drop 一个尚未 shutdown 的 authorization worker 确实会 abort，已有测试覆盖的是这一种；它不能证明“shutdown future 被取消”。watch 只在轮次边界检查，不能中止 in-flight I/O。
- **最小完整修复**：让 abort 所有权覆盖整个等待期，或在 timeout 分支明确 abort 并等待取消完成；先同时发退出信号，再在总预算内分别 drain，给资源关闭保留预算。不要用每阶段重新给 30s 使总时长无界。阻塞任务必须另有配额/协作取消边界。
- **验收**：人为挂起一轮，实际进入 shutdown 后取消外层 future；等待后确认任务标记不再变化、资源不再调用。正常关闭不应误报 worker 失败。

### R-02：迟到 restore/refresh 可覆盖退出或新会话（P2，已本地复现）

- **触发链**：发起 restore/refresh → `clearSession` 或新 `beginSession` → 旧 Promise 完成 → `session-controller.ts:95` 无版本判断覆盖 token；`auth-session.ts:96-99` 同样无条件 persist 并广播；`shell/session-bridge.tsx:27-30` 接受该广播。
- **证据**：`node docs/assessments/evidence/2026-10-25-session-race-check.mjs` 执行当前 TypeScript 实现，三例均复现（清会话后复活本地态、新登录被旧响应覆盖、auth 模块清空后仍广播 refreshed）。只替换网络/浏览器边界，未复制被测逻辑。
- **反证与边界**：服务器仍检查 JWT 撤销/授权版本；本复现不证明可绕过后端权限。跨标签退出、断网与响应乱序使本地状态发生矛盾；若旧身份 token 仍有效，后续请求可能使用过期的身份上下文。测试使用虚构 token，无真实凭据。
- **最小完整修复**：会话代次在开始/清除时递增；capture 代次的 restore/refresh 仅可向当前代次提交；同样保护事件广播与失败清会话。取消请求只是优化，代次检查才防迟到完成。
- **验收**：覆盖退出、新登录、凭据变更、跨标签结束四种边界及旧成功/旧失败；相同会话正常 refresh 仍保留 UI 上下文。

### R-03：refresh 的 10 秒预算不包含等待跨标签锁（P2，源码）

- `auth-session.ts:88-95` 的 `AbortSignal.timeout` 在锁回调内创建；`refresh-lock.ts:207-211` 的 Web Locks request 没有 signal/等待上限。因此 HTTPS 后走 Web Locks 分支时，排在其他持有者后面的请求不受 10 秒预算约束。
- storage fallback 有 15 秒等待上限（`:178-184`），但超时直接无锁执行，且心跳写租约不检查所有者（`:160-162`）；它是尽力互斥而非跨标签原子锁。不能用该 fallback 证明刷新永不竞争。
- **反证**：正常前台标签完成 fetch 后会释放锁，不能将普通多标签直接称为死锁。冻结/长时间调度暂停/持锁请求未能完成是触发条件；未做真实浏览器冻结实验。
- **修复/验收**：端到端 deadline 在排锁前开始，锁等待可取消；后端保留独立重放防御，客户端互斥只优化竞争。持锁 stall、等待方取消、恢复后旧响应、无 Web Locks 四类测试；不要仅用 never-resolving fetch 的 mock 验证 AbortSignal，因为 mock 可能根本不监听 signal。

### R-04：readiness 指标可保留陈旧成功，关闭 metrics 也会关闭管理探针（P2 / 配置条件）

- `telemetry.rs:220-229` 的 `/metrics` 只更新 pool metrics；`:232-251` 只有调用 `/health/ready` 才刷新 ready gauge。只 scrape metrics 而不周期探测 readiness，历史 1 可一直导出；`max_over_time(...[2m])==0 or absent(...)` 无法发现持续存在的陈旧 1。
- `start_management_server:142-144` 在 `metrics_handle=None` 时直接 return，关闭 metrics 同时关闭部署所依赖的 9090 readiness。健康门禁与观测开关耦合应显式记录/校验；不是默认配置事故。
- **最小修复**：确定一条实际运行的探针链，监测它自己的 up/采样新鲜度和 worker progress；管理面健康服务是否独立于 metrics 应明确合同。指标不应把“上次 ready”宣传为“现在 ready”。
- 多目标时无 label 的 `absent(yang_system_readiness_ready)` 也不能发现只缺一个实例；需将目标清单、`up` 与每目标新鲜度关联，不能仅加全局 absent。
- **验收**：停止探针但保持 metrics 服务，告警必须触发；停 scraper/目标也触发；关闭 metrics 的配置有预期行为且部署预检一致。未连接任何生产采集端。

### R-05：生命周期单实例注释漂移与清理容量边界（已有控制 + 条件性待测项）

- `grant_expiry_worker.rs:8-9` 假定蓝绿不重叠，实际发布 smoke 会重叠。Feishu 专属 `workers_enabled` 不能自动约束 `bootstrap.rs` 启动的 outbox/grant expiry/session cleanup。
- outbox 已有数据库 claim 机制，不能把所有 worker 一概称为无租约。交叉反驳确认：`access/domain/grant_expiry.rs:73-115` 已锁用户授权行，再按观测到的 expires_at、`converged_at IS NULL` 做 CAS（`access/domain/repository.rs:225-243`），仅 affected=1 才递增版本；因此不能报告双实例必然重复 bump，顶部单实例注释过时。会话清理 SQL `session_cleanup.rs:95-103` 一次删除全部过期行，无 batch，对首次大积压可能持长事务；后者未做容量实测。
- **修复/验收**：建立逐 worker 的所有者与幂等表，smoke 使用覆盖所有副作用任务的明确运行模式；在隔离库演练双实例与大积压。不要为每个 worker 无差别引入分布式锁。

## 4. 相关执行步骤逐条裁决

| ID | 裁决及必须改写的验收 |
|---|---|
| S11 | 拆分交付，按消费者落配置；只加未生效开关不能宣称修复。配置/环境变量/secret/模板/契约必须同批；新字段会让旧版本 `deny_unknown_fields` 失败，revert 代码并不保证新配置可回滚。 |
| S12 | 保留目标，删除硬编码超时阶梯与全局空2xx成功；加入 R-02/R-03。redirect 是前后端合同变更，不能在并行表称“纯前端”。瞬态恢复 UI 需要完整状态转移与旧请求隔离。 |
| S15 | 范围覆盖 7 worker + 管理任务；Drop abort 要覆盖 take 后等待期。用总预算+阶段上限/预留，不能用独立阶段无限续期。panic 与 hang 分开验收，panic failpoint 绿不证明 stall 可发现。 |
| S50 | 保留，必须有外部消费者、up/absent/freshness 与端到端报警演练；源码 promtool 绿不等于通知成功。counter 必须明确零初始化或用缺席规则，不能要求未触发过的惰性 counter 一定已在 `/metrics`。 |
| S60 | 重写矛盾验收：同一探针允许1.5s完成，则不能又以500ms统一超时。设总预算与各探针预算明确值，再验证并行max与超时。保留业务池饱和对readiness的真实反映。 |
| S74 | 把 Redis authz 缓存失败回源 MySQL 称为“有权威验证的降级”，不是授权 fail-open。整个认证链还有 Redis JTI/subject 撤销检查（见数据附录），受保护API不能承诺Redis故障时继续工作。 |

## 5. 未编号结论、正向控制与范围边界

- D7 遗漏五项：共享截止时间、无仓内采集器、ready-before-bind、pretty日志、两套Action指标均复核；前两项按 R-01/R-04 修订，bind窗口不可凭静态代码估为亚毫秒。pretty 为明确实现，日志采集文档已有前置说明；两套指标应定一份SLO分母，不要求强行相等。
- D8 遗漏四项：登录超时、AbortSignal功能检测差异、裸401没有中央过期、portal取消均复核。**不是任意业务401都该登出**：`auth-session.ts:155-157` 特意只在refresh终态失败时过期，统一漏斗不能破坏这个设计。
- SMTP：`email_delivery.rs:169-172` 使用强制 STARTTLS 与配置超时，错误映射不输出收件人/内容；不要新增“SMTP默认明文”误报。未验证线上服务商、SPF/DKIM/DMARC、送达与模板运营效果；“send完成”仅代表传输侧结果，非收件箱已投递。
- 浏览器：只内存存 access token、清遗留 storage、Refresh Cookie、跨标签结束广播都是已有控制，不能当待开发。没有实浏览器证据证明所有支持版本的 AbortSignal/Web Locks 能力；feature detect与真实浏览器回归仍需在修复时做。
- 真实浏览器 lazy chunk 恢复、线上gzip、RUM/弱网、锁冻结、SMTP与OTLP链路未执行。没有修改生产源码、配置、数据库，也没有运行部署或发布。

## 6. 本轮实际验证

| 验证 | 结果与边界 |
|---|---|
| `node docs/assessments/evidence/2026-10-25-session-race-check.mjs` | exit 0；3个确定性反例重现当前会话竞态。脚本断言的是现存缺陷，修复后应改成正确性回归测试。 |
| 对默认filter、13条alert、shutdown所有权、指标采样入口的源码追踪 | 代码证据；未用它代替生产可用性/告警送达证明。 |
| 精确Router层序与take-then-await取消 | 见边缘附录的独立可重跑最小复现及其限制。 |

本次为审计，未改行为；未跑全量quick/full/integration，不把定向自检写成整个项目通过。

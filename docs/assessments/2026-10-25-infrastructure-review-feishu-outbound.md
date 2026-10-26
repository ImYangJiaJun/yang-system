# 出站 HTTP 与飞书基础设施对抗复核

日期：2026-10-25。范围：D3-1～D3-7、D4-1～D4-11、D6 的飞书相关项及相应路线图；补查 OAuth、并发、事件、通知、限流、附件和 XLSX。审阅源码、检查公式模型并引用协调代理的隔离 Redis 复现，未修改业务代码、提交、SSH、读取凭据文件或触达生产。

## 1. 证据基线与结论

输入为 `2026-10-24-network-infrastructure-first-principles-audit.md`、`2026-10-25-network-infrastructure-audit-adversarial-review-and-roadmap.md` 和 `docs/operations/2026-10-25-s01-production-facts.md`。应用审核期间 HEAD 从 `0adc5ab1cbd0729dfdf264fe617ae07a029ee585` 前进至 `5c0ea9cf8c1afe09f8a2cebe1354456c9c703bea`；由并行会话提交原有飞书改动，并非本审核的变更。最终判断按当前文件。框架基线为 `e27a2d3c127e0bea6c40bc5fa584b2fdb2162705`。原报告“当前未提交飞书改动”的描述已经过时，不能继续作为最终基线描述。

代码先用 CodeGraph 定位，再读它未返回的具体范围；框架依赖使用 Cargo.lock 锁定的 reqwest 0.12.16 源码。下文应用路径相对本仓，`crates/` 相对 `D:/code/lib_yang`。S01 是同日历史采集资料，**不是本轮在线验证**：其中测试机 `log_inbound_requests=false`，财务机此值未知；不能继续把“两台均 true”写成事实。

主要结论：原报告抓住了出站错误分类、事件处理确认和同步派发取消三个真实缺口，但“全部事实成立”的概括过强。需要纠正 connect timeout 的 DNS 边界、“创建后只有 1–2 秒窗口”、重复通知只需极短崩溃窗口等结论。更关键的漏项是：刷新覆盖重新授权、事件终态短路吞掉后续撤销、无 fencing 的并发回写、正常成功通知失败退出自动重试且历史通知压制新结果、创建限流预订键提前过期、门户计数窗口尾段意外续期，以及 XLSX 共享字符串展开后的内存放大。

本文的 P2 表示可达但依赖故障、并发或有权限输入的可靠性/恢复缺口；不表示低成本或可以长期搁置。未掌握业务依赖这些投影的强度，未把所有错误审批状态直接升为财务损失 P1。也没有把源码反例写成线上已发生事故。

## 2. 逐条裁决

### 2.1 D3 出站 HTTP

| ID | 本轮裁决/级别 | 事实、因果与反例 | 修法可行性与验证要求 |
|---|---|---|---|
| D3-1 | **部分成立 / P2** | `crates/yang-base/src/http/client.rs:35,264` 只有总超时，没有独立连接/读预算。但“所有飞书调用均 30s”错误：`outbound.rs:212` 覆盖请求预算，附件 `approval_attachment.rs:188,209` 为 120s，通知 `approval_notification.rs:304` 为 2s。复核稿“connect_timeout 不覆盖 DNS”也错误，见下方依赖证据。总超时已使慢滴流有界，不是无限等待。 | client 级连接/读超时可行；仍保留每请求总预算。仅 `is_timeout/is_connect` 不能准确区分 DNS、TCP、TLS、读阶段；不要承诺不存在的阶段指标。用可控 resolver、接受连接但不响应、半响应体本地服务验证，不依赖 `10.255.255.1` 恰好黑洞。 |
| D3-2 | **成立 / P3** | `request.rs:774,786,841` 确有可选重试/熔断；飞书 `outbound.rs:216` 不配置框架重试。未使用路径和“尚未实现”注释漂移成立；没有证据支持现网缺少一层重试即有故障。 | 改文档、标实验性即可；删除公共 API 需清点外部框架使用方，不能按本应用零调用推出整个框架可删。 |
| D3-3 | **成立 / P2，优先修** | `response.rs:192-196` 把 chunk 错误变成 Client 类 parse failure；`outbound.rs:621-626` 折为 Fatal，创建响应路径可进入 `approval_dispatch.rs:1161` 不可逆回填。`request.rs:924` 也绕过 HttpTimeout 分类。不过当前新建实例后的详情失败已在 `approval_dispatch.rs:721-725` 保留 CreatedPendingBackfill，不能概括为所有 get 失败都终态。 | 修传输错误分类并联动框架可重试判断；解析坏 JSON 必须与网络读取失败区分。飞书业务处置还必须保守处理未知结果，不能仅靠框架 error category 决定是否写业务失败。测试分别覆盖建单响应中断、建单确认后详情中断、超限、非法 JSON。 |
| D3-4 | **成立，精确峰值不可核 / P2** | `approval_attachment.rs:193,207` 保留下载字节并构造 multipart；`outbound.rs:604` 深拷贝请求；`request.rs:889-890` 再复制 body。多份 50MiB buffer 的模型成立；200MiB、250–270MiB 是容量估算，不是本轮实测峰值，分配容量/时点会改变数值。 | `Bytes` 可消除后两次深拷贝，multipart 仍需一份。验指针共享及本地 RSS；不要由一次附件测试推导任意并发安全。完整流式不是本轮必要前置。 |
| D3-5 | **成立 / P3** | `bootstrap.rs:140` 使用无 breaker 的 client；`request.rs:852-856` 仅按 HTTP 记账、按 host 分组，200+非零业务码被视成功；body 读取失败发生在其记账之后。启用现有 breaker 不能修复飞书主要错误形态。 | 不直接启用。保留/说明当前无熔断即可；若未来启用，应按 endpoint 的真实失败语义设计并测试 half-open 并发，不把“有 breaker”当可靠性证明。 |
| D3-6 | **部分成立 / P3（采集闭环另计）** | `outbound.rs:634-655` 已有应用层重试/最终失败 WARN，不能叫日志完全断链。缺成功分母、尝试次数、完整 body 延迟指标成立；框架日志只量到 headers，不能充当下载完成耗时。 | 在统一出站入口区分 attempt 与 logical call 两组语义；否则 ok/retry/failed 混在一个计数器会使失败率失真。endpoint 固定枚举，禁原始 URL 标签。 |
| D3-7 | **成立但影响有条件 / P3** | `client.rs:275-280` 未设 no_proxy，reqwest 默认系统代理和 redirect 生效；scheme 接受 http。当前附件请求目标由固定飞书 host + 经验证的 file_token 构造，`approval_attachment.rs:1,179` 并不直接请求用户附件 URL，不能升级成已证 SSRF。生产代理环境本轮未核。 | 固定代理及重定向策略可行；先验证媒体 CDN 重定向。禁止无条件全框架 https_only 破坏受控本地测试/其他合法 HTTP 调用；约束飞书客户端即可。重定向 HTTPS→HTTP 也须拒绝。 |

**D3-1 的上游反证**：本机锁定源码 `reqwest-0.12.16/src/async_impl/client.rs:886` 把配置的 connect timeout 交给 connector；`src/connect.rs:728-767` 用 `tokio::time::timeout` 包住整个 `connect_with_maybe_proxy`/代理连接 future，`:555,593` 的 `http.call(dst)` 位于其中。因此连接 future 中的 DNS 等待受此外层预算约束。准确边界是“超时终止等待，未必能杀死底层已启动的阻塞 DNS 工作”，不是“DNS 不受 connect timeout 约束”。

### 2.2 D4 飞书异步/恢复

| ID | 本轮裁决/级别 | 事实、因果与反例 | 修法可行性与验证要求 |
|---|---|---|---|
| D4-1 | **部分成立 / P2，优先修** | 无业务码的 502/503/504 在 `outbound.rs:352-357` 为 Retry，原审计例子错。未知非零业务码在 `:344-347` 为 Fatal；200 非 JSON 在 `approval.rs:160-163` 为 Fatal；可写入错误回填并退出扫描。不过业务码优先，5xx+未知非零业务码仍可 Fatal，不能把复核稿“504 根本不会 Fatal”说成无条件结论。 | 引入未知/待核实结果，不写业务失败。已确认创建与尚未确认创建分开保存；限次后进入人工核查/暂停，不以限次把未知变成已失败。见 S40。 |
| D4-2 | **成立 / P2，优先修** | `approval_dispatch.rs:1032` 吞扩展回写错误；`feishu_event_worker.rs:631,638-666` 仍可能更新终态并返回 Processed；`:251` 标处理完成。任务状态写入自身失败也仅 warn。这是持久化确认超前于必要副作用。入站已经提交 event，修改 worker 不改变飞书的 3s 接收 ACK。 | `write_extended` 返回 Result；必要回填/任务状态成功后才标 processed，并要求 affected rows 正确。注意编号回填与扩展字段契约不同，`finish_backfill:918-925` 的主产物成功不可被抹成创建失败。还必须修 N2/N3，单改 Result 不足。 |
| D4-3 | **成立 / P2；原降级理由撤回** | handler 内 await 整条链，task 在 `dispatch.rs:540` 才落库。创建后仍执行 `approval_dispatch.rs:709` 详情、`:733` 部门、`:915` 编号、`:922` 扩展回写、`dispatch.rs:519` 通知；没有 1–2s 上界，多个出站调用可消耗完整预算。首次请求若无 task，轮询 `feishu_approval_worker.rs:689-691` 不为新记录建行，超时孤儿不会自动接手。反例：已有 pending task 或人工重发可凭 UUID 恢复，不是物理不可恢复。 | 先持久受理事实再执行，显式 202 契约、稳定 UUID、申请人快照；接收确认与业务完成分开。慢详情/慢回写也需覆盖，不只测附件。 |
| D4-4 | **成立 / P2** | `user_token.rs:721-727` 先轮换单次 refresh token 再写 DB，写失败/取消会丢局部 bundle。不能根据代码推算“100ms窗口”概率；数据库 stall 可把窗口拉长。还有 N1 的并发覆盖。 | 刷新后短重试、在下一次 await 前保留 pending bundle 可降低风险；必须带授权代次与 CAS，不能覆盖后到的新授权。进程崩溃/响应丢失仍可能要求重新授权，不能承诺完全消除。 |
| D4-5 | **成立 / P2** | claim/outcome 不使用 attempts/available_at，读取表结构/记录错误均 Retryable；每轮可重复烧调用。已有 pending 告警是反证，但本轮不能证明线上采集会响；也不是每条一定每 30s，单轮耗时和游标会拉长间隔。 | 有界退避、可恢复暂停和独立人工重试。Waiting（用户待补字段）与传输失败不可共用简单 10 次失败计数；确认已创建的任务不能因回填重试耗尽丢失实例坐标。seed 空格语义必须同时改。 |
| D4-6 | **成立 / P2** | challenge handler `:89,100-107,146` 错误体仍 200，事件共用 URL 时可在转交前被吞。正常 event_callback 委托成功也必须 200，所以“只有 challenge 才 200”字面要求错误。 | 按认证/格式错误、服务配置/暂态故障区分 4xx/5xx；保留成功事件 ACK。外部重投策略和坏密钥恢复窗口未 live 核；不能承诺任意非 2xx 永久自动恢复。 |
| D4-7 | **成立 / P2，修复不足** | `bootstrap.rs:292-363` 四 worker 无专门关闭开关；事件 claim `feishu_event_worker.rs:295` 和审批 claim `feishu_approval_worker.rs:783` 都是 SELECT，没有租约。smoke 双进程构成可达并发。UUID 保护创建唯一性，不保护通知、回填、补集停用、用户凭据刷新。 | workers-off 能消除 smoke 的主要并发来源，仍需覆盖旧实例排空、手动拉取、HTTP 同步派发与新 worker 并发。不能等同全系统 fencing。schema 门禁另做；关 worker 不关 DDL。 |
| D4-8 | **成立，影响有条件 / P2** | `receive_approval_event.rs:175` 缺 UUID 落空串；唯一索引把后续当重复成功。正常 v1 带 UUID 是反例；不能称正常报文必丢。 | 最小是验证必需事件字段并明确拒绝不支持形态；若兼容派生 id，不能只 hash event_type/instance/status/time，approval_task 还有不同 task/节点。先定义规范化、版本、缺字段策略并覆盖同一毫秒不同任务。 |
| D4-9 | **成立但低估影响 / P2** | 同轮 UUID 幂等存在，但 `approval_notification.rs:153` 每次 send_feedback 生成新 UUID，`:82` 仅保证一小时。发送成功后 DB 更新失败、两个实例并发都可重发，无需崩溃；还有 N4 成功通知直接丢失。 | 持久意图、稳定通知键和上游 UUID，明确一小时窗口；发送前标 sent 会改成丢消息，notified_at 单列不能实现严格一次。S41 的绝对验收需改。 |
| D4-10 | **部分成立 / P2** | 领域函数共享、worker 有 state=pending CAS 是真实保护；但不能推出两路径完全等价。HTTP `record_dispatch_task:808-821` 在执行完成后读取当前 state 再 CAS，该 state 可已是 backfilled；它不是执行开始代次 fencing。较晚的旧失败仍可能覆盖较新的成功状态。外部副作用发生在 CAS 之前，双通知/旧回填也不会被 CAS 阻挡。 | 入队化收敛调用方；过渡期两条入口用同一个 durable claim/代次，终态不能按“最后读取的当前状态”无条件降级。验证旧请求晚到、两个 worker、HTTP+worker 三种交错。 |
| D4-11 | **成立 / P3，采集闭环按 P2** | `outbound.rs:655` 仅最终 failed；event worker 缺队列年龄和 pending 指标。与 D3-6 是同一改动，不应重复计算收益/工期。源码有终态 counter 不等于生产报警。 | success/attempt/retry/429、完整响应耗时、队列最老年龄、真实总量；不要用 batch size 冒充 backlog。故障注入后验证指标及告警消费者，两者分开报告。 |

原审计 D4“现状与优点”里的“创建前落 state=creating+uuid、通知两阶段”也不适用于当前完整路径：`dispatch_one` 文件头仍有这个历史描述，但实际无创建前持久化；claim 注释明确已放弃 lease。不能把注释当已实施事务保证。

### 2.3 D6 飞书信任边界

| ID | 裁决/级别 | 事实、反例及可行修法 |
|---|---|---|
| D6-1 | **条件成立 / 开启且接收凭据时 P1；默认关闭 P3** | `request_log.rs:94-115` 记录 headers/body，`option/actions/mod.rs:33-37,72` 由开关控制。历史 S01 测试机为 false，财务机未知；不要再说已确认两台正在泄漏。该开关只覆盖其实际注册的机器入口，不等于所有 dispatch/event 请求。当前 dispatch 入库前 `dispatch.rs:966-970` 已脱敏 token；也不能说其新请求日志全为明文。部署闸门可行，但“不脱敏是唯一调试用途”论据不足：真实报文形状仍可保留而去掉凭据。另有 N7 的事件明文落库/日志旁路，关此开关不足以封闭全部秘密通路。 |
| D6-2 | **部分成立 / P2** | 匿名 options/dispatch 可以先打 DB，再做 token 校验；无边缘限速。32字节随机 token+摘要+常数时间比较不支持实际爆破叙事；真实风险是连接、查询及派发失败日志写放大。创建桶在鉴权/读库之后，不能保护匿名入口。边缘全局预算+真实 IP 桶可行，单 config_key 桶可被随机 key 绕过，且可能让攻击者耗尽某合法配置的额度。 |
| D6-3 | **交叉项，交由账户/边缘审核** | 密码重置 URL 非本分工；本报告没有将其算作已独立核实。OAuth code/state 查询串也应纳入边缘日志脱敏，但 code/state 的一次性/TTL 不等同永久 refresh token。 |
| D6-4 | **成立 / P3** | `user_token.rs:176-208` 为进程内 TTL+remove，一次性 fail-closed。现有蓝绿流量不是任意均衡到 smoke，正常单实例可用；重启/cutover 期间失败真实但可重新发起。Redis GETDEL 可跨实例；不可用时不能悄悄退回各自内存。state 只解决 CSRF，不能解决 N1 刷新与授权覆盖。 |
| D6-5 | **条件成立 / P2，边缘审核交叉项** | 飞书/门户真实 IP 额度依赖实际受信跳链；模板 CIDR 不能证明部署 peer。按 S01 历史测试机信 loopback；最终应按真实网关链配最小可信地址并清洗 Forwarded，不能机械扩大整个网桥段作为通用修复。 |

## 3. 原清单漏项或遗漏的关键反例

### N1：刷新覆盖后到的重新授权，且跨实例锁不存在（P2）

- **可达链**：A 在 `user_token.rs:713` 读旧授权 → `:721` 向飞书刷新并等待 → OAuth 回调 B 在 `oauth_callback.rs:152` 调 `store_authorization`，`:566-587` 按同一 slot/id 写入新用户和新凭据 → A 在 `update_tokens:622-626` 只按 id 覆盖 token/scope。最终行保留 B 的 `open_id/authorized_at`，凭据却属于旧授权 A；新授权被覆盖、365天时钟与凭据来源也失配。
- **反证/边界**：只有一个刷新请求且没有并发重新授权时，进程内 `REFRESH_LOCK:689,712` 能避免同进程重复刷新。两实例同时刷新同一单次 token 时，失败方下一次可能读到成功方新行，因此不能一概断言必然永久断链；已证更强缺陷是旧刷新覆盖新授权。
- **最小修复**：刷新和重新授权共享授权代次；刷新更新按已读代次/旧 refresh 摘要 CAS，CAS 失败加载新授权，丢弃旧代次的 pending bundle。跨进程串行化刷新，锁需有明确超时/所有权；单纯延长进程锁无效。S44 的内存 pending 同样必须携带代次。
- **验证**：两个 provider barrier 控制返回次序；刷新中重新授权另一个用户，断言身份、凭据、authorized_at 始终来自同一代次。另测失败落库、取消、跨实例竞争；不用真实凭据。

### N2：已终态任务把后续撤销/删除事件直接吞掉（P2）

- **可达链**：某实例已同步为 APPROVED → 后续 REVERTED/DELETED 事件入库 → `feishu_event_worker.rs:419-427` 仅看本地终态返回 Skipped → `:276-278` 标 processed，未调用 get_instance。`receive_approval_event.rs:185-186` 明确兼容 REVERTED，`approval_dispatch.rs:1066-1068` 也设计了“已撤销”后缀，但此路径到不了。
- **反证/边界**：不发生终态后变化时跳过有节省调用价值；本轮未做飞书真机撤销，合法后续事件形态来自仓库契约和处理代码。重复旧事件本可去重，但这不能证明所有后续事件都是重复。
- **最小修复**：以事件身份/版本去重，不以本地终态拒绝所有新事件；新事件触发受控的最新详情同步，保留同步版本/时间。不能仅删除 guard 后就认为多实例顺序已解决。
- **验证**：APPROVED 落库后送 REVERTED，远端返回 reverted=true，断言回填发生；重复相同事件不重写。结合 N3 覆盖乱序、终态后的删除。

### N3：SELECT“认领”没有互斥，DB 的末尾 CAS 不约束外部回写（P2）

- **证据/触发**：审批 `claim:783-801`、事件 `claim_events:295-305` 均不修改认领状态。A 读到 PENDING 详情后阻塞；B 读到 APPROVED 并先回写；A 恢复后在 `feishu_event_worker.rs:631` 写旧投影、`:649` 覆盖状态；两事件都可 processed，之后没有新事件纠正。即使加 DB CAS，旧 Bitable 写已发生。HTTP 状态 CAS 又是在执行结束后才读期望值（D4-10）。
- **反证/边界**：单 worker 串行时不存在上述双 writer 交错，创建 UUID 也确实阻止同 UUID 建第二个审批。该保证不延伸到消息、字段更新、表级全量补集停用；smoke 共用库会破坏单实例前提。
- **最小修复**：第一阶段关闭 smoke worker，收敛同步派发；随后按实例/数据源串行写者，任务 claim 带代次/owner/租约，旧 owner 不得继续落结果。外部 API 不支持 fencing 时，租约过期不能自动假定旧网络请求已取消；必须有最新状态重读/对账最终收敛。
- **验证**：两 worker 可控 transport，故意让旧详情最后完成；不得最终回到旧状态。验证租约到期后旧 worker 恢复、旧 HTTP 完成、pull 与新快照交错。

### N4：成功通知失败退出自动重试，历史通知又能压制新结果（P2）

- **可达链一**：编号回填成功 → 通知返回 Failed/无接收人 → `feishu_approval_worker.rs:1185` 仍写 backfilled，且 last_error=NULL，完全忽略 notified=false；HTTP `dispatch.rs:1622` 同样。claim 只看 pending，seed 只扫回填格为空，因此正常调度不会自动重试这条通知。无需崩溃，也无需同时两个实例。
- **可达链二**：`send_worker_notification:1460-1478` 按 record_id/config_id 查询全部 succeeded/failed 历史日志；任意行被 `http_path_notified:1420-1432` 判定 `notification.status=sent`，就直接返回本轮已送达。查询没有执行代次、当前结论或接收人条件。因此同记录曾发出的失败通知，可以压制修正资料后重试成功的通知；旧接收人的通知也不能证明当前申请人已收到本轮结果。
- **反证/边界**：编号确实成功，不能把审批结果改为失败；人工重新 dispatch/清回填后重新触发仍可能再发，故不是物理永久不可恢复。HTTP 调用者可能已收到响应，但 worker 注释 `:1268` 明确承诺每个结果反馈，且异步调用者没有该同步响应。失败通知路径已有 unnotified，不覆盖成功路径；历史通知本来用于跨 HTTP/worker 去重，问题在于去重范围过宽。
- **最小修复**：通知结果与审批结果拆成独立持久状态/发送意图；backfilled 仍保持，未确认通知可查询和重试，稳定 UUID 跨轮复用。意图身份包含执行代次、结论和预期接收人，不能用“这条记录曾有任意通知”替代。记录 sent 前写成功的办法会改成静默丢消息，不可采用。
- **验证**：回填成功、IM 暂态失败后恢复，任务保持 backfilled，通知队列仍可重试；历史失败通知后修正并成功、不同代次/接收人均应发送新结果，同代次同结果才去重。发送成功但确认落库失败时，在平台去重窗口内复用 UUID。超窗口交给可见的未知送达处置。

### N5：创建限流不是实际出站配额上界（P2）

- **源码反例一**：`approval_rate_limit.rs:80-85` 预订任意远的 target，却只设置 `PEXPIRE interval*2`。100 个同时调用者预订到未来 65.934s；1.332s 后键过期，2s 后第二批即可在第一批尚未消费的时间轴重新预订。匿名无凭据不能到这里，但有效工作流并发可到达。
- **源码反例二**：`:103-106` 用本机 now 减 Redis 绝对时间，快时钟会少等；注释“Redis 时间基准消除时钟偏差”过强。worker `:447` 在读表/附件前占槽；HTTP `dispatch.rs:1396` 在 `dispatch_one` 前占槽；`approval.rs:158` 内层重试不会再取槽，多个准备过程完成后仍会聚集创建请求。
- **范围反例**：键只按 deployment（`:34-35`），两个 deployment/两台 Redis 若用同 app 并不共享上游配额；该同 app 条件本轮未证。附件 `approval_attachment.rs:170-171` 只在单条内 sleep 200ms，跨请求没有 5QPS 总闸门。429 重试是退避机制，不是总配额守恒。
- **最小修复**：门控每一次实际 create 尝试；TTL 至少覆盖全部已预订区间并返回相对 wait，或不提前预订长队列而等待后原子重试取槽；保留有界等待/取消。按真正上游限额主体设计 key，下载额度独立；避免重复给同逻辑请求和每尝试各扣一套。
- **本轮检查**：Python 等价时间模型（不连接 Redis）复现 `interval=666ms`、第一批最后 slot=65934ms、第二批首 slot=2000ms、任意 60000ms 窗口最多 **182 个预订**；断言通过，exit 0。它证明公式反例，不代表生产实测速率。
- **实施验证**：真实隔离 Redis、多调用者、超过 TTL 的积压、取消、时钟偏差、内部 retry；用 transport 实际发送时刻计算滑动窗口，不能只断言 `60000/interval<=100`。

### N6：XLSX 解压上限不约束内存展开，probe 同步占线程（P2）

- **可达链**：有 datasource.write 的用户可上传合法 XLSX；`xlsx.rs:35,52` 只限制每文件 ZIP 目录声明的总解压字节。`cell_text:601-602` 将 shared string 每个引用复制，`:532,537` 再保存各行独立 String，`:507,543` 积累整个 snapshot。一个 1MiB 共享字符串被一万行引用，理论上可产生约 10GiB 字符串副本，而 ZIP/XML 声明远低于 256MiB；label 在 import_binding 后期才截短，挡不住前面的分配。
- **第二条链**：`probe_xlsx_headers.rs:153` 在 async handler 直接同步 probe；`read_header:253` 急切读 sharedStrings，能阻塞 Tokio worker。import 虽用 `spawn_blocking`（`:267,286`），已开始的阻塞任务不因 handler timeout/drop 被杀；`ImportGuard` 为进程内表（`import_progress.rs:95-145`），handler 取消释放占用后新解析又可开始，缺全局并发/内存预算。
- **反证/边界**：已有压缩包总量、每文件解压上限、panic 捕获、选中列过滤；所以不能报告“完全无资源保护”。本轮未构造 10GiB 文件或做 OOM 压测；这是源代码分配模型。ZIP 目录伪造能否绕过 calamine/zip 的真实解压校验未验证，不作为已确认缺陷。
- **最小修复**：共享的解析并发预算，probe 放 blocking pool；解析途中累计展开字符/行/单格上限，读取前拒绝超出本业务字段边界的数据；持有 permit 到 blocking 任务真正结束，并提供协作取消。不必先写流式导入框架。
- **验证**：小规模重复 sharedStrings fixture 放大比测试、低预算提前拒绝、两个取消请求后的并发上限、异步心跳延迟。多实例同 datasource 导入还需 N3 的跨实例写者约束。

### N7：关闭原始请求日志仍有事件秘密持久化/日志旁路（P2，权限条件明确）

- **证据/后果**：`receive_approval_event.rs:222` 保存完整已解密 event_body，其中包含刚在 `:156` 校验的 verification_token；`event_table.rs:71` 是普通 raw_payload Text。读取数据库/备份者得到比任务投影更多的活认证材料。`event_challenge.rs:145,151` 的 WARN 直接打印 body；`receive_approval_event.rs:121` 的 ERROR 打印解密前200字节，均不受 `log_inbound_requests` 开关控制。开启 DEBUG 另有完整解密报文。
- **反证/边界**：`list_events.rs:76-95` 不回显 raw_payload，本轮没有发现只读控制台通过该 Action 直接取到 token；配置含 encrypt key 时单拿 verification token 也不能绕过明文拒绝。不把此问题夸成匿名直接获取秘密。
- **最小修复**：认证后移除 token 再存诊断报文；日志只留事件 ID、结构和安全错误类别；历史行/备份需单独盘点轮换及保留策略。不能仅靠 S71 加密用户 token 表宣称全部秘密已离开明文持久层。
- **验证**：带测试 token 的正确/缺字段/非法 JSON 事件，日志捕获与落库快照均不得包含原值；控制台投影保持不变。生产历史清洗是另行授权的写操作。

### N8：门户限流把正常 TTL 尾段误当无过期，超限窗口可反复延长（P2）

- **证据/触发**：`src/addon/feishu/domain/rate_limit.rs:27-37` 在 INCR 后读取秒级 TTL，`:33` 使用 `ttl < 1` 补设 60s。Redis `TTL=0` 可以表示正常的亚秒过期尾段，并非无过期；此时任意请求都会保留累积计数并把窗口延长 60s。超限请求同样先 INCR，持续请求若每轮命中尾段可反复延长拒绝期。
- **范围/反证**：key 为 `portal:query:{portal_id}:{client_ip}`（`:45-46`），影响相同门户、相同识别 IP 的桶，不是所有门户或所有来源。共享 NAT 下同桶正常用户可能持续被拒；若代理链把多人识别为同一 IP，影响随之扩大，但本轮未证生产存在该拓扑。请求未命中尾段时键仍会正常到期，不能称任何超限都永久封禁。
- **最小修复**：补 TTL 的判断收窄为 `ttl == -1`（存在但未设过期）；正常 `ttl == 0` 不续期，返回值由既有 verdict 至少给 1s。保留门户 ID/IP 隔离与 inclusive 阈值，不需重写限流器。
- **动态证据**：协调代理执行 [隔离 Redis 脚本](evidence/2026-10-25-redis-version-replay-check.py)，从当前源码提取原 Lua，`SET count 31 PX 300` 后观测 `TTL=0`；执行脚本得到 `count=32, TTL=60`，断言 PTTL>59000ms，exit 0。使用独立、无网络、无宿主端口的临时 Redis 容器，未触达既有 Redis/HTTP/MySQL。本分工阅读脚本并复核源码，未重复运行。
- **修复验收**：亚秒尾段请求不能延长 PTTL，旧窗口到期后计数恢复；无 TTL 异常键仍被补 TTL；同门户共享 IP 的持续超限请求不能把其他同桶用户的恢复期再延长。至少对 `ttl == -1` 守卫做摘除式变异，确认尾段测试失败。

## 4. 路线图逐步骤裁决

原稿实际存在 S40～S46；**没有 S47、S48、S49**。本文不虚构这三个步骤。相关交叉步骤按原编号列出。

| 步骤 | 可行性裁决与必须修改的验收/依赖/回滚 |
|---|---|
| S40 | **有条件可行，当前验收/回滚不安全。** 未知结果分类、回填 Result、确认顺序正确；“事件回写失败不再 ack”应写“不再标本地 processed”，入站仍及时 ACK durable inbox。401/403 无业务码不等同 token expired，更不能仅第二次401就认定记录永久失败。限次到暂停/待核实；与 S41 的限次/seed 必须原子落地或先给未知状态停止自动执行。删掉“噪声时临时判 Fatal”回滚；降级应暂停消费者/限制重试，不写未知失败格。Fatal 回填失败归 Skipped+alert 也要保留可重试/可修复事实，不能冒充同步完成。 |
| S41 | **范围正确，2人日与严格一次验收不足以证明。** “发送成功+更新失败下一轮不再发”只有稳定持久 UUID 且在上游一小时幂等窗口内才可验证；notified_at/意图本身不保证严格一次。补 N4、N5、N3。不能对回填格原本一直为空的 capped 行每轮重置 attempts；需显式 retry 命令/代次，或可证明的从非空到空用户动作。Waiting 不消耗错误上限。trigger 有 applicant，channel(1) 合并必须先持久化每条受理意图，不能丢不同申请人。10k+ seed 改为持久游标分批/增量并保证有限轮覆盖，不能只放宽页数。 |
| S42 | **方向必要，接口破坏风险真实。** 202 前原子写任务、UUID、申请人和配置执行版本/快照；异步队列必须接管异常和恢复。不能在 enqueue 之后才保存申请人且 warn-only。旧同步 flag 若保留，必须共用 claim，不能维持原双执行者。202前 request log 可失败降级，但 durable intent 不可失败降级。验收要断开客户端、超时/重启、外部成功本地失败、重复按钮及旧请求最后返回；不只看返回快。 |
| S43 | **部分可行。** 解密/配置失败不回成功正确；成功 event_callback 仍回200。缺 uuid 最小安全策略是清楚拒绝不支持形态；若派生哈希，原建议四字段不能区分不同 task，且缺字段时仍会坍缩，须规范化完整稳定业务身份。修 raw body日志/存储秘密旁路一并完成。不要让所有匿名畸形请求都按服务器故障反复重投。 |
| S44 | **只降低丢凭据概率，不能承诺 refresh token“不丢”。** 必须加 N1 授权代次/CAS、跨实例协调，以及 pending 槽先于可取消 DB await。槽按授权代次有效，重新授权淘汰旧槽。进程崩溃/令牌端点响应丢失仍需重新授权；验证不得仅用同进程第二次调用。禁止把“只存内存”当持久化补偿。 |
| S45 | **可行，纠正 DNS 结论和测试方法。** client级 read/connect + 每请求总预算；连接 timeout 覆盖连接 future 中 DNS 等待；黑洞 IP 不稳定，换本地可控 transport/resolver。框架 HttpTimeout retry 分类同步改。错误 body 读取分类、未知结果保守处置应配套；URL路径脱敏不能代替错误 message/body 脱敏。无数据/schema 变化不代表语义回滚无风险，恢复旧错误分类会恢复 D4-1。 |
| S46 | **可行但不实现跨轮复用。** Bytes 消除深拷贝，并不能让下一轮不下载；上传 `idempotent=false`，跨轮 token/code 无持久缓存。删除收益中“避免整包重下载”的无条件说法；若另加缓存，必须定义源文件身份、有效期、失败清理，不能混入本最小改动。验收共享分配、Content-Length、合法大附件，并由 S59 验实际并发峰值。 |
| S47 / S48 / S49 | **原文没有编号，不适用。** |
| S00 / S01 | **必要但要更新状态。** 基线最终5c0ea9c，原在制品描述已过期；同日S01历史证据已排除测试机开关true，财务机仍未知。本轮未再次采集；不应重复要求已完成的事实采集，也不能把历史资料当当前live。 |
| S11 / S14 | **worker开关是减轻并发，不是 fencing。** 默认值兼容旧部署，smoke确实传false且受配置合成优先级控制；schema只读门禁独立。关闭 smoke worker仍留下旧版本排空、HTTP直接派发等并发，需S42/N3。 |
| S15 / S56 | **监督/取消必要，执行点需谨慎。** 中止未来等待不能撤销远端已完成的创建/通知；可取消前先 durable intent，停止时保留待核实任务。abort 已运行 spawn_blocking 不生效，XLSX另需协作取消/permit生命周期。重启/stop预算变化不能独立证明副作用安全。 |
| S50 | **可行但与S40/D3-6合并去重计费。** attempt与logical outcome分开、加入事件最老年龄/回填失败/通知未知/refresh pending指标。采集端按S01历史未有，新增Prometheus规则不等于闭环；要有实际告警演练。 |
| S52 | **Redis一次性方案可行；回滚内存fallback须删/限定。** SET NX EX+GETDEL，consume失败fail-closed；无条件 fallback将重新产生A签发B消费失败，以及双存储接受域，削弱一致性。若混版必须明确版本化state路由与独立接受集合，最小回滚是提示旧state重做授权。功能关闭可无Redis依赖，启用时不能承诺Redis故障仍无影响。与S44不是天然先后依赖，但都受授权代次契约约束。 |
| S59 | **仅容器限额不能替代N6准入。** 先小规模测附件/XLSX展开/并发、阻塞任务取消后峰值，再定内存；在唯一生产机进行300并发导入不适合作为首次验收。给合法最大输入、并发数和OOM恢复分别设边界。 |
| S71 | **加密方向可行，回滚描述错误。** CBC协议解密不能直接用作凭据存储；可复用项目既有AEAD经验但需独立密钥域/版本。双读双写明文期间，秘密仍在库和备份中，收益不能提前宣称完成；必须有停止明文写、清理历史和备份到期计划。恢复迁移前数据库快照可能恢复已经被飞书作废的旧 refresh token，不是可靠回滚。优先兼容旧新密文的代码回滚/向前修复，必要时明确重新授权，不能隐式回退业务数据库。补N7的秘密清单。 |
| S74 | **应纳入本审核反例。** 用户refresh和state失败closed；tenant token缓存故障已有直接取新token降级，不得误套用户refresh单次语义；创建slot失败停止派发，保留待办；通知未知不能报sent；上传超时不得当确定失败。补N8真实Redis窗口尾段测试，不能只测纯函数阈值或正常满60秒过期。 |
| S76 | **只读报告先做，自动对账可行性尚未充分证明。** 由持久UUID/坐标可恢复的任务先处理；“删掉本地行便能重建所有孤儿”还需能枚举远端实例并恢复其本地config/record身份。来源行已删、配置变更、UUID不能反解时，仅凭远端instance不能可靠恢复意图。先明确候选集合、分页、权限、API配额和歧义人工处置；纳入N2终态后事件与N3乱序，不能只处理pending。 |
| S77 | **必要但不能先删恢复证据。** request log/event raw_payload/通知意图保留期分别定义；去掉token不必删除业务事件。成功但未回填/未知通知、对账未解决的行不能按一般日志年龄删除。脱敏、凭据轮换、备份留存是独立动作；本轮没有执行。 |

## 5. 验证与未验证边界

已执行只读 `git status --short`、`git rev-parse HEAD`、CodeGraph、定向 `rg`/行号读取，最终源码读取成功。一次对 Cargo registry 使用 shell 通配路径的 `rg` 返回 exit 1（Windows路径通配不展开），随后以实际绝对目录重试成功；不影响结论。Python限流时间模型 exit 0，复现182个预订/60秒；未把模型算作真实Redis或真实飞书验证。

本分工未运行 Cargo/pnpm、真实数据库测试、压力测试、浏览器、真实飞书、生产端点、SSH或凭据读取。N8引用协调代理的真实隔离Redis+当前Lua复现；其余新反例除N5公式模型外属于可追溯静态执行路径。具体线上频率、真实上游去重窗口/事件语义/配额主体、最大安全文件和峰值RSS仍须隔离环境验证。代码修复未在本报告范围实施。

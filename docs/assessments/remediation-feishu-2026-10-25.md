# 飞书基础设施修复记录（2026-10-25）

依据 `2026-10-25-infrastructure-review-feishu-outbound.md`。已实施 N1、N2、N5、N7、N8 和 D4-6/D4-8；N3/N4/N6 及下文列出的 D4 缺口仍未完成，不能由本批通过推导飞书全域已完成。按用户最新要求，当前小批次验证后冻结源码，交协调代理运行全量门禁。

## 验证边界

- 所有凭据均为测试字符串。受控传输回放不访问飞书、不发送消息。
- MySQL 使用新建无宿主卷容器 `codex-feishu-remediation-mysql-20261025`，库 `codex_feishu_remediation_test`；Redis 使用独立容器 `codex-feishu-remediation-redis-20261025` 的 DB 15，仅发布到 loopback 临时端口。
- 不读取应用真实配置或凭据、不使用共享默认数据库、不接触生产。本记录不代表线上事故频率或已部署行为。

## N1：OAuth 刷新与重新授权串行化

回归在独立 MySQL 连接上持有用户凭据行锁，再调用真实刷新门面；旧实现仍立即进入 provider，红测明确失败。已用数据库事务行锁覆盖刷新读取、出站刷新和落库，重新授权普通 UPDATE 自然等待同一行锁；锁拥有者由事务连接确定，锁等待最多五秒，取消/失败由事务回滚释放。

完整授权写入与刷新现在共享数据库互斥边界，故不另加 schema 代次或第二套 CAS。真实库回归验证跨连接锁冲突、有界超时、释放后恢复，以及刷新中到达的另一用户授权最终保留完整身份/凭据/授权时间。它是独立数据库连接验证，并非真实双进程或真实飞书验证。

范围：`domain/user_token.rs`、`domain/repository.rs` 与真实库回归。无 schema / 外部 API 变更。

保留 D4-4/S44：飞书签发成功后的响应丢失、进程崩溃或取消仍可能丢失新 refresh token；本批没有持久补偿或声称彻底解决。

## N2：终态后的新事件继续同步

删除按本地 `instance_status` 终态跳过整个事件的判断。使用新事件获取最新详情、回写扩展字段并更新已有 `last_synced_at`。同一事件的入口 UUID 唯一索引去重保持不变。

回归目标：本地 APPROVED 之后远端 `reverted=true` 和 DELETED 均真正回写；同 UUID 的重复投递只落一行。

保留 N3/D4-2：跨实例无 fencing 的旧回写覆盖、扩展回写失败确认过早是独立缺口；本批不会把删除终态 guard 当作已解决并发乱序或持久化确认。远端返回 NotFound 时仍保留失败，不把缺失实例推断成 DELETED。

## N7：事件认证材料不写诊断快照或日志

日志捕获回归已在旧代码检出测试 verification token。覆盖 challenge 缺字段、异常 type、正确加密 challenge 和解密后非法 JSON；落库回归校验 raw_payload 无顶层 token。

修复范围为事件接收及 challenge 两入口：认证后删除顶层 token，再持久化业务报文；不打印解密报文、原始 body、解密预览或解析错误原文。

保留生产历史清理、备份到期与凭据轮换：这些没有执行，也不由停止新写入自动完成。可选原始请求日志中间件另属 D6-1 范围。

## N8：门户限流不延长亚秒尾段

真实 Redis 红测：以 `SET ... PX 300` 建立正常尾段，旧 Lua 返回 `(32,60)`，预期 `(32,0)`。最小修复将补 TTL 条件从 `ttl < 1` 收窄为 `ttl == -1`，保持原阈值、门户 ID/IP 桶及最小一秒 Retry-After。

回归覆盖持续尾段请求、原窗口自然到期后计数恢复，以及异常无过期键仍补 TTL。

## 最终验证

- `cargo test --lib --locked`：1275 passed、13 ignored，exit 0；含事件日志捕获回归。
- 独立真实 MySQL：`real_mysql_round_trips_secret_token_columns` 通过；覆盖授权、刷新、锁等待超时与重授权交错。
- 独立真实 Redis：`real_redis_counter_preserves_subsecond_window_and_repairs_missing_expiry` 通过。
- 独立 MySQL/Redis + 受控回放：`terminal_task_still_syncs_later_reverted_and_deleted_events` 通过；确实回写撤销/删除文案，并保存同步时间。
- 独立 MySQL/Redis：`shared_challenge_url_accepts_encrypted_event_callback` 通过；同时校验 raw_payload 脱敏、同 UUID 重投不重复落行。四项变异恢复后，上述四个真实依赖回归均已再次通过。
- `python scripts/check_architecture.py` 与 `git diff --check` 通过。

摘除式变异全部有效变红，随后恢复：N1 将 FOR UPDATE 改普通 SELECT；N2 恢复本地终态短路；N7 摘除 token 删除语句；N8 摘除 `ttl == -1` 守卫。分别被跨连接锁、真实回写次数、落库凭据快照、窗口尾段断言捕获。

一次误用 `real_` 过滤运行了九项 ignored 测试，八项隔离测试通过；额外匹配到 `real_app_sends_rich_feedback_and_confirms_sender_and_content`，在缺少 `FEEDBACK_SMOKE_CONFIG` 的首个断言立即失败。根据该入口代码，失败发生在读取配置/凭据、构建 HTTP 客户端之前，未外发消息；后续均使用具体测试名。新增回归的首次编译类型错误、SQL 夹具必填项/唯一坐标及字段列表 total 缺失已修复，不计作业务红测证据。

完整 quick/full、Clippy、所有真实依赖入口、部署及上游真机验收由协调代理继续处理；本批不声称这些已经完成。新增两个 `YANG_SYSTEM_TEST_*` 消费文件需由协调代理登记 `scripts/run_ci.py` 集成测试清单。

## N5：每次实际审批创建出站领取共享名额

门控从 HTTP/worker 的业务前置步骤移到 `HttpClientTransport::send` 的审批创建 POST；内部 5xx 重试与 token 刷新后的重发均再次领取名额。未配置共享门控的真实 transport 对创建请求失败关闭，读取详情和附件操作不消耗创建配额。

Redis 键按 `app_id` 区分；同一应用的部署必须共用 Redis。Lua 只批准当前可发送的请求，其他调用只收到 Redis 计算的相对等待值，等待后重新竞争，不再预订未来时间片。键过期不会让积压预订重叠；本机时钟不参与判断。间隔向上取整；单次领取总预算五秒，失败保留可重试限速结果，不写业务失败。

兼容性边界：键名改变，旧版本仍使用 deployment 桶，混版期间两个算法不能保证共享配额；发布时需停止旧创建写者后切换。本次未部署。不同 Redis 实例之间、以及不经过本服务的其他飞书调用不在该门控保证范围内。

红测实际复现 15 个并发等待者中相邻放行仅约 42ms（配置间隔 100ms），并检出 666ms 向下取整和未配置门控仍尝试连接的问题。真实 Redis + 本机受控 HTTP 绿测覆盖重试、两个独立 transport 共享应用桶、只读绕过和等待预算；没有访问真实飞书。

本批恢复后定向测试 **13 passed，0 failed**（7.78 秒），架构检查与 scoped `git diff --check` 通过。摘除 ceil、Redis 等待判定、传输层门控、429 可重试分类、五个 challenge 错误出口、事件身份守卫后，七个对应用例全部变红；单独将五秒预算放宽到三十秒，六秒外层验收 deadline 确认变红。所有变异已逐字恢复。三项新增 ignored 用例由协调代理登记到集成门禁：`real_redis_create_gate_keeps_spacing_when_waiters_outlive_key_ttl`、`real_redis_create_gate_wait_is_bounded_and_does_not_reserve_future_slots`、`real_redis_http_create_limit_covers_retries_and_independent_transports`。

## D4-6 / D4-8：回调错误不伪装成功，事件身份先校验

challenge 共用事件 URL 的配置/密钥错误返回框架服务端错误；解密后坏 JSON、缺 challenge、不支持的 type 返回校验错误。成功 challenge 和成功落库的 event_callback 仍保持原 raw JSON ACK。

事件接收在事务前校验 `type=event_callback`、`approval_instance|approval_task`、非空顶层 `uuid` 和 `instance_code`（各最多 128 字符），不再把缺失身份落成唯一空键，也不自行推断替代 UUID。红测分别检出错误 challenge 返回成功 ACK 和缺 UUID 已进入数据库访问；修复后定向用例通过。外部重投策略、坏密钥恢复时间以及真实上游事件版本仍未在线验证。

## 尚未完成

### P08 第一阶段进行中（认领子段已验证）

新增 `real_mysql_worker_claims_are_exclusive_and_old_owner_cannot_commit`，使用两个独立 MySQL 连接池竞争同一任务，再模拟新持有者接管。旧代码实测结果 `(2, "等待输入")`，预期 `(1, "新持有者的结果")`：同时证实重复执行权与旧持有者覆盖。新实现使用事务 `FOR UPDATE SKIP LOCKED`、随机 owner、单调 generation 和数据库时钟租约；最终写入要求有效 owner/代次/租约且恰一行受影响。等待与错误重试分别处理，错误重试有退避与十次上限；播种不再把现有状态强制重置。

认领子段恢复后该真实依赖用例 **1 passed，0 failed**（0.90 秒）。随后已接入单条 HTTP 持久受理：配置与申请人首份快照胜出，HTTP 与 worker 共用执行权；创建/回填出站前保存阶段，创建成功在后续详情查询前保存实例编号。超时、非 2xx 成功信封、坏响应与本地确认失败不自动重放，未知阶段过期转 `review`。配置删除与受理锁同一配置行，待执行、在途或 `review` 任务阻止删除；扩展字段回填失败不再提前确认任务或事件完成。

当前新增五项独立 MySQL + 受控 transport 回归，覆盖快照/身份不可改写、远端成功后本地确认失败、未知写只发送一次、owner/代次/租约/阶段守卫以及持久重试预算；另补两项扩展字段回填单测、扩展现有事件回放失败分支、修正删除配置合同测试。上述新增行为已运行 Cargo 验证（2026-10-26，均 exit 0）：5 项真实 MySQL 执行回归、1 项独占认领、2 项扩展回填单测、12 项派发集成（含删除配置合同与事件回放）、1 项终态事件回放分别 5/1/2/12/1 passed（日志 `target/infrastructure-remediation/feishu/green-restored-{1..5}.log`）。另对 8 个真实不变量做摘除式变异，全部被对应业务断言杀死，恢复后 SHA256 与原始一致（`mutation-results.log`）：跨配置/身份守卫、停用配置受理门、执行阶段守卫、未知结果隔离状态、过期未知转 `review`、`finish` 代次守卫、重试预算上限、删除配置活动任务守卫。验证中另修正两处测试隔离缺口（`feishu_approval_task_applicant` 未纳入集成测试 reset、`recDisabled` 固定记录跨轮残留），仅改测试清理与夹具唯一性，未改业务断言；修正后整套 5 项绿测复跑仍全部通过。

批量受理仍依赖内存信号，尚无持久分页展开；事件尚未原子认领或共享任务写权；未知创建的只读核查及通知 outbox 尚未接入。当前只是 P08 中间落点，不能宣称 P08 已完成。

- N3：HTTP 首次执行前 durable intent、真正 claim/执行代次、事件与派发共享回写权；远端无 fencing 时超时/失联的有界核查状态。
- N4：独立持久通知意图、成功通知失败重扫、稳定 UUID、新结果不被历史 HTTP sent 压制。
- N6：XLSX 共享字符串展开预算、解析并发准入、blocking 工作结束前持有 permit、协作取消。
- 关联 D4-1/2/3/4/5/7/9/10/11：未知创建结果保守处置、扩展回填确认顺序、持久异步受理、刷新凭据丢失窗口、有界退避、并发 worker、通知去重与观测仍不能由本批视为解决。

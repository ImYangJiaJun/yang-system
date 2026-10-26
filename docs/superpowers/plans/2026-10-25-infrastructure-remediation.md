# 全基础设施修复实施计划

> **2026-10-25 用户已暂停目标。** 最新工作树、已验证范围、当前 5 项配置红测和恢复顺序见[交接文档](../../assessments/2026-10-25-infrastructure-remediation-handoff.md)。暂停不表示修复完成。

> **For agentic workers:** 使用 subagent-driven-development 或 executing-plans 按包推进；本计划与专项修复记录共同追踪。用户已明确授权按照审核结果全量修复。

**Goal:** 在新分支完整修复审核与全量测试中成立的问题，逐项保留实现、验证及现场前置证据。

**Architecture:** 保留模块化单体、声明式 Schema、Tools 依赖注入和前端分层。重复发现按共享根因修复；被推翻的建议不实施，但同条仍成立的问题必须收口。

**Tech Stack:** Rust 2021 / MSRV 1.80、MySQL 8、Redis 7、React 19、TypeScript 5.9、pnpm 10.33.1。

**Spec:** [审核总入口](../../assessments/2026-10-25-all-infrastructure-adversarial-review.md)、[全量测试结果](../../assessments/2026-10-25-full-test-results.md)及其四份附录。

## 全局约束

- 两仓分支均为 `codex/infrastructure-remediation-20261025`；应用起点 `5c0ea9c`，框架起点 `e27a2d3`。保留无关未跟踪文件，显式路径提交；不擅自 push、部署或生产写入。
- CodeGraph 优先定位；不新增 SQL 迁移和数据库单例；配置、环境、secret、样例和生成合同同批联动。
- 行为修复遵循失败回归→最小改动→摘除式变异→恢复验证；真实依赖只用独立 `_test` 库和 Redis DB 15。
- 状态区分待处理、实施中、代码已验、已反证、待现场/业务；没有最终证据不得关闭目标。全量门禁绿色不替代新增失效场景验证。

## 工作包

- [ ] P01 授权 DI-01：`authorization/{version_cache,request_validator,worker}.rs`；旧事件只失效缓存，事实快照的 5 秒预算从读前开始，延迟写不能续期。验收真实 Redis/MySQL 乱序、过期、ack 失败及暂停后的回填。
- [ ] P02 身份 DI-02/03：account session/context 与 token 验证链；稳定会话撤销、refresh 当前代次、恢复世代。验收历史 Access、旧 refresh、并发、空 Redis 和其他设备正例。
- [ ] P03 MFA/密钥 DI-04/05/06：config、mfa、secret 绑定与恢复文档；digits 承诺一致、密钥域隔离、密文可恢复。不擅自恢复已移除的 Step-up 产品流程。2026-10-26 代码已验证：digits 仅 6、TOTP 跨域隔离、4 条 secret 绑定与文档同步；79 项 config 绿测 + 6 类摘除变异通过；最终总门禁待完成，见[配置修复记录](../../assessments/remediation-config-2026-10-26.md)。
- [ ] P04 数据依赖 D5、DI-07/08：yang-db 池/命令总预算、TLS、日志脱敏、MySQL 会话/DDL取消、真实池指标。验收半死多槽、已借连接 stall、锁等待与部分 DDL。
- [ ] P05 入站 D1/S11/22：真实 axum 层序、全局过载/错误信封、公开入口工作预算；单文件16MiB、multipart整体17MiB、JSON16MiB。验收跨路由/方法、404/405、精确边界。
- [ ] P06 部署恢复 D2/E01-E09：deploy 脚本/Docker/备份；curl退出码、停止失败、发布锁/image ID/config绑定、代理路径、原子备份。验收替身变异与本地容器。
- [ ] P07 飞书 N1/N2/N7/N8：OAuth行锁、后续撤销事件、秘密出口、TTL尾段。验收真实隔离DB/Redis与受控外部transport。
- [ ] P08 飞书 N3/N4、D4/S40-44：认领代次与持久执行第二段已通过 5+1+2+12+1 绿测及 8 项摘除变异；N3/N4、批量受理、事件共享写权、未知只读核查、通知 outbox/重扫仍开放。验收并发旧写者、远端成功本地失败及历史通知不抑制新结果。
- [ ] P09 飞书 N5/N6、D3：实际尝试限流、HTTP完整预算/错误分类、XLSX累计展开与blocking所有权。验收可控故障上游和低预算恶意文件。
- [ ] P10 生命周期 R-01/R-05/D7：shutdown取消不detach、总预算、smoke无副作用、cleanup有界；逐worker取消及阶段失败。
- [ ] P11 可观测 R-04/D7/D5-8：全入站指标、ready新鲜度、目标缺席、采集资产与告警。验收promtool、隔离全链；实际通知保留现场状态。
- [ ] P12 前端 R-02/R-03/D8：代次、锁前deadline、恢复错误、请求身份/取消、错误元信息、Query重试、gzip、关闭按钮和视觉。验收单测/变异/Chromium/Nginx。
- [ ] P13 凭据 D6/S52/53/71：重置fragment兼容query、OAuth跨实例state、日志秘密出口、静态加密和密钥版本。验收旧链接、重放与日志哨兵。
- [ ] P14 测试/供应链 S75：runner UTF8、registry能力、ignored完整登记、两仓制品证据；最终应用full/integration/视觉/MSRV与框架受影响门禁。
- [ ] P15 现场 S01/02/30/31/32/50/57/59/70/74/76/77：准备可复审产物，明确拓扑、容量、恢复与业务保留期依据，必要授权作为最后一步；未完成现场项不关单。

## Review Focus

1. 旧异步结果不能覆盖新身份、权限、任务代次；超时后不能重新延长授权缓存。
2. 远端成功而本地确认失败必须可重扫/对账；未知不变成确定失败。
3. 取消不能留下任务、连接锁或无上限内存；合法16MiB文件必须仍能上传。
4. Redis不可达与可达空状态分别验证；旧能力不能复活，其他设备不误踢。
5. 蓝绿制品、配置、Schema策略与worker启停一致；回滚和备份不能假绿。

## 当前分工

2026-10-25 测试检查点：按用户要求执行当前修复树的 full、integration、视觉与相关框架/部署门禁，见[本轮全量测试记录](../../assessments/2026-10-25-remediation-full-test-results.md)。此检查点不关闭尚未实施的工作包或现场验收项。

2026-10-26 新稳定快照检查点（四阶段全绿）：配置 79 项 + 6 类变异、身份 7 项回归 + 8 项变异、运行期 6 项变异、飞书 5+1+2+12+1 项绿测 + 8 项变异、XLSX 49/19/9、部署 32/16 已验；应用 full 1300 passed/176 ignored、integration 38 组 174 passed、视觉 2、框架 874+12、MSRV 1.80.1 与 doctest 通过。完整证据见[检查点](../../assessments/2026-10-26-remediation-checkpoint.md)。未实现的工作包（P01/P04/P08 批量与通知/P11/P13/P15 等）仍开放，不因本检查点关闭。

根代理：P01–05、P10/11/13/14及最终整合；部署代理P06；飞书代理P07–09；前端代理P12并配合上传边界。每包专项记录存于 `docs/assessments/remediation-*-2026-10-25.md`。下列覆盖表只在获得最终证据后更新状态。

## 原68项发现覆盖

| ID | 来源 | 状态/证据 |
|---|---|---|
| D1-1 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D1-2 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D1-3 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D1-4 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D1-5 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D1-6 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D1-7 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D1-8 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D1-9 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D2-1 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D2-2 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D2-3 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D2-4 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D2-5 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D2-6 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D2-7 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D2-8 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D2-9 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D2-10 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D2-11 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D2-12 | [复审](../../assessments/2026-10-25-infrastructure-review-edge-delivery.md) | 待逐项验收 |
| D3-1 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D3-2 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D3-3 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D3-4 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D3-5 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D3-6 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D3-7 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D4-1 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D4-2 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D4-3 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D4-4 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D4-5 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D4-6 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D4-7 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D4-8 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D4-9 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D4-10 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D4-11 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D5-1 | [复审](../../assessments/2026-10-25-infrastructure-review-data-identity.md) | 待逐项验收 |
| D5-2 | [复审](../../assessments/2026-10-25-infrastructure-review-data-identity.md) | 待逐项验收 |
| D5-3 | [复审](../../assessments/2026-10-25-infrastructure-review-data-identity.md) | 待逐项验收 |
| D5-4 | [复审](../../assessments/2026-10-25-infrastructure-review-data-identity.md) | 待逐项验收 |
| D5-5 | [复审](../../assessments/2026-10-25-infrastructure-review-data-identity.md) | 待逐项验收 |
| D5-6 | [复审](../../assessments/2026-10-25-infrastructure-review-data-identity.md) | 待逐项验收 |
| D5-7 | [复审](../../assessments/2026-10-25-infrastructure-review-data-identity.md) | 待逐项验收 |
| D5-8 | [复审](../../assessments/2026-10-25-infrastructure-review-data-identity.md) | 待逐项验收 |
| D5-9 | [复审](../../assessments/2026-10-25-infrastructure-review-data-identity.md) | 待逐项验收 |
| D6-1 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D6-2 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D6-3 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D6-4 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D6-5 | [复审](../../assessments/2026-10-25-infrastructure-review-feishu-outbound.md) | 待逐项验收 |
| D7-1 | [复审](../../assessments/2026-10-25-infrastructure-review-runtime-frontend.md) | 待逐项验收 |
| D7-2 | [复审](../../assessments/2026-10-25-infrastructure-review-runtime-frontend.md) | 待逐项验收 |
| D7-3 | [复审](../../assessments/2026-10-25-infrastructure-review-runtime-frontend.md) | 待逐项验收 |
| D7-4 | [复审](../../assessments/2026-10-25-infrastructure-review-runtime-frontend.md) | 待逐项验收 |
| D7-5 | [复审](../../assessments/2026-10-25-infrastructure-review-runtime-frontend.md) | 待逐项验收 |
| D7-6 | [复审](../../assessments/2026-10-25-infrastructure-review-runtime-frontend.md) | 待逐项验收 |
| D7-7 | [复审](../../assessments/2026-10-25-infrastructure-review-runtime-frontend.md) | 待逐项验收 |
| D8-1 | [复审](../../assessments/2026-10-25-infrastructure-review-runtime-frontend.md) | 待逐项验收 |
| D8-2 | [复审](../../assessments/2026-10-25-infrastructure-review-runtime-frontend.md) | 待逐项验收 |
| D8-3 | [复审](../../assessments/2026-10-25-infrastructure-review-runtime-frontend.md) | 待逐项验收 |
| D8-4 | [复审](../../assessments/2026-10-25-infrastructure-review-runtime-frontend.md) | 待逐项验收 |
| D8-5 | [复审](../../assessments/2026-10-25-infrastructure-review-runtime-frontend.md) | 待逐项验收 |
| D8-6 | [复审](../../assessments/2026-10-25-infrastructure-review-runtime-frontend.md) | 待逐项验收 |
| D8-7 | [复审](../../assessments/2026-10-25-infrastructure-review-runtime-frontend.md) | 待逐项验收 |
| D8-8 | [复审](../../assessments/2026-10-25-infrastructure-review-runtime-frontend.md) | 待逐项验收 |

## 原36步骤覆盖

| ID | 状态/证据 |
|---|---|
| S00 | 待按复审裁决验收 |
| S01 | 待按复审裁决验收 |
| S02 | 待按复审裁决验收 |
| S10 | 待按复审裁决验收 |
| S11 | 待按复审裁决验收 |
| S12 | 待按复审裁决验收 |
| S20 | 待按复审裁决验收 |
| S21 | 待按复审裁决验收 |
| S22 | 待按复审裁决验收 |
| S45 | 待按复审裁决验收 |
| S60 | 待按复审裁决验收 |
| S40 | 待按复审裁决验收 |
| S43 | 待按复审裁决验收 |
| S44 | 待按复审裁决验收 |
| S70 | 待按复审裁决验收 |
| S14 | 待按复审裁决验收 |
| S28 | 待按复审裁决验收 |
| S30 | 待按复审裁决验收 |
| S33 | 待按复审裁决验收 |
| S41 | 待按复审裁决验收 |
| S46 | 待按复审裁决验收 |
| S52 | 待按复审裁决验收 |
| S53 | 待按复审裁决验收 |
| S56 | 待按复审裁决验收 |
| S15 | 待按复审裁决验收 |
| S42 | 待按复审裁决验收 |
| S71 | 待按复审裁决验收 |
| S74 | 待按复审裁决验收 |
| S50 | 待按复审裁决验收 |
| S31 | 待按复审裁决验收 |
| S57 | 待按复审裁决验收 |
| S76 | 待按复审裁决验收 |
| S77 | 待按复审裁决验收 |
| S32 | 待按复审裁决验收 |
| S59 | 待按复审裁决验收 |
| S75 | 待按复审裁决验收 |

## 新增发现与测试问题

| ID | 状态/证据 |
|---|---|
| E01 | 待逐项验收 |
| E02 | 待逐项验收 |
| E03 | 待逐项验收 |
| E04 | 待逐项验收 |
| E05 | 待逐项验收 |
| E06 | 待逐项验收 |
| E07 | 待逐项验收 |
| E08 | 待逐项验收 |
| E09 | 待逐项验收 |
| DI-01 | 待逐项验收 |
| DI-02 | 持久会话权威与 Refresh CAS 已实现；7 项真实回归 + 8 项摘除变异通过；新快照总门禁待完成，见[身份记录](../../assessments/remediation-identity-2026-10-25.md) |
| DI-03 | 旧 Refresh 在 Redis 空恢复后拒绝已验；TOTP 消费记录、MySQL 旧备份恢复世代仍开放，见[身份记录](../../assessments/remediation-identity-2026-10-25.md) |
| DI-04 | digits 已收窄为仅 6；79 项 config 绿测与摘除变异通过；最终总门禁待完成，见[配置修复记录](../../assessments/remediation-config-2026-10-26.md) |
| DI-05 | 轮换文档已改为不可解密/需独立受控备份/无无缝轮换工具；未实现多 keyring，见[配置修复记录](../../assessments/remediation-config-2026-10-26.md) |
| DI-06 | TOTP 跨域隔离与 4 条 secret 绑定已实现；79 项 config 绿测与绑定摘除变异通过，见[配置修复记录](../../assessments/remediation-config-2026-10-26.md) |
| DI-07 | 待逐项验收 |
| DI-08 | 待逐项验收 |
| N1 | 数据库行锁串行化已实现并通过真实跨连接及摘除验证；刷新响应丢失另属 D4-4，见[飞书记录](../../assessments/remediation-feishu-2026-10-25.md) |
| N2 | 新终态事件不再跳过，真实回写与摘除验证通过；事件共享写权仍属 N3，见[飞书记录](../../assessments/remediation-feishu-2026-10-25.md) |
| N3 | 待逐项验收 |
| N4 | 待逐项验收 |
| N5 | 每次真实创建尝试共享限流已实现；真实 Redis/受控 HTTP 与摘除验证通过；混版发布仍需停旧写者，见[飞书记录](../../assessments/remediation-feishu-2026-10-25.md) |
| N6 | 累计展开、解析准入及取消所有权已实现；49 单测、19 真实集成、9 摘除验证通过；新总门禁待完成，见[XLSX 记录](../../assessments/remediation-xlsx-2026-10-25.md) |
| N7 | 新事件快照和诊断出口脱敏已验；历史数据清理/凭据轮换未执行，见[飞书记录](../../assessments/remediation-feishu-2026-10-25.md) |
| N8 | 亚秒尾段 TTL 不再延长；真实 Redis 和摘除验证通过，见[飞书记录](../../assessments/remediation-feishu-2026-10-25.md) |
| R-01 | OwnedTask 保留句柄/Drop abort、总预算与共同截止时间已实现；runtime 基线/恢复绿测 + 摘除变异通过，见[运行期记录](../../assessments/remediation-runtime-2026-10-25.md) |
| R-02 | 本地会话代次保护已实现并专项验证，见[前端记录](../../assessments/remediation-frontend-2026-10-25.md)；新快照总门禁待完成 |
| R-03 | 锁前总预算/取消已实现并专项验证，见[前端记录](../../assessments/remediation-frontend-2026-10-25.md)；新快照总门禁待完成 |
| R-04 | 全入站计数/耗时/安全访问日志已实现，真实 Router 与摘除验证通过，见[入站记录](../../assessments/remediation-inbound-2026-10-25.md)；生产采集消费者仍待现场 |
| R-05 | cleanup 分批、刷新复核、积压短间隔已实现；真实 MySQL batch 绿测 + 摘除变异通过，见[运行期记录](../../assessments/remediation-runtime-2026-10-25.md) |
| TEST-视觉 | 待逐项验收 |
| TEST-React诊断 | 待逐项验收 |
| TEST-UTF8 | 待逐项验收 |
| TEST-registry | 待逐项验收 |
| TEST-ignored登记 | 待逐项验收 |

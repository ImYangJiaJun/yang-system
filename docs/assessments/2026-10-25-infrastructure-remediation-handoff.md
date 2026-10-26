# 基础设施审核与全量修复交接

交接日期：2026-10-25。暂停快照采集时间：`2026-10-25T15:59:30Z`。

> **2026-10-26 接手进度（覆盖下方暂停说明）**：用户已恢复目标。P03 配置红转绿（79 项绿测 + 6 类摘除变异）、身份 8 项变异、运行期 6 项变异、飞书 P08 第二段 21 项绿测 + 8 项变异、四阶段全量检查点均已完成并全绿。最新证据见 [2026-10-26 检查点](2026-10-26-remediation-checkpoint.md) 与 [配置修复记录](remediation-config-2026-10-26.md)。本文第 1 节的“用户已暂停”只描述 2026-10-25 时点；第 6 节第 1–6 步已执行，未实现的工作包与现场项继续按第 7–9 步推进。

## 1. 接手先看

**用户已主动暂停“新建一个分支，按照结果进行全量修复”目标。本次只整理交接，没有恢复修复、部署或外发。** 原目标尚未完成；继续实现须等用户恢复目标。

当前最重要的状态：

1. 全基础设施审核及原报告的 **68 项 D 发现、36 项 S 步骤**逐项复审已交付，另外补出 E01–E09、DI-01–DI-08、N1–N8、R-01–R-05 共 30 项发现。
2. 两仓修复分支已建立，大量实现和测试已落盘，**尚未提交 Git、推送或部署**。两仓暂存区均为空，不能只取 HEAD 重现当前实现。
3. 曾完成一轮全量绿色检查，但它对应旧源码快照。之后继续修改了身份会话、XLSX、生命周期、审批持久化与配置回归，**旧 full/integration/MSRV 结果不能代表当前树**。
4. 最后一次运行 `cargo test --lib config:: --locked -j 1` 已结束，退出 **101**：**74 passed、5 failed**。这 5 项是刚补的有效失败回归，P03 生产修正尚未实施，当前工作树不是全绿状态。不要删除/忽略这些测试来取得绿色。
5. 身份 7 项真实依赖回归通过，摘除验证待跑；XLSX 49 单测、19 真实集成、9 项摘除验证通过；部署 32 行为测试、16 项摘除验证通过。运行期和最新审批批次仍需验证。
6. 暂停时没有观察到活跃 `cargo`、`rustc`、`link` 进程；最后的工具会话 `56976` 已收取退出码。三个修复代理均确认停止，无各自后台测试进程。四个专用 MySQL/Redis 容器保留运行，供恢复后续用。

## 2. 工作区与资料入口

| 仓库 | 路径 | 分支 | HEAD |
|---|---|---|---|
| 应用 | `D:/code/lib_yang/project/yang-system` | `codex/infrastructure-remediation-20261025` | `5c0ea9cf8c1afe09f8a2cebe1354456c9c703bea` |
| 框架 | `D:/code/lib_yang` | 同上 | `e27a2d3c127e0bea6c40bc5fa584b2fdb2162705` |

应用依赖同工作树的 `../../crates/yang-base`、`yang-db`、`yang-runtime`。这两个目录是独立 Git 仓库，不要把应用当作根 workspace 的普通成员。两仓都有 `.codegraph/`；定位代码先用 CodeGraph，再补未展示的精确范围。

暂停时应用有 101 个已跟踪文件发生变化（不含新增未跟踪文件），不能用该数量代表全部任务产物。完整清单与逐文件哈希：

- [应用状态清单](../../target/infrastructure-remediation/handoff/application-status.txt)
- [框架状态清单](../../target/infrastructure-remediation/handoff/framework-status.txt)
- [暂停源码快照](../../target/infrastructure-remediation/handoff/source-paused.json)：681 个应用文件、398 个框架文件；不含文档和忽略文件。

框架中的 `.edit.py`、`edit_helper.py`、`__pycache__/`、`docs/superpowers/plans/2026-10-08-navigation-catalog-single-source.pre-navigation-merge.md` 是既有无关内容，必须保留。不要 `git add -A`、清理未跟踪内容、reset 或回退共享树。

阅读顺序：

| 资料 | 用途 |
|---|---|
| [全基础设施审核总入口](2026-10-25-all-infrastructure-adversarial-review.md) | 全部原结论的复审结果、反证、补漏与证据边界 |
| [原网络审核及路线图](2026-10-25-network-infrastructure-audit-adversarial-review-and-roadmap.md) | 原始输入；不能直接按其中已被推翻的修法执行 |
| [边缘与交付附录](2026-10-25-infrastructure-review-edge-delivery.md) | D1、D2、E01–E09 |
| [数据与身份附录](2026-10-25-infrastructure-review-data-identity.md) | D5、相关 D6、DI-01–DI-08 |
| [飞书与出站附录](2026-10-25-infrastructure-review-feishu-outbound.md) | D3、D4、N1–N8 |
| [运行期与前端附录](2026-10-25-infrastructure-review-runtime-frontend.md) | D7、D8、R-01–R-05 |
| [15 个工作包实施计划](../superpowers/plans/2026-10-25-infrastructure-remediation.md) | 68 D / 36 S / 新发现覆盖表；未验项仍开放 |
| [原始全量测试记录](2026-10-25-full-test-results.md) | 修复前的失败和历史基线 |
| [修复分支旧快照全量记录](2026-10-25-remediation-full-test-results.md) | 已完成的全量检查点及原始日志 |

审核文档中“没有实施修复”描述的是审核阶段，不应被解释为当前工作树没有修复。当前进度以本交接、专项修复报告和对应日志为准。

## 3. 已完成的全量检查点与最新证据

### 3.1 旧稳定快照：全量通过

权威记录为 [修复分支全量测试](2026-10-25-remediation-full-test-results.md)，源码对应 `target/infrastructure-remediation/verification/source-after-msrv.json`。

| 检查 | 实际结果 |
|---|---|
| 应用 `python scripts/run_ci.py full` | 退出 0；Rust 全目标 1,282 passed；架构、格式、严格 Clippy、生产依赖审计、前端完整检查通过 |
| 应用 `python scripts/run_ci.py integration` | 退出 0；30 组、159 项通过，无零匹配组 |
| 前端单测 | 80 文件、938 项通过 |
| Chromium | 开发 21、生产 4、单独视觉 2，共 27 项通过；视觉补跑禁止重录 |
| 应用 Rust 去重执行数 | 1,441 项，即 1,282 + 159；不叠加重跑次数 |
| 框架 `yang-base` 全目标全特性 | 874 passed、52 ignored；严格 Clippy 通过 |
| 冷缓存 Rust 1.80.1 | 最终旧快照 `cargo check --all-targets --locked` 退出 0，实际打印 1.80.1 |
| Nginx / Prometheus | 固定摘要镜像语法与运行时验证通过；19 条规则及告警演练通过 |
| 部署 / TLS / CMS | 当时 25 行为、10 变异；TLS 模板语法和一次性 CMS 加解密通过 |

旧全量检查仍有边界：两项真实飞书/现场回执库测试未执行，三个所需回执文件不存在；框架 52 个 ignored 未补跑；没有整个 `lib_yang` workspace 全量验证、生产验证、真实飞书外发、Firefox/WebKit、实际灾备恢复或完整容量证明。

### 3.2 后续新增改动：专项结果

| 范围 | 当前证据 | 尚缺什么 |
|---|---|---|
| 身份会话 | 真实 MySQL/Redis 7 passed、0 failed、0 ignored；[最终日志](../../target/infrastructure-remediation/identity/green-durable-sessions-final.log) | 8 项摘除验证、恢复绿测、最终总门禁 |
| XLSX | 49 单测、19 真实集成；9/9 摘除验证实际断言失败且源码已恢复；[报告](remediation-xlsx-2026-10-25.md) | 当前整合快照总门禁；真实大文件导入/RSS、跨进程同源互斥未证明 |
| 前端 `review` 状态 | 两个文件 16 项通过；[红测](../../target/infrastructure-remediation/review-label-red.log)、[绿测](../../target/infrastructure-remediation/review-label-green.log) | 最新前端全量和浏览器验证 |
| 部署最新批次 | 32 行为测试、16 变异、语法通过；日志前缀 `verification/deploy-smoke-*` | 真实镜像/现场部署、schema 兼容证明 |
| 生命周期 / cleanup | 修复前取消任务分离、单轮删 1,001 行的红测有效；真实 scratch smoke 已绿 | 最终实现的 runtime/session 绿测与 6 项摘除验证尚未执行 |
| 审批认领第一段 | 双独立池竞争与旧 owner 拒写真实回归 1 passed | 最新单条 durable admission 批次的用例与变异尚未执行 |
| P03 配置 | [最后红测](../../target/infrastructure-remediation/config-red.log)：79 项中 74 通过、5 失败，退出 101 | 最小生产修正、文档、绿测及摘除验证 |

最后配置命令成功编译了当前应用库和库测试，因此最新 P08 库代码具有这一次编译证据；其新增场景并未被运行，修改后的独立集成测试文件也不能据此视为已编译/通过。所有专项数量可能与旧全量有重叠，不能直接相加成新的“全量通过数”。

## 4. 15 个工作包状态

| 包 | 已落盘/已有证据 | 继续工作与关闭条件 |
|---|---|---|
| P01 授权 DI-01 | 历史 Outbox 只失效缓存；事实快照 TTL 从读前计时，延迟回填不能续期；改动在 authorization 三文件 | 对照真实乱序、TTL 后回灌、ack 失败、暂停后回填验收逐项核对，不能仅凭旧现有用例关闭 |
| P02 会话 DI-02/03 | Access 查询持久会话；Refresh 校验旧 JTI 并 CAS；7 项真实回归通过 | 跑 8 变异；TOTP 丢失与 MySQL 恢复回滚仍开放 |
| P03 MFA/密钥 | 六项配置回归已新增，当前 5 项失败；生产逻辑未修 | digits 仅 6、密钥域隔离、4 个 secret 绑定、DI-05 错误恢复说明；随后红绿/变异 |
| P04 数据依赖 | DI-07 取消安全已有只读最小方案，未实施 | Redis 完整借用/命令预算、MySQL 会话/DDL 超时、TLS、日志脱敏、真实池指标与取消安全均待推进 |
| P05 入站 | 同一 ServiceBuilder 的共享限流/卸载、错误信封、request ID、安全日志、全入站指标、16/17 MiB 合同已实现并专项验证 | 最终整合复验；真实代理拓扑/容量另验，详见[入站记录](remediation-inbound-2026-10-25.md) |
| P06 部署恢复 | 发布锁、镜像/config 身份、curl/停止失败处理、配置 ACL 预检、备份、TLS 模板；最新 32/16 通过 | 锁内 schema 策略/旧写者兼容、真实恢复、CI 制品绑定和现场网络仍未闭环，见[部署记录](remediation-deploy-2026-10-25.md) |
| P07 飞书基础 | N1 行锁、N2 后续终态事件、N7 脱敏、N8 TTL 尾段已实现，真实依赖及变异通过 | OAuth 刷新远端成功而响应/落库丢失 D4-4 仍开放；历史泄露数据清理未执行 |
| P08 审批执行/通知 | 认领 owner/generation/lease；单条持久受理和首次快照；未知写入 review；扩展回填错误传播等已落盘 | 最新 5 真库场景及现有合同需跑；批量持久队列、事件共享写权、只读核查、独立通知 outbox 尚未完成 |
| P09 出站/XLSX | N5 每次实际审批创建尝试共享限流通过；N6 XLSX 49/19/9 通过 | 对照 D3 逐项核对完整出站时限/错误分类，不能以 N5/N6 代替 D3 全部验收 |
| P10 生命周期 | OwnedTask、并行通知/共同截止时间、收尾预算、先 bind、只读 smoke、cleanup 分批已落盘 | runtime harness 最终绿测与 6 变异；运行期 panic/stall 监督仍不具备 |
| P11 可观测 | 入站指标/日志已验；管理面接入关闭所有权 | ready 新鲜度、挂起/目标缺席、依赖服务器采集、告警接收闭环仍待做；promtool 绿不等于通知送达 |
| P12 前端 | 会话代次、锁前预算、请求取消/超时、401 续期、错误元数据、恢复 UI、Query 重试、gzip、视觉/关闭按钮已实现并专项验证 | 最新整合复验；真实多标签调度、Firefox/WebKit、生产未验，见[前端记录](remediation-frontend-2026-10-25.md) |
| P13 凭据出口/恢复 | 重置页兼容 fragment/query 并清 URL，边缘安全日志已实现 | 后端重置邮件/管理链接生成者需继续核对；OAuth state 跨实例、原始请求日志、静态加密/密钥版本尚未收口 |
| P14 测试/供应链 | runner UTF-8、官方审计 registry、ignored 双向登记、部署回归门禁、旧全量证据已交付 | 刚登记 5 个审批 ignored，需再自检；最终 OpenAPI/full/integration/视觉/MSRV/受影响框架门禁；制品证明仍待 |
| P15 现场 | TLS、备份/恢复模板和场景清单已准备，历史 S01 记录保留 | 拓扑、容量、可信证书、真实备份恢复、保留期/告警消费者等必须现场确认；没有执行生产写入/部署 |

各包只是进度，不表示其关联全部 D/S 项已关闭。计划中的未验条目必须按实际证据更新，禁止批量改成完成。

## 5. 在途实现的具体交接

### 5.1 身份会话：代码已改，7 项绿，8 变异待跑

主要触点：

- `src/infrastructure/authorization/ports.rs`：required `is_session_active` 端口；4 个测试 fake 已同步。
- `src/addon/account/domain/authz_version.rs`：按用户、session_id、未撤销查询 MySQL。
- `authorization/request_validator.rs`：授权缓存之前校验会话；5 秒 deadline；依赖故障 503，失效会话拒绝。
- `account/domain/context.rs`：Refresh 快照加入用户/会话/当前 JTI 校验，无会话不再兼容放行。
- `account/domain/session/repository.rs`：`touch_on_refresh` 改为 `rotate_refresh`；更新谓词含用户、会话、旧 JTI、未撤销，影响行数必须 1，last_seen 仍 60 秒节流。
- `account/user/actions/refresh.rs`：返回 Token/Cookie 前必须持久化轮换成功，移除失败仅 warn 后仍成功的出口。
- 两个飞书集成测试的 `seed_operator` 已补 session_id 和真实 `user_session` 行，不要覆盖掉这些辅助修正。

7 项真实回归包括历史 Access、空 Redis 后旧 Refresh、无会话/错属、行锁屏障下撤销/推进 JTI/删除、连接池耗尽/查询错误，以及另一设备正常。行锁屏障最终使用 `performance_schema.data_lock_waits` + `data_locks`；旧 `innodb_trx` join 不能稳定观察等待者，失败日志仅用于诊断。

待执行脚本：[run-session-mutations.py](../../target/infrastructure-remediation/identity/run-session-mutations.py)。8 项为 Access 会话守卫、用户归属、CAS 撤销谓词、CAS 旧 JTI、影响行数、提交错误传播、查询 deadline、依赖错误分类。每项按原始字节恢复，要求真正断言失败，编译失败不算。

兼容性：历史无会话或 `refresh_jti = NULL` 凭据须重登；每个受保护请求增加 MySQL 查询，旧容量基线失效；混跑旧实例不具备完整保证。MySQL 提交结果未知可能要求重登。框架签发 tracing 可能早于应用 CAS 成功，不能据此推断客户端收到令牌。详见 [身份专项](remediation-identity-2026-10-25.md)和 [会话契约](../contracts/SESSION_AUTHORITY.md)。

### 5.2 P03：当前故意保留的失败回归

仅修改 `src/config/mod.rs`、`src/config/source.rs` 的测试，以及 `#[cfg(test)]` SecretKey 枚举/映射；尚未改 P03 生产逻辑、示例或文档。

实际失败的 5 项：

- `totp_configuration_only_accepts_the_supported_six_digits`：7 位仍被接受。
- `totp_key_is_isolated_from_all_authentication_secret_domains`：TOTP 仍可复用 Token active 密钥。
- `totp_and_email_secret_bindings_override_nested_file_values`：secret 未覆盖文件值。
- `totp_and_email_secrets_create_optional_sections_with_safe_defaults`：secret 未启用可选段。
- `totp_and_email_secret_files_reject_invalid_contents_without_echoing_them`：4 个未绑定 secret 文件被忽略，非法内容未被拒绝。

第 6 项验证未登记环境变量继续拒绝且不泄露，已通过。其他已有配置测试亦通过。

最小生产修正已经决定：digits 仅允许 6；复用 `validate_verification_secret` 比较 Token/Step-up 的 active + retiring 及注册/换绑/MFA/登录邮箱域；增加 `security_totp_aead_key`、`email_change_secret`、`email_mfa_secret`、`email_login_secret` 四条 secret 绑定。不为此扩大环境变量白名单、不新增 keyring 框架或 UI。

文档同步：TOTP 密钥实际为配置原文 UTF-8 的 SHA-256，不能写成截前 32 字节；直接更换 AEAD key 会使已有密文无法解密，不能声称用户下次登录可自行 setup；旧密钥须独立受控备份，当前没有无缝轮换工具。移除将已退役 Step-up 流程写成当前应用已装配的错误承诺。

### 5.3 XLSX 与前端追加：本地专项已收尾

XLSX 新增 `src/addon/feishu/domain/xlsx_job.rs`，并同步 parser、context、module、两个 Action、进度所有权、夹具生成器和三个资源夹具。边界为整批 ZIP 声明 256 MiB、实际保留文本 256 MiB、4,194,304 格、1,048,576 行；表头探测触及文本及选中列单格最多 32,767 UTF-16 单元。共享字符串先计费再复制，跨文件不重置；未选列数据不做该单格长度检查，仍计遍历格数和行数。

probe 移至 blocking；一个 Context 共享 1 个解析名额。permit 和 ImportGuard 由实际工作持有，取消只发协作信号，blocking 真正结束前不释放；成功后继续持有至异步写库结束。calamine 内部一次 eager sharedStrings 加载不能中途协作取消，这个边界已写入报告。没有宣称限额等于 RSS 或具备跨实例同源互斥。

9 项变异已完成，**不要无理由再重跑**。脚本 `target/infrastructure-remediation/xlsx/run-mutations.py` 和结果保留；后续最终总门禁正常包含这些测试。真实银行文件只读核对规模约 154,388 行、1,235,104 格、约 77.1 MB ZIP 声明展开，不是成功导入或内存压测证据。

前端只同步 `review` 为“待人工核查”、未知状态原值回退、类型注释和夹具；没有新增状态筛选或自动重试入口。原任务下钻没有状态筛选，派发记录页已有 Base Token、table_id 和结果筛选；不能以本次修复为由扩产品功能。

### 5.4 生命周期/部署：实现稳定，最终 Rust 验证待执行

- 框架 `yang_runtime::shutdown::OwnedTask<T>` 保留 JoinHandle，`join(&mut self)` 借用等待，Drop abort，避免 take 后外层取消使任务分离。接入七类 worker 与 MetricsServer。
- HTTP、worker、管理面同时收到停止信号，共同绝对截止时间；总预算 10%（最多 1 秒）留给 Tools/trace 收尾。Tokio abort 仍是协作取消，不能据此证明已运行 blocking 或不 yield 代码被杀死。
- axum 新增 `serve_listener_with_shutdown`，原入口委托；业务端口 bind 在 worker 启动和 mark_ready 前。
- `--smoke-check` 运行完整只读 schema/旧数据/依赖预检，不 apply、不监听、不启 worker、不初始化 telemetry 出站；部署检查固定镜像、参数、退出码和版本化 JSON 的必需字段（interface/mode/status 与非负整数 schema_changes，允许额外字段），前端单独静态冒烟。它不是 schema 锁内执行证明。
- 会话清理最多取 1,000 候选，DELETE 再核对保留期；满批后 100 ms 续扫，否则 6 小时间隔。刷新后的候选不能被误删。该生产修正已落盘，尚未跑最终绿测。

待执行：[run_runtime_mutations.py](../../target/infrastructure-remediation/run_runtime_mutations.py)。顺序为 runtime/batch/smoke 基线绿 → 6 摘除 → 恢复后 runtime、session unit+ignored、smoke、CLI 解析绿。6 项覆盖 abort、收尾预算、batch limit、smoke 分支、刷新复核、积压短间隔。临时修改框架 `shutdown.rs`、应用 `bootstrap.rs`、`session_cleanup.rs`，finally 按原始字节恢复；执行期间三文件及共享 Cargo 必须独占。

真实 cleanup 测试会重建并最终 DROP `user_session`，只能用专用测试库；不能与身份、审批测试并行。真实 scratch smoke 已证明预占业务端口仍可运行且随机 `*_test` 库保持 0 表，scratch 已清理。该测试直接调用 `run_after_tools_created`，构建完整应用/schema，但手工提供 MySQL/token，跳过配置加载、公共资源创建和 Redis 连接；只能证明实际 bootstrap 下层 smoke 分支，不能当成真实 CLI 进程或镜像部署的证明。

部署最新证据：`target/infrastructure-remediation/verification/deploy-smoke-safety-tests.log`、`deploy-smoke-safety-mutations.log`、`deploy-smoke-syntax.log`。32/16 是命令替身和本机 shell 证据，不是线上验证。详见 [运行期报告](remediation-runtime-2026-10-25.md)。

### 5.5 P08 审批：第一段已验，第二段刚落盘

新增 `src/addon/feishu/domain/approval_execution/{mod,repository,transport}.rs`，任务表增加 generation/effect_phase/review；声明式 schema，无 SQL migration。

第一段已验证：两个独立 MySQL 池争同一任务，只有一个获得执行权；旧 owner 无法提交覆盖新 owner。旧实现红测得到重复执行与旧结果，新实现 1 项绿。owner + generation + 有效 lease + affected=1 保护结果写入，播种不重置已存在状态。

第二段已落盘并通过 scoped rustfmt、架构、diff 检查；库已由最后配置命令编译，但场景尚未执行：

- 单条请求先持久化首份配置/映射/身份快照，HTTP 与 worker 共享 claim；首次正常请求保留同步 200，已有任务不重新执行。不要改成无必要的全单条强制 202。
- 创建/回填前持久 effect phase，远端确认后尽快保存 instance code；未知外部写结果进入 `review`，停止自动重发。
- `code=0` 但 HTTP 非 2xx 不可确认成功；首份没有申请人也不能被后续请求补写覆盖。
- 扩展回填和单字段转换失败继续传播；事件本地同步落库失败不能写 Processed。
- 删除配置拒绝 pending/creating/created/review 等仍有活跃/未知效果的任务；集成断言已按保留证据的合同调整。

5 个新增 ignored 已登记到 `scripts/run_ci.py`，完整前缀均为 `addon::feishu::domain::approval_execution::tests::`：

```text
real_mysql_execution_admission_preserves_first_snapshot_and_identity
real_mysql_execution_transport_confirms_success_and_quarantines_unknown
real_mysql_execution_remote_success_local_failure_never_recreates
real_mysql_execution_rejects_lost_owner_generation_lease_and_phase
real_mysql_execution_waiting_and_retry_budget_are_durable
```

下一验证窗口还需跑既有 exclusive claim、相关事件、删除配置合同、两个新增扩展回填单测，再做针对真实行为不变量的摘除验证。存储+受控 transport 测试不替代完整 HTTP 派发集成；不要为冗余守卫制造镜像实现测试，也不把编译失败当有效变异。

**仍未完成**：批量请求持久分页展开队列（目前计划复用 request_log 保存进度）、事件与派发共享回写权、未知结果只读核查、独立 notification outbox、成功通知失败重扫及历史 HTTP sent 不抑制新结果。当前 `review` 只是安全停自动重放，不等于已提供完整恢复流程。OAuth D4-4 的 refresh 成功但新凭据确认丢失也未解决，应另包处理，不能靠 N1 行锁宣称关闭。详见 [飞书专项](remediation-feishu-2026-10-25.md)。

### 5.6 P04 / DI-07：仅有建议，未改代码

框架 `crates/yang-base/src/database/schema_sync/sync.rs::sync_table_definitions` 借到 PoolConnection 后，**在任何 SELECT/GET_LOCK await 前**调用锁定 sqlx 0.8.6 已提供的 `close_on_drop()`，是当前候选最小修法；保留正常 RELEASE_LOCK 和原有错误合并。不能只在收到 GET_LOCK 成功响应后才标记，否则获锁但响应丢失的取消窗口仍存在。

该 API 的 Drop 会取出连接关闭，不回池；优雅关闭最多后台等待约 5 秒，不是同步立即释放。成功同步也关闭这条单次启动连接。未引入自建 async Drop guard 或新的连接配置；只读 plan/preflight 无需跟着改。

建议真实红绿场景：一条控制连接持有同名 GET_LOCK，确认同步连接等待后取消，再释放控制锁，独立连接能限时得锁；另一场景在第一张表 DDL 已完成、第二张表 ALTER 等 MDL 时取消，检查锁最终释放且重跑收敛。不得断言取消能回滚已提交 DDL。摘除 `close_on_drop()` 应让锁可用性断言失败。**上述测试与修正尚未编写/执行。**

## 6. 恢复后的执行顺序

以下步骤仅在用户恢复修复目标后执行。不要在交接期间自动运行。

1. 核对两仓 branch/HEAD/status、暂存区、源码暂停快照及活跃进程；读 AGENTS 和各专项记录。若有其他会话新改动，先按路径协调，不能恢复旧文件覆盖新内容。
2. **先把 P03 从红转绿。** 做第 5.2 节的最小生产修正和文档同步；跑 `config::`，确认所有 79 项及新增影响通过；摘除 guard/secret 绑定确认回归变红，再恢复。不扩大到新 UI/keyring 功能。
3. 串行跑身份 8 项变异和 7 项恢复绿测，检查相关账号/授权回归；更新身份专项。日志缺失或“0 tests”不能计为通过。
4. 串行跑 runtime harness 的最终绿测/6 变异，更新运行期专项。
5. 给飞书独占窗口运行第 5.5 节新旧用例、修改后的派发集成与变异；失败先查根因，记录编译/夹具失败与业务断言失败的区别。
6. **固定一份稳定源码快照完成用户要求的新一轮全量检查点。** 暂不继续 batch/outbox、Redis 等新修改，所有代理冻结源码；统一重导 OpenAPI、runner 自检、应用 full/integration、视觉、受影响框架、冷缓存 MSRV，并核对前后哈希。发现失败后做范围内修正，再重新验证受影响阶段。只把这一检查点标为已验证，不关闭尚未实现的工作包。
7. 再按计划继续 P08 批量/事件/通知、P04 Redis/MySQL、DI-03 恢复安全、P03/P13 密钥与 OAuth、P11 监控及 P15 现场材料。每个独立包达到完整可编译点再交测试窗口，避免长期在移动源码上重编译。
8. 所有应实现项完成后，再做最终生成契约与全量门禁；逐项核对 68 D / 36 S / 30 新发现、修复前后证据和部署前置。需要现场授权的动作先把产物/范围/回滚准备好，作为最后一步提出，不能把本地通过写成生产完成。
9. 提交前检查两仓 `git diff --cached --name-only` 与 HEAD，只显式暂存本任务路径并做范围清楚的本地提交。push、merge、deploy、生产写入和真实飞书外发仍是不同动作，当前未执行。跨仓推送必须先 lib_yang 确认完成，再 yang-system，避免 CI 取到旧依赖。

## 7. 测试环境与命令

### 7.1 独立依赖

| 用途 | 容器 | loopback 端口 | 测试库 |
|---|---|---|---|
| 身份/统一本地集成 MySQL | `codex-identity-remediation-mysql-20261025` | 64533 → 3306 | `identity_remediation_test` |
| 身份/统一本地集成 Redis | `codex-identity-remediation-redis-20261025` | 64536 → 6379 | DB 15 |
| 飞书分工 MySQL | `codex-feishu-remediation-mysql-20261025` | 60761 → 3306 | `codex_feishu_remediation_test` |
| 飞书分工 Redis | `codex-feishu-remediation-redis-20261025` | 60764 → 6379 | DB 15 |

这些是任务专用临时容器；本次交接没有停止或删除。不要对其他日常容器执行清理。MySQL 测试会重建业务测试表，数据库名必须以 `_test` 结尾，Redis 必须为 DB 15；所有 Cargo 和会修改这些测试表的测试都串行。

在应用目录的 PowerShell 中配置身份测试环境（从专用容器元数据读取测试密码，不输出或写入文档；不要用于生产）：

```powershell
$env:CARGO_BUILD_JOBS = '1'
$env:PYTHONUTF8 = '1'
$env:PYTHONIOENCODING = 'utf-8'
$env:NO_PROXY = '127.0.0.1,localhost'
$env:no_proxy = '127.0.0.1,localhost'
$identityTestMeta = docker inspect codex-identity-remediation-mysql-20261025 | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw '专用测试 MySQL 不存在，请重新核实环境' }
$identityTestPasswordLine = @($identityTestMeta[0].Config.Env |
    Where-Object { $_.StartsWith('MYSQL_ROOT_PASSWORD=') })
if ($identityTestPasswordLine.Count -ne 1) { throw '专用测试密码来源不唯一' }
$identityTestPassword = $identityTestPasswordLine[0].Split('=', 2)[1]
$env:YANG_SYSTEM_TEST_DATABASE_URL = 'mysql://root:' +
    [Uri]::EscapeDataString($identityTestPassword) + '@127.0.0.1:64533/identity_remediation_test'
$env:YANG_SYSTEM_TEST_REDIS_URL = 'redis://127.0.0.1:64536/15'
Remove-Variable identityTestMeta, identityTestPasswordLine, identityTestPassword
```

### 7.2 当前待跑的定向命令

```powershell
# P03 生产修正后；现在直接跑会再次出现已知的 5 项失败。
cargo test --lib config:: --locked -j 1

# 身份 mutation 会临时改生产文件；先确保独占和源码已保存。
python target/infrastructure-remediation/identity/run-session-mutations.py
cargo test --test session_revocation_integration --locked -j 1 -- --ignored --test-threads=1

# 生命周期/cleanup/smoke 绿测、6 变异和恢复验证。
python target/infrastructure-remediation/run_runtime_mutations.py

# P08 五项；完整前缀避免误匹配真实飞书外发测试。
cargo test --lib addon::feishu::domain::approval_execution::tests::real_mysql_execution_ --locked -j 1 -- --ignored --test-threads=1
cargo test --lib infrastructure::feishu_approval_worker::tests::real_mysql_worker_claims_are_exclusive_and_old_owner_cannot_commit --locked -j 1 -- --ignored --exact --test-threads=1
cargo test --test feishu_approval_dispatch_integration --locked -j 1 -- --ignored --test-threads=1
```

以上是可依次执行的命令清单；实际执行须逐条检查退出码和通过数，任何失败先停在该步骤，不能把后续命令的退出 0 当作前面成功。相关事件与扩展回填单测按最新源码精确选择，不能用宽泛 `real_` 过滤跑全部 ignored。

### 7.3 新稳定快照全量检查点

```powershell
python scripts/run_ci.py --self-test
python scripts/dump_openapi.py
python scripts/run_ci.py full
python scripts/run_ci.py integration
cargo test --doc --locked -j 1
```

仍须逐条保留日志/退出码/测试计数。runner 最新新增了 cleanup batch、只读 smoke、claim 和 5 个 execution ignored；最后 5 个登记后的自检尚未执行。OpenAPI JSON 与 API types 均禁止手改；最新任务状态/schema 可能影响生成结果，统一重导后审查差异。

额外检查不能被上述命令遗漏：

- 视觉用独立本地端口与 `--update-snapshots=none` 补跑；CI 默认跳过的 2 项不能记为已执行。沿用 `frontend/e2e/visual.spec.ts` 和已有 Playwright 配置，不把过期的演示二进制当最新后端证据。
- 受影响框架：`cargo test -p yang-base --all-targets --all-features --locked -j 1`、同范围严格 Clippy；新增 yang-runtime 生命周期变更还需该包相应测试/Clippy。所有 Cargo 仍然串行。
- 固定摘要 Nginx 语法/运行时、Prometheus 规则/演练、部署安全测试与变异。旧通过且源码无变化可按证据复用范围明确记录；有新修改则复验对应资产。
- Rust 1.80.1 冷缓存 all-target check。只读挂载正确的当前两仓源码；显式 `RUSTUP_TOOLCHAIN=1.80.1`、独立 `CARGO_HOME`/`CARGO_TARGET_DIR`、`CARGO_BUILD_JOBS=1`，检查实际版本与退出码。在 worktree 中不能挂错主检出，`crates/` 若为链接须另外挂真实目录；Git Bash 必须处理 MSYS 路径转换，详见 AGENTS。
- 测试开始/结束分别运行既有 `verification/snapshot-sources.py`，逐文件比较；文档不计入该源码哈希。不能在其他代理写代码时声称验证了一个固定快照。

## 8. 不能重踩的结论与边界

- 原本就有队列等待超时和 404/405 Trace，真正缺口是快速卸载、共享容量、信封/ID、非 Action 指标；不要恢复已被实验推翻的根因。
- LoadShed 和共享限流必须在同一 ServiceBuilder；分开 Router layer 会改变排队行为。
- Redis 不可达时认证失败关闭；可达但空状态是不同问题。不要做进程正缓存绕过撤销，也不要把 Redis 写成可任意清空的纯缓存。
- 当前应用没有装配 Step-up；不能把框架测试当现网保护，也不能擅自恢复已经移除的产品流程。
- grant-expiry 双实例必然重复 bump 已反证；保留现有行锁/CAS，不重复重写。
- 单文件 16 MiB、HTTP/multipart 整体 17 MiB、JSON 16 MiB 是不同限额；合法文件应留封装空间。
- timeout/取消不是副作用回滚；未知外部效果不能当确定失败自动重试。`review` 需要后续核查机制，不是完整恢复闭环。
- Drop abort 若句柄已被 take 仍无效；当前 OwnedTask 保留所有权。它不证明运行期 panic/stall 被监督。
- 配置权限必须核对容器 UID/ACL；盲目 chmod600 可能断服务。只读 smoke 不证明 schema 兼容，人工 rollback 声明也不是锁内证明。
- TLS 是否可用取决于实际网关拓扑、证书信任和端点；不能泛化成“无域名/443 就不能 TLS”。gateway 容器的 loopback 不是宿主 loopback。
- 不用生产 ResetDb/FLUSHALL、真实飞书外发或伪造回执补测试。两项现场测试保持显式未验。
- 前次并行 Cargo 曾触发 Windows `os error 1455` / `LNK1102`。不同 target 目录也不隔离提交内存；坚持串行 + `-j 1`，不要为过门禁清缓存或修改系统分页文件。
- 变异必须捕获行为断言失败；编译失败、0 tests、环境初始化失败都不是有效红测。脚本 finally 只能覆盖正常异常路径，中断后仍须手工核对被变异文件是否恢复。

## 9. 证据归档与最终交付要求

主要本机日志目录：`target/infrastructure-remediation/{verification,identity,xlsx,handoff}`；入站变异另在框架 `target/inbound-remediation/mutation-logs/`；前端变异在 `frontend/test-results/remediation-mutations/`。这些大多被 Git 忽略，交接到其他机器前须安全归档必要日志与哈希，单独复制 Markdown 不会带走原始证据。不得归档真实配置/凭据。

当前交付仅包括本地源码、测试和文档。没有新的 scoped local commit，没有 push/merge/deploy，没有生产 SSH/数据库写入、真实飞书消息、证书申请或灾备恢复。四个专用容器和测试证据保留，未为交接执行破坏性清理。

恢复后应以“当前失败 → 最小修复 → 场景回归 → 摘除验证 → 固定快照总门禁 → 逐项关闭”推进。一次绿色检查点只能证明该快照的已执行范围；全部审核修复和现场验收仍需独立完成。

# 参考项目（scs）与本项目（yang-system）架构对比分析报告

| 项目 | 内容 |
|---|---|
| 分析日期 | 2026-11-08 |
| 仓库 | `project/yang-system`（`main`） |
| 对比对象 | `scs/scs-api`（Rust，2,108 `.rs`）+ `scs/scs-web`（Quasar/Vue3，416 `.vue`） |
| 分析范围 | 8 个横切维度：框架与扩展模型、模块解剖、数据层、身份与权限、API 契约、前端架构、运行时与运维、可复用业务机制 |
| 分析方式 | 每个维度对两侧并行测绘 → 强模型差异分析 → 事实核对 + 价值/原则双镜头对抗校验 → 完整性批评 → 综合 |
| 结果 | 46 条候选建议：24 条通过双镜头校验，22 条淘汰（第 5.2 节逐条给出淘汰证据） |
| 代码改动 | 无。分析全程只读；本文件为报告落盘，是唯一新增产物 |
| 证据等级 | 全部为静态阅读/grep/文档，未跑构建、测试或生产库；待核实项集中列在第 9 节 |

> 安全边界：分析未读取 `deploy/config.cloud.toml`、`~/.claude/settings.json` 或参考项目的任何凭据值；报告中不复述密码/Token/密钥原文。

> 使用说明：本报告是**参考项目对比**，不是缺陷清单。第 3.4 节把差异分为「刻意取舍 / 真实缺口 / 不适用」三类，落地前先按第 9 节做一次实证。

## 目录

- [0. 阅读说明](#0-阅读说明)
- [1. 结论摘要](#1-结论摘要)
- [2. 两个系统的本质定位](#2-两个系统的本质定位)
- [3. 架构差异矩阵](#3-架构差异矩阵)
- [4. 值得吸收的机制](#4-值得吸收的机制)
- [5. 不建议照搬的模式](#5-不建议照搬的模式)
- [6. 本项目已对齐或优于参考项目之处](#6-本项目已对齐或优于参考项目之处)
- [7. 维度之外的高优先级未覆盖面](#7-本次对比的边界8-个维度之外的高优先级未覆盖面)
- [8. 路线图](#8-路线图)
- [9. 验证待办](#9-验证待办本次未验证的事实)

## 0. 阅读说明

**证据等级标记**：每条结论后标注 `[代码]`（逐行静态阅读或 grep 实测）、`[文档]`（契约/审计文档记载）、`[待核实]`（未跑构建、未连生产库、未实跑测试的推断）。多处结论基于静态阅读，凡未验证项已集中到第 9 节，不进结论区。

**差异归类口径**：本节所有"参考有、我们没有"的差异必须先落入三类之一，否则不计入改进项——
- **A 刻意取舍**：有文档或测试锁定，照做是回归。
- **B 真实缺口**：有可复现失效路径或已发生漂移。
- **C 不适用**：参考项目的业务域本项目不需要，或规模前提不成立。

**事实基线**：参考项目 scs-api 实测 2,108 个 `.rs`、约 156,633 行、30 addon、217 个 fields() 模块、1,842 个 Action、416 个 `.vue`；本项目 src 249 个 `.rs`、108,020 行、3 个 AddonSpec、18 个 ModuleSpec、100 个 Action；飞书 addon 131 文件 67,078 行，占 src 62% `[代码]`。

---

## 1. 结论摘要

1. **本项目缺的不是架构能力，而是"契约与运行期一致性"的机械闭环。** 经事实核对与价值核对后存活的所有条目，几乎都收敛到同一形态：机制已存在、缺一条断言或一处收口。Step-up 文档漂移、OpenAPI 快照过期、双层信封、权限分类缺反向断言、worker 监督盲区，全部属于"能写对但没人强制写对"，而非"不会写"。
2. **把"参考项目有而我们没有"当作缺陷是系统性误判。** 30 addon / 1,842 Action 与本项目 3 addon / 100 Action 不是同一类规模竞争：参考项目用运行时字符串注册表吸收广度，代价是 12 个 `.rs` 从不参与编译、5 条悬空 `api_run`、19 个 MENU 无页面；本项目用构建期冻结 Catalog 换深度，代价是改契约必须重新部署。
3. **真正值得移植的只有四条低成本补强**：运行期 worker 监督扩面、engine 边界 ESLint、管理员等价权限分类断言、契约快照新鲜度。它们的共同点是"改动不超过 20 行、复用已有机制、能指向具体失效路径"，其余 22 条候选建议已被淘汰（第 5 节）。
4. **本次 8 个维度之外存在更高优先级的未覆盖面**：明文边缘（已知 P0）、无 cargo-deny、告警规则无采集端、无 DR 演练。这些已写进本项目自己的审计文档，属于"待办可见"而非"新发现"，但严重度高于任何新增架构改进，报告不把它们重复计为新缺口，也不让它们被 8 个维度遮住。
5. **规模是双向的债。** 本项目行数已达参考项目 69%，单文件密度高 8.5 倍，风险从"文件多而浅"换成"文件少而深"——9,081 行的 `feishu_approval_worker.rs`（其中 46% 是文件内单测）就是这一形态。这不支持"我们的架构更简单"，只支持"治理手法该针对深度而非广度"。

---

## 2. 两个系统的本质定位

**参考项目**是供应链 ERP 套件。它的扩展模型要解决"大量业务域如何在运行期被组织、裁剪与后台管理"：`Plugin::addon → Addon::module → Module::action` 三级字符串 match（`scs-api/src/service.rs:57-96`），启动扫描 `src/addon` 生成 `api.json` 再写入 `api.api` 表（`src/main.rs:57-96`、`src/addon/mod.rs:131-190`），前端用 `import.meta.glob` 按 `pages/{addon}/{module}/{action}.vue` 动态 `addRoute`（`scs-web/src/router/index.js:30-54`）。它换来的是"后台可增删接口、新增菜单近零前端改动"，付出的是维护债：12 个 `.rs` 从不参与编译、2 个 Module 未挂进 `Addon::module`、至少 5 条悬空 `api_run`、44 处 `api_run` 后直接 `unwrap`。

**本项目**是身份/权限 + 飞书集成控制台。它的扩展模型要解决"少量高治理要求的域如何被约束"：`AppBuilder::build` 在构建期做唯一性/依赖/模块归属/内容/中间件顺序/引用/路由七类交叉校验后冻结为不可变 Catalog（`crates/yang-base/src/definition/builder/app.rs:131-174`），请求期只做 `BTreeMap<ActionRef, ActionHandle>` 预解析查找。

**为什么不能按模块多少论优劣**：两者的失效模式与边际成本曲线不同。参考项目的边际成本随广度线性增长（多一个业务域就多一份手写 match 与模板复制），失效形态是"漏一行、编译器不管"；本项目的边际成本随深度增长（多一条业务分支就压进同一批大文件），失效形态是"形状对、语义错、门禁看不出"。因此参考项目需要的是注册表与启动期校验，本项目需要的是契约断言与层边界——照搬任何一方都会把另一方的债引入。

**本项目当前边界**（写死，避免后续讨论漂移）：单租户、单实例部署（`src/infrastructure/feishu_pull.rs:6-12`、`src/addon/feishu/domain/import_progress.rs:15-16`）、5 个手写生产控制台入口、通用前端引擎仅服务 demo 与 DEV workbench、无行级/资源级授权、无多实例协调。这些是已文档化的取舍，不是待补齐的空白。

---

## 3. 架构差异矩阵

| 维度 | 参考项目做法 | 本项目做法 | 第一性判断 |
|---|---|---|---|
| **框架与扩展模型** | 请求期字符串三级 match，API 清单由启动扫描写表，后台可增删接口（`scs-api/src/service.rs:57-96`）；33 个 Cargo feature 当 addon 边界，14 组跨 addon 调用的闭包不含被调方，裁剪后运行期静默降级为 Invalid API（`scs-api/Cargo.toml:5-96`） | 构建期七类校验 + 冻结 Catalog，请求期 BTreeMap 查找（`crates/yang-base/src/definition/builder/app.rs:131-174`）；`cfg(feature)` 在 src 内零消费，`AddonSpec::depends_on` 已实现但项目零调用（`spec.rs:816`） | **本项目更合理**：失效前移到构建期。但两处残余：addon 漏挂载无声（校验只覆盖已传入的 addon）；`UiCatalogAction` 挂在业务模块 `account.user`（`src/addon/account/user/mod.rs:70`）而前端启动必经。参考项目的规模债不能当能力学 |
| **模块解剖与约定** | Action 自带 UI 元数据，`table.rs` 现场拼 columns/btn/cnd，前端 20 行薄壳（`scs-api/src/addon/goods/goods/table.rs:39-65`）；一文件一 Action 平铺，无宏无注册表；超大模块用文件名后缀 + feature 分叉 | register 链只声明路由/展示/权限/成功码，`params!/fields!` 生成强类型 Input 与 JSON Schema；一 Action 一文件 + `ACTIONS` + `new_action.py` 脚手架 + 15 项形状门禁；无任何体量上限，9,081/5,424/3,982 行文件全部通过 | **本项目契约层更强，但"只管形状不管体量"是真实缺口**。修法不是加行数门禁（46% 是测试，scs 最大 `.rs` 也只 790 行），而是拆有资源缝的 `api.ts` 审批段（约 719 行） |
| **数据模型与持久化** | 启动期 auto-DDL，列 COMMENT 即指纹，DB 存在而 fields() 已无的列直接 DROP COLUMN（`[待核实]` 来自 docs.rs 测绘，本仓无可验证源码）；持久化内联 Action，`update(request.body)` 实测 ≥168 处 | GET_LOCK 串行 + 只读规划 + 旧数据预检 + 只增不删（`crates/yang-base/src/database/schema_sync/sync.rs:140-207`）；业务事实 + authz_version + Outbox + audit 同事务（`src/infrastructure/authorization/outbox.rs:113-180`） | **本项目正确性更强**，代价是删列/缩短字符串无自动路径，只能重建。可接受。但 raw-sql 门禁漏了 `yang-db` 的 `_with_params` 家族，而生产代码正在用（`src/infrastructure/schema.rs:54-61`），这是治理面唯一已确认的逃逸口 |
| **身份与权限** | 权限只到 4 个 model 标签，权限组里 API 列表塞 text 列，`LIKE '%x%'` 子串匹配；`interface_type==API` 直接放行（`scs-api/src/addon/admin/auth/verify.rs:63-79`）；`org_org` 由请求头带入 | 权限目录 15 键 + 构建期投影 AuthorizationPolicy，链尾统一 fail-closed（`crates/yang-base/src/definition/builder/registry.rs:307-329`）；authz_version + Outbox + Redis 5s TTL + 会话撤销 | **本项目明显更强**。缺口不在判定强度而在登记完整性：管理员等价清单是手工跨 addon 登记点（`src/addon/access/domain/sensitive_permissions.rs:50-71`），关键词启发式对 `*.write` 形态天然无效 |
| **API 契约** | OpenAPI 只描述 requestBody：1,127 path 全 POST、`required` 非空 0 个、responses 全空，且入库 swagger 与运行期已漂移（1,087 vs 1,127） | 91 个 operation 全量 input/output；但 73/91 的 `data` 被投影成 ApiResponse 双层（`crates/yang-base/src/definition/openapi.rs:168,283-293` 与 `action/response.rs:531-543` 的运行时单层透传不一致），8 个 GET 带 requestBody、tags 全空 | **参考是"信息缺失"，本项目是"信息错误"——后者更危险，但危害有界**：`api-types.ts` 全仓零 import，运行时本就单层。真正的缺口是快照新鲜度无门禁，且已发生 17 小时漂移（8531782 → a603f7a）`[代码]` |
| **前端与元数据驱动** | 416 `.vue`、204 个 ≤20 行薄壳、201 个 MENU action；按约定动态取组件，缺失静默空白（实测 19 例）；`package.json:12` 的 test 是 `echo` 空实现 | 23 个 ModuleSpec 中 5 个生产入口全手写，src 内 `ViewSpec::new` 声明数 0；zod + Ajv fail-closed；session 代次/单飞/跨标签收敛；engine 18,805 行的真实用户是 demo 与 DEV workbench | **本项目是"引擎服务开发工作台"而非生产基础设施**，这个定位必须写死。新增控制台边际成本是"后端 Module+Action + 手写页面 + 路由"三处，是刻意取舍。参考项目的动态解析换来 19 个空白路由，不能学 |
| **运行时与运维** | 同步多线程，61 处 `thread::spawn` 不存 JoinHandle（`scs-api/src/handle/mod.rs:22-53`）；无重试/死信/幂等，`handle_listen` 成功失败都 `remove_file`（at-most-once，`src/handle/handle_listen.rs:39-52`）；只在 env_logger，`RUST_LOG` 实际未解析（`src/main.rs:25-44`） | 单 tokio 进程 + 7 worker + watch + 首个退出事件后共享关闭预算（`src/bootstrap.rs:462-570`）；审批派发是阶段化持久状态机（`effect_phase`/`owner`/`lease_until`）；独立管理面 `/metrics` + 13 条告警 + promtool 正反演练 | **本项目全面领先**，但两处真实缺口：运行期监督只覆盖审批 worker（`src/bootstrap.rs:462-488`）；13 条告警规则无采集端（`docs/operations/2026-10-25-s01-production-facts.md:60,106` 确认现网无 Prometheus） |
| **领域机制** | 靠复制模板摊广度：27 份字典、17 份审核动作、5 个支付通道复制；用条件 UPDATE 做并发守卫（`logi/node_plan/put_review.rs:36`）；无不可变审计，按钮日志经 Redis pub/sub 尽力而为 | 纯函数边界（`xlsx.rs` 的 `sniff_is_zip/read_header`、`outbound.rs` 的 FailureKind/RetryPolicy）、租约+代次、事务内 append-only 审计；但三套持久化各自完整、7 个 worker 手写循环 | **本项目已越过"复制状态机"阶段，停在"三份相似实现 + 七个手写循环"**。按 YAGNI，第二消费者出现前不抽公共层；唯一该动的是让 `run_round_or_shutdown` 被更多循环复用（现仅 2/7 使用，`worker_shutdown.rs:20-29`） |

### 3.4 差异的三种归类（示例）

**A 刻意取舍**（有文档或测试锁定，不应改）：
- 单实例部署：`src/infrastructure/feishu_pull.rs:6-12`、`docs/architecture/feishu-option-ingest.md:52`、`import_progress.rs:15-16` 写下"多实例时换 Redis 分布式锁 + 心跳"的升级路径。
- schema 只增不删：`docs/contracts/SCHEMA.md:12-13`。
- 通用表格无生产负载：`src/app.rs:248-259` 断言控制台模块 views 为空且编译视图 `data_action` 全为 None，`docs/superpowers/specs/2026-10-08-navigation-catalog-single-source-design.md:209` 与 `plans/...pre-navigation-merge.md:591-596` 说明这是防内部表泄漏的有意闸门。
- `interaction` 拒绝降级：`frontend/tests/engine/contracts/ui-catalog.test.ts:179-208` 明文"禁止降级为 invoke 执行"。
- `access.grants.write` 可授非管理员等价权限：`docs/contracts/AUTHZ_GRANTS.md:67-70`；`tests/permission_groups_integration.rs:4682-4699` 用反向对照固定"拒绝面过宽同样是故障"。
- Step-up 未装配：`807dc91` 系统级移除，`docs/assessments/2026-10-20-full-project-review.md:107-135` 登记 P0-2"暂不修复"。

**B 真实缺口**（有可复现失效路径或已发生漂移）：Step-up 文档漂移、worker 监督盲区、raw-sql 逃逸口、双层信封、快照新鲜度、管理员等价分类缺失、engine 深路径、明文边缘 P0、无 cargo-deny、告警无采集端。

**C 不适用**：`org_org` 多租户模式、12 种 `btn_type` 字符串分派、运行时 addon 注册、计费金额信任客户端、416 页薄壳生成器。

---

## 4. 值得吸收的机制

以下条目均已通过事实核对与价值核对。凡事实有误、价值不成立或与既有决策冲突的，已移入第 5 节。每条附"参考项目证据"字段——参考项目**没有**该机制时明确写出并给反证，不编造。

### P0：立即可做（S 成本，均可独立提交）

#### P0-1 运行期 worker 监督从 1 个扩到 7 个

- **解决什么问题**：7 个 worker 中只有审批 worker 有退出检测。其余 6 个（Outbox、过期直授、会话清理、飞书拉取、事件、OAuth）panic 后进程仍 ready、`/metrics` 无 `worker_alive` 信号、告警不响，只有关闭阶段 `join` 才暴露。这是 2026-10-25 审计 D7-3 的未验收残留 `[代码]`。
- **参考项目证据**：无。`scs-api` 是同步多线程，61 处 `thread::spawn` 且不保存 JoinHandle（`scs-api/src/handle/mod.rs:22-53`），不存在 async worker 与监督机制；`Cargo.toml` 无 tokio 依赖。本条依据本项目自身审计，不是从参考项目学来。
- **本项目落点**：`src/bootstrap.rs:462-488`（5 秒 ticker，唯一条件 `approval_worker.has_exited()`）、`src/infrastructure/feishu_approval_worker.rs:302-305`（唯一 `has_exited` 实现）、`D:/code/lib_yang/crates/yang-runtime/src/shutdown.rs:39`（`OwnedTask::is_finished` 已暴露，注释明写"供运行期监督使用"）。
- **最小改法**：6 个 worker 各加一行 `pub(crate) fn has_exited(&self) -> bool { self.task.is_finished() }`；在现有 ticker 内构造内联 `[(name, exited); 6]` 数组，命中任一即走现有 `Err(anyhow!("{worker} 意外退出"))` 关闭路径。不新建 `Workers` 聚合结构体、不做静态 fn 表、不加 readiness 门控（guard future 随 `select!` 结束即丢弃，关停信号在其后发出，不存在误判窗口）、不含管理面 task（已由 `absent(yang_system_readiness_ready)` 覆盖）。
- **验收标准**：手工 `kill -9` 一个 worker 的 spawn 后 5 秒内进程非零退出、容器重启；`cargo test --lib` 全绿；在 D7-3 验收记录中标记完成。定位写清楚：只抓 panic/abort，不抓挂起（后者已有 `ProgressHeartbeat`）。

#### P0-2 engine 边界 ESLint 门禁

- **解决什么问题**：`frontend/AGENTS.md:46-48` 的强制规则 2 要求 features 走 `@/engine` 桶，但自认"深度路径的机器禁令是记录在案的后续增强，落地前靠评审执行"；`docs/architecture/frontend-rebuild/README.md:183` 同样登记为未做。一旦有人在非 lazy 页面照抄 `renderers/table/TableView`，TanStack Table + Ajv 会回到首屏。
- **参考项目证据**：反例。`scs-web/src/router/index.js:30-54` 与 `stores/base.js:122-135` 用 `import.meta.glob` 动态拼组件路径，完全没有边界概念，直接导致 19 个 MENU 无页面。不能学。
- **本项目落点**：`frontend/src/engine/index.ts:10-17`（renderers 故意不导出，注释记录放进桶会让首屏 219.9→272.8 kB gzip）、`frontend/src/shell/routes.tsx:29-36`（ModulePage/BusinessPage 路由级 lazy 是现有防线）、`frontend/scripts/verify-bundle-budget.mjs:14-15`（350 kB 目标 / 450 kB 硬限，272.8 kB 不会触发，是盲区）。
- **最小改法**：在 `frontend/eslint.config.js` 加一条 `no-restricted-imports` override：`src/features/**` 禁止 `@/engine/*`；`src/shell/pages/**` 单独放行 `@/engine/renderers/*`；其余 shell 文件按现有桶能力收敛。存量 features 深路径实测 25 处（**不是 47 处**——原数把 shell 的 22 处混算），先补齐 6 个缺失桶导出（`SESSION_REFRESHED_EVENT`、`publishSessionEnd`、`SecondFactorRequiredError`、`apiBase`、`parseJson` 等）后一次性清零，不需要 baseline 文件。规则随现有 `pnpm lint`（在 `pnpm check` 内）生效，不新增依赖。
- **验收标准**：新增一个 features 深路径 import 时 `pnpm lint` 失败；首屏 gzip ≤350 kB。范围要如实写成"features/**"，与 AGENTS.md 规则 2 一致，不要扩到全 shell。

#### P0-3 管理员等价权限从"关键词提示"升级为"分类完备"断言

- **解决什么问题**：G2 授予闸门是 fail-open 的——未登记权限直接放行（`src/addon/access/domain/groups/admin.rs:305-307`）。现有反向检测只对含 `secret/credential/reset/token/rotate/key` 关键词且未登记的权限报错（`scripts/check_architecture.py:194-196,1201-1211`），而清单 5 条里有 3 条本身不含任何关键词（`feishu.approval.write`、`feishu.datasource.write`、`feishu.portal.write`），说明启发式对最危险的 `*.write` 形态天然无效。新增危险权限只能靠人工记忆登记 `[代码]`。
- **参考项目证据**：无。`scs-api` 2,152 个 `.rs` 中 `grep admin_equivalent/管理员等价` 命中 0 条，参考项目没有这套机制。本条是本项目原生闸门的加固。
- **本项目落点**：`src/addon/access/domain/sensitive_permissions.rs:50-71`（5 条清单）、`:146-166`（`every_listed_permission_is_declared_by_the_catalog` 单向断言，已有真实冻结 Catalog 的脚手架）、`src/addon/access/domain/permission_catalog.rs:19`（保留键 `RETAINED_PERMISSION_KEYS`）、`:74,91-100`（目录含 module 默认权限与保留键，共 15 条）。
- **最小改法**：把 `sensitive_permissions.rs:146` 的单向测试扩成双向——冻结 Catalog 投影出的每条权限必须 ∈ `ADMIN_EQUIVALENT_PERMISSIONS` ∪ 测试模块内 `#[cfg(test)] const REVIEWED_ORDINARY_PERMISSIONS`（当前 8 条），未分类即断言失败并打印模板行。**不要**在生产代码新增 `NON_ADMIN_EQUIVALENT_PERMISSIONS` 常量（运行期零消费者），**不要**扩充 Python 关键词表（`manage/write/grant` 会让当前全绿的构建立刻变红，且与"分类完备"自相矛盾），Python 那条已失效的关键词分支可直接删除。
- **验收标准**：新增任意权限键但未分类时 `cargo test --lib` 红；现有 15 条一次性登记后全绿；明确声明覆盖边界——只拦"新增未分类键"，不拦"危险动作复用既有普通键"（后者保持人工评审）。

#### P0-4 契约快照新鲜度断言（6 行，零新增命令）

- **解决什么问题**：`frontend/contracts/openapi.json` 是入库快照，唯一消费者是前端契约测试，刷新完全靠手写纪律。`src/addon/feishu/domain/field_binding.rs:24-27` 源码注释自认"没有门禁会拦住"；`scripts/run_ci.py` 对 openapi/api-types/contracts 零命中。已真实漂移一次：8531782（2026-11-01 03:33）的提交内 `openapi.json` 完全没有 `aggregate_batch`，直到 a603f7a（同日 20:19）才补上，漂移窗口约 17 小时 `[代码]`。
- **参考项目证据**：反例。`scs` 全仓无 swagger/openapi 的生成-比对门禁（grep 零命中），其入库 `public/swagger.json` 与运行期已漂移 1,087 vs 1,127。不能学。
- **本项目落点**：`src/app.rs:415` 的 `openapi_projection_covers_all_catalog_actions`（已构建出与 `examples/openapi-dump.rs` 同源的 document，但从不读快照）、`frontend/tests/engine/contracts/openapi-contract.test.ts:3`（唯一消费快照的测试，只编译输入 Schema）。
- **最小改法**：在 `src/app.rs:415` 的测试里加约 6 行：读取 `concat!(env!("CARGO_MANIFEST_DIR"), "/frontend/contracts/openapi.json")`，解析成 `serde_json::Value` 后与已构建 document 做相等断言，不等即失败并提示运行 `python scripts/dump_openapi.py`。Value 比较天然忽略键序与缩进。不新增 `--check` 模式、不在 `run_ci.py` 加 Command、不额外编译 example、不跑 pnpm。靠现有 QUICK/FULL 的 `cargo test --lib` 生效，反馈早于 FULL。
- **验收标准**：改任一 Action 输入类型但不重跑 dump 时 `cargo test --lib` 红。当前快照与 HEAD 静态对齐，接入应首次即绿 `[待核实]`（本次未能编译验证）。

#### P0-5 operation_id 字面量的类型级断言

- **解决什么问题**：features 下有 60 个去重的手写三段式 `operation_id` 字面量，拼错或后端改名时 `hasOperation` 恒 false，失败形态是静默隐藏整片 UI（`frontend/src/features/access/api.ts:36-43` 注释自己已写明）。现有两个 features 测试只把手抄字面量互相对齐（`frontend/tests/features/access/workspace-api.test.ts:438-446`、`frontend/tests/features/feishu/api.test.ts:569-583`），后端改名不会变红 `[代码]`。
- **参考项目证据**：反例。`scs-web` 用 `import.meta.glob` 按字符串拼路径且不检查文件存在性，19 个 MENU 无页面就是这么来的。
- **本项目落点**：`frontend/src/engine/contracts/api-types.ts:2824`（已生成 `export interface operations`，天然是 operationId 联合类型）；60 条字面量已收敛进 8 个 `OPERATION_IDS` 常量块（`access/api.ts:44`、`workspace-api.ts:48`、`feishu/api.ts:83/98/111/140/157`、`portal-api.ts:56`）与 `UserAdminPage.tsx:33-34` 两处内联。
- **最小改法**：在非生成文件里定义一次 `type OperationId = keyof operations | "feishu.datasource.pull_now" | "feishu.datasource.pull_schedule" | "feishu.option.approval_options"`（后三条是 `can_pull()/is_usable()` 条件注册、设计性缺席，不是笔误），给 8 个常量块加 `satisfies Record<string, OperationId>`。拼写错误在 `pnpm typecheck`（`scripts/run_ci.py:42` 已跑）报错，零新增测试框架、无正则误伤。注意 `registry.ts:26` 的键是视图注册 id 而非调用 id，不纳入。
- **验收标准**：故意写错一个 operation_id 时 `pnpm typecheck` 红。需显式接受上限：检查以已提交快照为基准，快照漂移会削弱它——所以 P0-4 是本条的前置。

### P1：中期（S/M 成本，需跨仓或跨层协调）

#### P1-1 Step-up 契约口径收口，并给 2 个自服务 handler 补内联再认证

- **解决什么问题**：`807dc91`（2026-10-09）系统级移除 Step-up 后，`93c620e` 只校正了 README/AUDIT/AUTHZ_GRANTS/frontend AGENTS，根 `AGENTS.md` 成为唯一未同步的权威文档；`[step_up]` 仍是必填配置并参与启动校验（`src/config/mod.rs:37,989`）却 `build_manager` 无调用点，是死配置。同时 `totp_deactivate` 文件头声称"停用前须出示第二因子"，实际输入为空结构体、handler 只校验登录（`src/addon/account/user/actions/totp_deactivate.rs:1-5,15-17,50-76`）`[代码]`。
- **参考项目证据**：无。`scs` 全仓无 step-up 代码（grep 零命中），这条不是从参考项目学来，而是从框架未使用能力反推。
- **本项目落点**：`AGENTS.md:7/58/86/130`（4-5 处失实，`:130` 的"proof 经 Redis 原子单次消费"最实质）、9 个 Action 文件头仍写"需 Step-up"（`admin_disable_user.rs:1`、`admin_enable_user.rs:1`、`admin_issue_password_reset.rs:1`、`change_email.rs:1`、`delete_account.rs:1`、`request_change_email.rs:1`、`revoke_session.rs:1`、`totp_activate.rs:1`、`totp_setup.rs:1`）、`src/config/mod.rs:1184`（`build_manager`）、`D:/code/lib_yang/crates/yang-base/src/action/step_up.rs:139,283`（框架原语仍在）。
- **最小改法**：**不重新挂 StepUpMiddleware**（会让敏感 Action 恒返回 428 而客户端拿不到 proof，属功能回归，且推翻已登记决策）。① 文档侧：AGENTS.md 4 处 + 9 个文件头统一改为"Step-up 未装配／仅框架能力"，`config.example.toml:32-37` 标注"当前运行期未使用"，并决定 `[step_up]` 是删除还是保留；② 代码侧只做两处内联再认证：`totp_deactivate` 增加 TOTP/恢复码字段，复用 `Account::verify_second_factor`（`totp_activate.rs:13,68` 已有 `TotpLiteVerifier`）；`change_email` 增加当前密码字段，照抄 `change_password.rs:43-69` 的 `passwords().verify` + `AuthOperation` 限流模式。不新增端点、不改前端、不引 Redis proof store。
- **验收标准**：文档 grep "Step-up" 只剩"未装配"措辞；两个 handler 的非法凭据用例返回 400/403 而非成功；`cargo test --lib` 与集成测试全绿。参考 `CONFIGURATION.md:276-280` 的既有裁定（登录阶段已要求第二因子），不要把它写成安全缺陷。

#### P1-2 raw-sql 门禁覆盖 yang-db 的参数化逃生口

- **解决什么问题**：`scripts/check_architecture.py:131-133` 的 `RAW_SQL_INVOCATION_RE` 只匹配 `sqlx::query*`，而 `yang-db` 暴露的 `Database::query_with_params/execute_with_params`（`D:/code/lib_yang/crates/yang-db/src/mysql/database.rs:454,499`）与 Transaction 版（`transaction.rs:140,93`）同样执行任意 SQL。生产代码 `src/infrastructure/schema.rs:54-61` 正在用，既不在 `docs/architecture/raw-sql-boundaries.md:22-31` 的三条登记清单里，也不会被门禁发现——门禁"每个含生产 SQL 查询的文件必须恰好声明一个边界"的不变量今天已被违反 `[代码]`。
- **参考项目证据**：无。`scs` 不做 raw-sql 边界登记，这不是规模补丁，是本项目治理机制内的漏洞。
- **本项目落点**：`scripts/check_architecture.py:131-133`（正则）、`:889-911`（`raw_sql_boundary_path_allowed` 把 schema-validator 限定为 `src/infrastructure/audit/schema.rs`）、`:986-1005`（代码↔文档按 `(kind,id,path)` 三元组双向比对）、`:609-613`（`production_source` 在首个 `#[cfg(test)]` 处截断，测试模块无需额外排除）。
- **最小改法**：正则追加 `|\.(?:query|execute)_with_params\s*\(`（**不要**放宽到 `.query(`/`.execute(`——会误伤 259 处 DSL 调用）；`raw_sql_boundary_path_allowed` 的 schema-validator 分支放宽为允许 `audit/schema.rs` 与 `infrastructure/schema.rs`；在 `src/infrastructure/schema.rs` 顶部补 `//! raw-sql-boundary: schema-validator legacy-index-guard`；在文档清单表与 HTML 注释区各加一条。注意"只改一处正则加一条登记"不成立——路径白名单与文档必须同步，否则会红。`Database::query/execute`、`init/create_table/drop_table`、`sqlx::query!` 宏仍不受约束，应在文档里明确这是已知残余面。
- **验收标准**：故意在 `schema.rs` 删掉声明时门禁红；`python scripts/check_architecture.py --self-test` 全绿；文档与代码双向一致。impact 是 medium 而非 high（仅 1 处良性只读调用，值仍走 bind）。

#### P1-3 删除 api-types.ts 生成轨（含流程要求）

- **解决什么问题**：`frontend/src/engine/contracts/api-types.ts` 12,636 行、自动生成、全仓零 import（唯一代码侧引用是 `frontend/eslint.config.js:16` 的 ignore）。91 个 operation 中 73 个的成功响应 `data` 被生成为未类型化的 ApiResponse 形状，未类型化叶子在 `…["data"]["data"]`；全部 feishu operation 都落在 unknown 桶里，生成类型对飞书零响应结构。而 `AGENTS.md:78` 要求每次后端契约变更都重跑并提交这个 12.6k 行死产物 `[代码]`。
- **参考项目证据**：无（`scs` 不知道 OpenAPI，没有生成类型轨）。
- **本项目落点**：`scripts/dump_openapi.py:93-122`（步骤 3 生成 api-types.ts，附带 corepack/pnpm 版本坑）、`frontend/package.json:72`（`openapi-typescript` devDependency）、`AGENTS.md:78`、`frontend/AGENTS.md:66`、`docs/architecture/frontend-rebuild/adr-4-contracts-and-validation.md:13,17,49`。
- **最小改法**：删 `dump_openapi.py` 第 3 步及仅供其使用的 `rebase_local_definition_refs`（`:36-90`）；删 `api-types.ts` 与 `openapi-typescript` 依赖；保留 `openapi.json` 继续供输入 Schema 的 Ajv 门禁；同步 5 处文档/配置去掉"必须生成并提交类型生成物"。**不要**做"接 18 个 typed operation 到前端"——那 18 个是 `access.grants.*` 与 `account.user.*`，features 真实引用的约 60 个 id 里只有 4 个命中，救不了已知漂移，还要新增一条"typed 数不得下降"的维护断言。
- **验收标准**：`pnpm check` 全绿；git 里不再有 12.6k 行无人消费文件；`openapi-contract.test.ts` 仍通过。若日后真要防响应字段漂移，正确路径是让 `GroupSummaryView` 等声明输出契约、让快照携带 response schema（参见 `.sqlcheck/digest.md:416-417`），而不是复活静态轨。

#### P1-4 修复成功响应双层信封（跨仓，影响低但事实确凿）

- **解决什么问题**：73/91 个 JSON operation 的 200 响应 `data` 被投影成 ApiResponse（`title=ApiResponse`，自带 code/message/data），而运行时 `wrap_dispatch_output` 对 ApiResponse 直接透传，HTTP 只输出单层信封。根因是这批 Action 的 `Output` 就是 `ApiResponse`（如 `src/addon/access/groups/actions/list_groups.rs:77`），`schemars::schema_for!(Output)` 生成 ApiResponse schema，`openapi.rs:168` 又无条件再包一层 `success_envelope_schema` `[代码]`。
- **参考项目证据**：无（`scs` 的 OpenAPI responses 全空，是另一种性质的缺陷）。
- **本项目落点**：`D:/code/lib_yang/crates/yang-base/src/definition/openapi.rs:168,283-293`、`D:/code/lib_yang/crates/yang-base/src/action/response.rs:531-543`、`D:/code/lib_yang/project/yang-system/src/app.rs:415`。
- **最小改法**：在 `success_envelope_schema` 里检测 `data_schema` 本身即信封（`title=="ApiResponse"`，**不要**用"properties 同时含 code/message/data"这种可能误伤真实业务对象的启发式）则原样返回；重跑 dump 并提交两个生成物（若 P1-3 已删 api-types.ts，则只提交 openapi.json）。**不要**把 responses 纳入前端契约测试、也不要加"data 不得为信封"的 app.rs 断言——修后这批 `data` 仍是 `serde_json::Value`，加形状断言只证明投影自洽、制造假安全。定位 low 而非 high：唯一下游零消费、运行时本就单层。
- **验收标准**：重生成后 `openapi.json` 中不再出现 `data.title == "ApiResponse"`；`src/app.rs:415` 契约测试与前端 `openapi-contract.test.ts` 全绿。注意 `yang-base` 是共享 crate（`Cargo.toml:9` path `../../crates/yang-base`，版本 0.4.0），改动需随版本发布。

#### P1-5 axe 挂到已有完整 mock 的两个 spec

- **解决什么问题**：`frontend/e2e/accessibility.spec.ts` 只有 2 条 axe 用例（登录页 `:29`、DEV workbench `:54`），而 `docs/architecture/frontend-rebuild/adr-5-acceptance-and-cutover.md:33-34` 明文要求"核心页面 axe 零 critical/serious"——生产面确实没有任何可访问性回归 `[代码]`。
- **参考项目证据**：无。`scs` 全仓 grep 不到 axe-core/AxeBuilder，不是从参考项目平移。
- **本项目落点**：`frontend/e2e/account.spec.ts:10-41`（已 mock `/api/v1/users/me`）、`frontend/e2e/approval-config-business.spec.ts:4-128`（已 mock 完整 ui-catalog action + 24 行数据，且 `frontend/e2e-production/approval-config-business.spec.ts:2` 直接复用，生产构建顺带覆盖）。
- **最小改法**：在这两个 spec 各加一次现有 `expectNoSeriousViolations`（helper 在 `accessibility.spec.ts:15-27`），约 10 行。**不要**按原方案新建 4 条裸导航用例：e2e 后端是 `examples/frontend_demo`，只注册 demo addon（`examples/frontend_demo/main.rs:32-36`），页面会因目录缺 operation 只渲染"当前身份没有权限"提示（`DatasourceListPage.tsx:511-517`、`ApprovalConsolePage.tsx:31-37` 等），axe 测不到真实界面。
- **验收标准**：两个 spec 的 axe 断言通过；故意引入一个缺失 `label` 的输入应导致失败。数据源详情页（2,272 行）与审批配置对话框（1,316 行）的覆盖属独立的 M 级任务，需另补目录+数据 mock。

### P2：观察/待触发（有明确前置条件，现在做是负收益）

#### P2-1 运行期保留期清理（S77 范围，需 owner 批准）

- **问题**：`feishu_approval_request_log` 与 `feishu_datasource_portal_log` 无任何按时间的清理路径，会无界增长；`portal_log` 在每次含 `rate_limited` 的查询都写一行，写放大最严重。
- **参考项目证据**：无（`scs` grep retention/清理 仅命中无关文件）。
- **落点**：`src/addon/feishu/approval/domain/request_log_table.rs:67`（`created_at` 无索引）、`src/addon/feishu/approval/actions/delete_config.rs:233-235`（明确不随之删除）、`src/infrastructure/session_cleanup.rs:26-30,69-99`（可复用骨架）。
- **前置与约束**：已立项 S77（`docs/assessments/2026-10-25-network-infrastructure-audit-adversarial-review-and-roadmap.md:751-758`），要求 owner 书面批准保留窗口、dry-run 只报数不删、首次 live 前备份、分批删除并打指标；`docs/assessments/2026-10-25-infrastructure-review-feishu-outbound.md:145` 要求"成功但未回填/未知通知/对账未解决的行不能按一般日志年龄删除"。数据库写操作需按全局规则二次确认。
- **待触发**：拿到 owner 的保留窗口答案后，先补 `created_at` 索引（增量、不改表），再上分批删除。**不要**自行决定 90/180 天默认值，也不要把 `login_event`/`feishu_approval_event` 捆绑进来（前者是用户可见安全历史、后者是可人工重放的证据）。

#### P2-2 告警消费端（S50）之后再加停滞规则

- **问题**：`feishu_approval_last_progress_timestamp_seconds`（`feishu_approval_worker.rs:406`）与 `feishu_approval_worker_stall_total`（`:468`）已导出却无规则引用；worker 卡在认领前时 `feishu_approval_pending` 停在旧值，现有唯一审批告警同时有假阴/假阳。
- **硬前置**：`docs/operations/2026-10-25-s01-production-facts.md:60,106` 确认现网无 Prometheus/Alertmanager/Grafana/Alloy/exporter，13 条规则"没有消费者"；`deploy/deploy-blue-green.sh:105` 的 metrics 默认绑 `127.0.0.1`。**没有采集端时新增规则零可观测收益**。
- **附加缺陷**：`approval_scan_interval_seconds` 合法上限 600s（`src/config/mod.rs:713,717`）而现有看门狗阈值 `PROGRESS_STALL_AFTER=180s`（`feishu_approval_worker.rs:134`），合法配置下会常态误报——这个问题独立于告警，应单独修。
- **待触发**：S50 落地后只加一条 `time() - feishu_approval_last_progress_timestamp_seconds > max(ROUND_WORST_CASE_SECONDS, approval_scan_interval_seconds) + 余量`（阈值随可配置间隔推导，不硬编码 360），并给启动/重启的 absent 留宽限。`increase(stall_total[10m]) > 0` 与现有 WARN+计数重复，删除。

#### P2-3 feishu_event worker 的心跳指标（单条，不带采集端则不做）

- **问题**：feishu_event worker 驱动审批结果回写，任务死掉后事件在表里堆积，而进程、readiness 与审批派发都保持绿色；它只有 `skipped/terminal` 计数（`feishu_event_worker.rs:272,297,313`），没有轮次或成功信号。其余 worker 都有覆盖路径：`authz_outbox` 已有 `oldest_age_seconds` 双级告警（`authorization/worker.rs:135-136` + `ops/prometheus/yang-system.rules.yml:92-111`）、feishu_pull 有表级 `last_success_at` 且达阈值发邮件（`src/addon/feishu/domain/pull.rs:723,748`、`alert.rs:209-233`）、会话清理/过期直授有 `runs_total{result}`。
- **最小改法**：只在 `run_loop` 每个 tick 结束处（含空轮 return 路径 `feishu_event_worker.rs:252`）set 一个 `feishu_event_last_tick_timestamp_seconds` gauge，配一条 `increase(feishu_event_terminal_total[15m]) > 0` 规则（后者零代码）。不做 6 worker 统一 last-success 框架，不加 `absent()`。
- **待触发**：同 S50。

#### P2-4 审计事件的行为级断言（不做静态门禁）

- **问题**：`docs/contracts/AUDIT.md:31-45` 列出的必须覆盖动作目前全部已有 `append_in_tx`，零现存漂移；但已有行为断言只覆盖 `update_config`/`delete_config`/`retry_event`/`revoke_session`（`tests/feishu_approval_dispatch_integration.rs:993-998,1050-1055,1471-1479`、`tests/session_revocation_integration.rs:648-651`），`register`/`change_password`/`reset_password`/`disable_self`/`logout`/`create_config` 无断言。
- **为什么不做原建议**：原建议的"扫描 register 链 + 硬编码 AUDIT.md 清单"是循环论证——新增高权限动作时既漏改 AUDIT.md 也漏改 checker；且"同文件出现 `append_in_tx`"字符串不能证明真不变式（成功路径、同一事务、正确 action）。死分支或旁路写法照样全绿，制造虚假安全感。
- **最小改法**：在既有集成测试里为上述 6 个动作补 `audit_event` 依 `result='succeeded'` 的行数与摘要断言，并按 `AGENTS.md:118` 做摘除式变异证明断言会红。纯测试、零生产风险。

#### P2-5 其余小项（各自 1 行到 10 行）

| 项 | 最小改法 | 现状/证据 |
|---|---|---|
| `admin-metadata` 空 feature | 从 `project/yang-system/Cargo.toml:9` features 列表删掉 `"admin-metadata"` | 空 feature（`crates/yang-base/Cargo.toml:53`），只门控一个无依赖 pub 模块，项目零引用。按**清理项**而非优化项排期；不做 CI 禁引用（该机制不存在，为假想误用新建规则违反极简） |
| `UiCatalogAction` 挂载断言 | 在 `src/app.rs:213-248` 导航测试里**新增**约 5 行，断言 `account.user` 模块含 `ui_catalog`、route `GET /.well-known/yang/ui-catalog` | 目前无任何测试断言该 Action 挂载（删 `src/addon/account/user/mod.rs:70` 那一行可同时骗过现有 app.rs 测试与前端单测）。**严禁**把该路由加进 `RESERVED_FRAMEWORK_ROUTES`——会立刻触发 `RouteConflict`（`validate.rs:676-689`）与 transport 的"框架保留路由缺少处理器" ConfigError（`transport/axum.rs:262-274`） |
| CI 只钉 `dtolnay/rust-toolchain@master` | 把 `.github/workflows/ci.yml:33,91,143` 的浮动分支改为固定引用 | 唯一真正浮动项。**不要**钉 `LIB_YANG_REF`（`AGENTS.md:141` 已把 master 漂移写成有意取舍并记录"重跑即恢复"；仓库无 dependabot，钉 SHA 会打断流程），也**不要**把 actions 全量钉 SHA（只会静默变旧） |
| `run_round_or_shutdown` 复用 | 给 `grant_expiry_worker.rs:77`、`session_cleanup.rs:75`、`authorization/worker.rs:202` 三处本轮 await 各包一层现有 helper（1-3 行） | 现仅 approval/event 使用（`feishu_approval_worker.rs:557,564`、`feishu_event_worker.rs:163,178`）。**不要**新建 `run_periodic_worker`（pull 有 mpsc 手动触发 + PullSchedule 重排、approval 有独立 watchdog、event 有启动期订阅、session_cleanup 延迟随删除量动态变化，只有 2 个循环能被原签名吸收） |

---

## 5. 不建议照搬的模式

### 5.1 参考项目自身的三类结构性弱点

**① 以运行时字符串分派代替编译期校验。** 参考项目的失效模式是"漏一行、编译器不管"：12 个 `.rs` 从不参与编译、2 个 Module 未挂进 `Addon::module`、至少 5 条悬空 `api_run`、44 处 `api_run` 后直接 `unwrap`。本项目用构建期冻结 Catalog 把同类错误前移到启动前，是正确方向；移植动态注册表只会引入没有消费者的间接层，并放弃 `--locked` 构建下的确定性。

**② 后端承载 UI 渲染语义。** `table.rs:39-65` 让后端现场拼 `columns/btn_all/btn_api/cnd`，前端 `is_show(cnd,row)` 逐行求值 12 种 `btn_type` 字符串。这套表达力确实能覆盖"按行状态启用/停用"，但代价是后端必须懂渲染细节、拼错静默降级、无编译期保护。本项目 `presentation` 只声明 `interaction/placement/confirmation/availability` 是刻意分层；第 4 节也没有采纳"给 presentation 加 `row_condition`"——因为生产侧 `.view()` 声明数为 0（唯一在 `examples/frontend_demo/main.rs:30`），为零消费方的引擎扩 DSL 是纯成本。

**③ 宽松信任边界。** `update(request.body)` 实测 ≥168 处、`insert(request.body)` ≥91 处、通用 `table_list` 把客户端 `where_and` 的比较运算符与比较值原样下推；授权用 `LIKE '%api%'` 对 JSON 文本列做子串匹配且 `interface_type==API` 直接放行（`admin/auth/verify.rs:63-79`）；`org_org` 由请求头带入、632 处手写 `where_and("org_org")`，`sale/order/put.rs:53` 的更新只按 id+state 完全不校验租户。这些都是系统性风险，修复要动上千处；本项目没有对应面，不应主动引入。

### 5.2 22 条被淘汰建议的归类理由

候选建议中 22 条经事实核对或价值核对后不采纳。它们不是"以后再说"，而是"照做在当前状态下是负收益"，归为六类：

| 类别 | 代表建议 | 不采纳的决定性证据 |
|---|---|---|
| **与既有决策正面冲突** | 直授路径补委派子集校验并禁止自授 | `docs/contracts/AUTHZ_GRANTS.md:67-70` 明文把 `grants.write/groups.write` 定义为"危害面被闸门封顶的委派权限"；`tests/permission_groups_integration.rs:4682-4699` 以"只持 grants.write 授予未持权限必须成功"作反向对照，并注"拒绝面过宽同样是故障" |
| | 给业务冲突加 `BaseError::Conflict` 并映射 409 | 设计 `docs/architecture/2026-09-24-permission-groups-and-bootstrap-design.md:448` 明确"不为个别用例扩展框架错误类型"，`:466-471` 更写明"文档错、代码对"；且热点是确定性资源状态守卫，非并发冲突，前端 `PermissionGroupsPage.tsx:61-62` 只渲染 message，409 无可观察收益 |
| | 把 Step-up 中间件接回敏感 Action | `807dc91` 系统级移除 + `docs/assessments/2026-10-20-full-project-review.md:107-135` 登记"暂不修复"；只挂中间件会让敏感 Action 恒 428 而 completion 端点不存在，属功能回归。正确落点是 P1-1 的内联再认证 |
| | 把管理员等价关键词表加 `manage/write/grant` | 会让当前全绿的构建立刻变红：`access.grants.write` 与 `account.users.manage` 都是文档明确判定为非管理员等价的委派权限（`sensitive_permissions.rs:13-22`），且该检查命中即写 errors 直接失败（`check_architecture.py:1203-1213`） |
| **依赖已由语言或测试强制，添加机制是冗余** | 把 Addon 依赖从"构造顺序"提升为 `depends_on` | 依赖已由 `account::build_addon` 实参签名编译期强制（`src/app.rs:83-93`、`src/addon/account/mod.rs:49-56`）；真实耦合双向（access 反向消费 `account::authorization_port()`），单向 `depends_on` 记一半；`validate_dependencies` 不查环，会静默接受；建议里的 `addon_name!` 宏不存在（应为 `addon!`，`crates/yang-base/src/lib.rs:113-119`） |
| | 给组合根补"目录↔挂载↔Catalog"一致性门禁 | `dfd41a0` 证明"声明未挂载"是本仓库刻意的契约先行分阶段提交，该门禁会误报既有工作流；且 `docs/architecture/2026-09-24-permission-groups-and-bootstrap-plan.md:26` 把"生产者先落地、消费者后续提交 + 带理由的 `allow(dead_code)`"定为惯例 |
| | 补 SCHEMA.md 承诺的"执行后二次规划必须为空" | `D:/code/lib_yang/crates/yang-base/tests/schema_sync_integration_test.rs:98-101,252-267,301-305` 与 `tests/schema_apply_integration.rs:211-222,270-281` 已在真实 MySQL 上断言 apply 后为 noop，并接入 CI；运行时自检在 DDL 已提交后 fail-closed，会把良性残留放大成启动崩溃循环 |
| | 索引名被"同列集"吞掉时留痕 | 被吞的只是索引名，索引本身、唯一性与列集都仍生效（`plan.rs:319` 对非唯一期望只要库内同列唯一也算满足）；所举 `uk_feishu_approval_task_record_id` 是单列键，与新声明复合键列集不同，走的是"未知索引不删"分支，在该处加 warn 观测不到（历史见 `a6cddb6`） |
| | 用现有 SessionCleanupWorker 模式补非审计表保留期 | 建议引用的"2000 行停摆"已由定向查询修复（`feishu_approval_worker.rs:1057-1099`）；保留期窗口须 owner 书面批准，建议"保留期先写常量"违反该约束；S77 已立项，范围应是 request/portal log（P2-1） |
| | 补强审计覆盖机械门禁（AUDIT.md 清单 → CI） | checker 必须把中文散文清单再硬编码一份，会漏写审计的人同样漏改 checker；字符串命中不能证明"同事务、成功路径"的真不变式。改为 P2-4 的行为断言 |
| **无消费方的"机制感"扩展** | 给 action presentation 加 `row_condition` | 生产侧 `.view()` 声明为 0（`src/app.rs:248-253` 断言 views 为空），只有 demo/Workbench 用；且 `usePresentedActions` 按 view 一次性计算 `rowActionGroups`（`:70-93`），按行过滤需改数据结构，成本远大于 M |
| | 收敛没有生产负载的通用 TableView 契约 | 默认视图 `data_action=None` 是有意的暴露闸门（`docs/superpowers/specs/2026-10-08-navigation-catalog-single-source-design.md:209`），`docs/reference/FRONTEND_FIRST_PRINCIPLES.md:224` 禁止"靠命名规则猜测数据源"；改成构建错误会让所有带表模块无法构建 |
| | 统一 UI schema 枚举兼容策略（给 `interaction` 加 `.catch`） | `frontend/tests/engine/contracts/ui-catalog.test.ts:179-208` 用测试锁定"拒绝未知 interaction，禁止降级为 invoke 执行"；降级会在 `use-presented-actions.ts:137-141` 直接发起写操作 |
| | 给业务指标加 `business_metrics` 模块 + 标签枚举 | 40 处埋点在 10.8 万行规模下是稀疏而非蔓延；所有标签实参要求 `&'static str`（编译期已排除高基数）；各族词表互不相同（approval_row_edit: edited/timeout/no_row/failed；tenant_token: lock_error/waited/...），单一枚举要 15+ 变体且拦不住跨族误用；真实漂移风险在 code↔rules 却没覆盖 |
| | 补强 worker 骨架 `run_periodic_worker` | 只有 grant_expiry 与 authorization 两个循环能被原签名吸收；现有 5 个手写循环的 Err 分支全部 `error!` 后继续，不存在建议所称的"静默 continue"；监督登记点在 `bootstrap.rs:38-44,491-504`，换循环形状不解决 |
| | 补强三套持久化 claim/settle 共享原语 | `approval_notification.rs` 全文无 sqlx/INSERT/UPDATE，通知去重靠同一任务行的 `last_error`（`feishu_approval_worker.rs:4411-4419`），根本不是第三套持久化；剩下两套在并发模型、围栏（generation/effect_phase vs worker_id）、退避上均不同；`claim_batch(pool, table, ...)` 用表名参数化直接违反 raw-sql 静态字面量门禁（`docs/architecture/raw-sql-boundaries.md:17-18`、`check_architecture.py:889-918`） |
| | 审批 worker 域编排下移/拆组 | 9,081 行里 4,175 行是文件内单测（`feishu_approval_worker.rs:4906` 起），生产约 4,905 行；`AGENTS.md:52` 直接点名该 worker 属 infrastructure，`:85` 规定 domain 只放共享机制、业务用例内联在 Action；`approval_aggregation/mod.rs:14` 明写"本层不做 I/O"而 `maintain_groups` 直接持有事务；集成测试只走 HTTP 派发出口，不覆盖这些函数 |
| | 把 OAuth state 与导入互斥挪到 Redis | 蓝绿 cutover 是"先 stop_pair 再 start_pair"（`deploy/deploy-blue-green.sh:1594-1600`），无并行服务窗口；冒烟绿容器是一次性 `--smoke-check`，不监听业务 HTTP（`:1421-1422`）；`user_token.rs:25-28` 已明文论证"为它引 Redis 或加列都不值" |
| | 收敛列表分页样板 / 加文件体量门禁 | 分页校验已收敛：6 个 list Action 复用 `feishu/domain/list_input.rs:56-67`，框架 `SelectQuery` 也有同判据；仅剩 2 处形态不同（GET i64、POST + terminal_only）的合法例外。体量门禁：9,081 行里 46% 是测试，且 `scs` 最大生产 `.rs` 只 790 行，这不是参考项目规模教出来的 |
| **参考项目的规模补丁，本项目规模不成立** | 让 OpenAPI 快照表达"条件注册"接口 | 快照缺的 10 个 operation 是 `can_pull()/is_usable()/[feishu]` 门控下的设计性缺席；`docs/architecture/feishu-datasource-table-config-decisions.md:478` 明写"不在快照里是正常的……无行为影响"；前端枚举能力用运行时 ui_catalog + `hasOperation`，不用静态快照 |
| | 补一个管理端可见的登录/会话安全事件入口 | `scs` 全仓 grep 不到 login_event/security_event，功能不是从参考项目学来；`account-system-roadmap.md:225` 把管理控制台视图明确推迟到"有专职运营团队"；`ip/user_agent` 被 `login_event.rs:73-75` 定义为个人数据，跨用户展示是隐私面扩张 |
| | 给引擎/控制器加"文件体量"类门禁 | 同上：参考项目没有该门禁，`scs-web` 自己最大 `.vue` 2,925 行；把行数当门禁会引入 7 条手工豁免清单并复刻"手工登记会漂移"的失效模式 |
| **前提已过期或事实不成立** | readiness 与 metrics 开关解耦 + self-refresh tick | 生产镜像 HEALTHCHECK 每 30s 请求 `:9090/health/ready` 刷新 gauge（`docker/app/Dockerfile:76-81`），正常部署未禁用；耦合是 `docs/contracts/CONFIGURATION.md:88-91` 与 `OBSERVABILITY.md:88-92` 的明文契约；检测探针停摆零 Rust 改动即可（对已导出的 `yang_system_readiness_checks_total` 加一条规则） |
| | 关闭未消费的框架扩展面（含新建 CI 禁引用） | 事实成立但收益是"少编译一个无依赖模块 + 消除读者歧义"，应按清理项排期；`check_architecture.py` 不存在通用"禁用符号"机制，为假想误用新建 CI 规则违反极简。只保留 `Cargo.toml:9` 单行删除（P2-5） |

---

## 6. 本项目已对齐或优于参考项目之处

1. **扩展注册：构建期冻结 vs 请求期字符串 match。** `AppBuilder::build` 七类交叉校验后冻结（`crates/yang-base/src/definition/builder/app.rs:131-174`），请求期 BTreeMap 预解析；参考项目 URL 三段拼字符串查表，559 处 `.unwrap()`，改名无编译期保护 `[代码]`。
2. **授权：可枚举目录 vs 子串匹配。** 权限目录 15 键由 Action 声明的 `.permissions([...])` 自动进入（`src/addon/feishu/domain/permission_catalog.rs:70-157`），构建期投影 AuthorizationPolicy 链尾 fail-closed；参考项目 `LIKE '%x%'` 会碰撞、无索引、读改写整列丢并发更新 `[代码]`。
3. **审计：事务内 append-only vs 异步尽力而为。** `append_in_tx` 与业务状态变更原子提交（`src/infrastructure/audit/repository.rs:163-188`），append-only 由门禁强制（`scripts/check_architecture.py:848-863`）；参考项目按钮日志经 Redis pub/sub 异步落 `api_log`，467 个 API 型动作静默 `[代码]`。
4. **认证：Argon2id + 可撤销会话 vs 单轮 SHA-256 + 30 天不可撤销 token。** 本项目有 `user_session` 行 + Refresh JTI 原子轮换 + 逐设备撤销 + `credential_version/authz_version/revoke_by_subject` 水位（`docs/architecture/session-ttl.md`）；参考项目改密不清理 token、泄露 token 最长 30 天不可撤销 `[代码]`。
5. **Schema 演进：只增不删 + 预检 + GET_LOCK vs auto-DDL 可能静默销毁数据。** `sync.rs:140-207` + `plan.rs:284-289,313-322`；参考项目据测绘会 DROP COLUMN 且无预检 `[待核实]`（本仓无可验证源码）。
6. **运行时：可取消 + 关闭预算 vs 61 处 thread::spawn 不存 JoinHandle。** `src/bootstrap.rs:462-570` + `worker_shutdown.rs`；参考项目任一后台线程 panic 会静默永久停摆且进程仍显示健康 `[代码]`。
7. **可观测性：`/metrics` + 13 告警 + promtool 正反演练 vs 只有 env_logger。** `D:/code/lib_yang/crates/yang-runtime/src/observability/telemetry.rs:138-276`、`ops/prometheus/yang-system.rules.yml:42-218`；参考项目 `set_var("RUST_LOG")` 后未解析，release 固定 Debug `[代码]`。
8. **配置与凭据：fail-closed 注入 vs 构建期烤进镜像。** 启动期单次合成（config.toml < `YANG_SYSTEM_*` < secret provider），未知键/空值/NUL 全 fail-closed，生产强制 `metrics_enabled=true`（`src/config/mod.rs:1138-1140`）；参考项目 7 个 config 变体被跟踪且凭据非空 `[代码]`。
9. **前端契约：zod + Ajv fail-closed 动态校验 vs 完全无校验。** 后端下发的 `input_schema` 交给前端 Ajv 2020-12 白名单校验（`frontend/src/engine/contracts/ui-catalog.ts`、`renderers/form/ajv-resolver.ts:56-63`），是真门禁；参考项目 162 个硬编码 API 名、6 个 fetch 包装、无运行时校验 `[代码]`。
10. **持久任务：阶段化状态机 vs at-most-once 丢消息。** `effect_phase`/`owner`/`lease_until`/`attempts`/`unknown→review`/`uuid` 回捞防重复建单（`src/infrastructure/feishu_approval_worker.rs:6-40`）；参考项目 `handle_listen` 成功失败都 `remove_file`，消息直接丢 `[代码]`。

**需要如实承认的 3 个非领先项**：① 后端集成测试直接调用 Action、前端 E2E 接无数据库的 `frontend_demo`，没有一条"真 HTTP + 真后端 + 真前端"全栈用例；② 5 个手写控制台的边际成本高于参考项目"action + 一个 vue 文件"，这是刻意的分层取舍而非能力差距；③ 无行级/资源级授权，声明式权限只能表达"能调用这个动作"。

---

## 7. 本次对比的边界：8 个维度之外的高优先级未覆盖面

材料 B 指出的缺口确实不在 8 个维度内，且其中多项严重度高于本报告第 4 节的任何条目。按优先级列：

1. **边缘与传输层（已知 P0）**：`deploy/tls/nginx.host.conf`、`deploy/tls/README.md`、`docs/assessments/2026-10-25-infrastructure-review-edge-delivery.md` 整域未进入任何维度。明文边缘未关、CSP、静态资源缓存、可信代理头都属于这里。**任何一次架构扫描若跳过它都会漏掉最高严重度问题**。建议先做 S01 只读核对，别先动 TLS `[文档]`。
2. **依赖与供应链**：应用仓无 `deny.toml`/cargo-deny，CI 只有 fmt/clippy/test --locked 与 pnpm audit，无 Rust 漏洞扫描与许可证审查。`docs/architecture/feishu-bank-branch-datasource.md:444-445` 明写"没有 cargo-deny 门禁，许可证审查是人工责任"；`docs/assessments/2026-10-25-network-infrastructure-audit-adversarial-review-and-roadmap.md:307` 已登记为 P1 `[文档]`。
3. **告警送达与运维闭环**：13 条规则无采集端（S50），`docs/operations/LOG_SHIPPING.md:7-14` 确认也没有日志采集器；`docs/contracts/SLO.md` 定义 p95<500ms 与 99.9% 可用性，但实现与 SLO 之间没有对照。
4. **备份/恢复/DR**：`docs/operations/RUNBOOK_BACKUP.md`（CMS 加密、异机摘要校验、明确写 last-success 不代替解密与恢复验证）、`deploy/backup-mysql.sh` 存在，但无恢复演练记录；RPO/RTO 未定义。
5. **性能与容量**：`docs/architecture/benchmarks/{refresh-ttl,tenant-capability,work-scale}.json` 与 `tests/refresh_load_benchmark.rs` 已存在，但 8 个维度无一条分析延迟、吞吐、索引选择性、连接池尺寸或 N+1。基线存在但未被用于决策 `[待核实]`。
6. **安全工程面**：webhook 的 `verification_token` 校验与 AES-256-CBC 解密（`src/addon/feishu/event/actions/event_challenge.rs:40-120`）、138 处限流点、xlsx 上传、SSRF、静态凭据加密，都没有形成威胁模型与信任边界清单；8 个维度只把安全切成 identity-authz 的授权一块。
7. **隐私与数据生命周期**：已有 `export_data`/`delete_account`/`login_event` 隐私删除，但没有 PII 清单、数据分级、保留策略一致性分析。`login_event.rs:73-75` 已把 `ip/user_agent` 标为个人数据，但无跨表一致性核对。
8. **i18n**：`frontend/src/shared/lib/product-locale.ts` + `frontend/scripts/verify-locale-contract.mjs` 已进 `pnpm check`，是唯一"已有门禁却被整体忽略"的横切面。风险低，但应在完整性清单里占一行。
9. **文档治理/ADR 一致性**：Step-up 漂移是偶然撞见的——`docs/contracts`、`docs/architecture`、`AGENTS.md` 三处口径无一致性门禁。**"文档作为事实源会不会漂"本身应成为评估项**，否则下一次漂移仍只能靠人工发现。

---

## 8. 路线图

### 第 1–2 周（P0，全部 S 成本，可并行）

| 顺序 | 事项 | 依赖 | 风险 |
|---|---|---|---|
| 1 | P0-1 worker 监督扩面 | 无 | 低。`OwnedTask::is_finished` 已暴露；注意 6 个 worker 中 4 个是 `Option` |
| 2 | P0-3 权限分类断言 | 无 | 低。一次登记 15 条；只改 Rust 测试，不动运行期 |
| 3 | P0-4 契约快照新鲜度断言 | 无 | 低。首次接入可能因快照已漂移变红——正好用第一条红证明收益 `[待核实]` |
| 4 | P0-5 operation_id `satisfies` 断言 | P0-4（快照需新鲜） | 低。需先补 6 个桶导出以外的类型 `OperationId` |
| 5 | P0-2 engine 边界 ESLint | 补齐 6 个桶导出；清理 25 处 features 深路径 | 中。rules 2 原文只约束 features，扩到 shell 需同提交改 `AGENTS.md:46-48`；渲染器白名单应收敛为 `shell/pages/{ModulePage,BusinessPage,WorkbenchPage}` |
| 6 | P2-5 `admin-metadata` 单行删除（可选） | 无 | 低。删后跑一次 `cargo check`；影响仅编译期 |

**两周出口标准**：`run_ci.py quick` 与 `full` 全绿；新增 5 条门禁均有一次"故意破坏即变红"的记录。

### 第 1 个月（P1，S/M，需跨层协调）

| 顺序 | 事项 | 依赖 | 风险 |
|---|---|---|---|
| 1 | P1-1 Step-up 文档收口（纯文档，先做） | 无 | 低。需决定 `[step_up]` 配置段去留 |
| 2 | P1-1 的代码半部：`totp_deactivate`/`change_email` 内联再认证 | 前端配合动态码输入；复用 `change_password.rs:43-69` 模式 | 中。会改变自助关闭 MFA 的交互；需契约快照重生成 |
| 3 | P1-2 raw-sql 门禁覆盖 `_with_params` | 无 | 低。必须同时改路径白名单与文档，否则"只加正则"仍会红 |
| 4 | P1-5 axe 挂到 `account.spec.ts` 与 `approval-config-business.spec.ts` | 无 | 低。两 spec 已有完整 mock；生产构建经 `e2e-production` 的 2 行复用自动覆盖 |
| 5 | P1-3 删除 api-types.ts 生成轨 | 需同步 5 处文档/配置 | 中。会移除 `openapi-typescript` 与 corepack 版本坑，但改的是既定设计（ADR-4），需一次性说明 |
| 6 | P1-4 双层信封修复（yang-base） | 需 yang-base 版本 bump + 全仓重跑 `--locked` | 中。跨仓协调；`d9c7b37` 的 `chore(deps)` 是既有先例 |

**一个月出口标准**：`openapi.json` 不再含 `data.title == "ApiResponse"`；`AGENTS.md` 与 9 个 Action 文件头无"Step-up 已装配"表述；api-types.ts 不再入库；axe 覆盖审批与账号两面。

### 季度（P2 + 未覆盖面专项）

| 事项 | 前置条件 | 风险 |
|---|---|---|
| S50 采集端 + 日志采集 | 基础设施决策；`docs/operations/2026-10-25-s01-production-facts.md:106` 已确认缺口 | 中。metrics 默认绑回环，需改部署 |
| 告警规则加停滞/心跳（P2-2、P2-3） | **必须在 S50 之后** | 低但无效前置——没有采集端时零收益 |
| S77 保留期清理（P2-1） | owner 书面批准保留窗口、dry-run 计数、首次 live 前备份、先补 `created_at` 索引 | 高。数据库写操作需二次确认；不能自定 90/180 天 |
| 审计行为断言（P2-4） | 无 | 低。纯测试 |
| 边缘与传输层专项（明文边缘 P0） | 先做 S01 只读核对 | 高。最高严重度，但不要先动 TLS |
| cargo-deny 接入 | 需先补 `deny.toml` 与许可证基线 | 中。可能暴露一批待处理依赖 |
| 性能基线对照 SLO | 读取 `docs/architecture/benchmarks/*.json` 与 `tests/refresh_load_benchmark.rs` 的实际结果 `[待核实]` | 低 |
| DR 恢复演练 | 需目标机与备份介质 | 中。`RUNBOOK_BACKUP.md` 已写"last-success 不代替恢复验证" |

### 全局风险

- **yang-base 是共享 crate**（`Cargo.toml:9` path `../../crates/yang-base`，版本 0.4.0）：P0-1、P1-4 与任何框架侧改动都需版本同步；本地开发就住在 lib_yang 工作树内，CI 与本地会分叉。
- **多 worktree 并存**（`.claude/worktrees` 下 5 个）：任何大 diff 的冲突来源是并行会话而非评审；P1-4 跨仓 + P1-3 删产物应尽量避免与其他在制品同周。
- **数据库安全规则**：本报告所有涉及 DELETE/DDL 的项（P2-1）均需用户明确要求 + 执行前二次确认，不得由脚本自动执行。

---

## 9. 验证待办（本次未验证的事实）

以下均为静态推断或文档记载，进结论前需一次实证：

1. `frontend/contracts/openapi.json` 与当前 HEAD 是否已漂移——P0-4 接入后第一条红即可判定 `[待核实]`。
2. `cargo build --release --locked` 与 `cargo test --lib` 在本机未实跑；P0/P1 各项的"全绿"需在接入时验证。
3. 目标服务器 MySQL 是否支持 `partial_revokes`（P2 审计分权的前置）`[待核实]`。
4. 财务机（47.112.16.98）是否有采集端/告警接收器——S01 文档只覆盖测试机 `[待核实]`。
5. `docker HEALTHCHECK` 是否在所有部署路径（蓝绿正常启动、财务机脚本）都生效 `[待核实]`。
6. `docs/architecture/benchmarks/*.json` 的实际测量值与 `docs/contracts/SLO.md` 的 p95<500ms 是否对齐 `[待核实]`。
7. 参考项目 `br-db` auto-DDL 的 DROP COLUMN 行为来自 docs.rs 测绘，本仓库无可验证源码 `[待核实]`。
8. `feishu_approval_worker.rs` 当前 9,081 行中 `#[cfg(test)]` 起始行 4906 与生产段 4,905 行的拆分口径 `[代码]`，但"拆分后能否过 800 行阈值"未评估 `[待核实]`。
9. 现网 `approval_scan_interval_seconds` 实际取值（决定 180s 看门狗是否已在误报）`[待核实]`。

（完）
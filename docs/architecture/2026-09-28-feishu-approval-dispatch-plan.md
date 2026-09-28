# 飞书审批实例自动创建与编号回填 实施计划

> **执行进度（分支 `feat/feishu-approval-dispatch`）**
>
> | 任务 | 状态 | 提交 |
> |---|---|---|
> | Task 1 `outbound.rs` 扩表 | ✅ 完成 | `75d8358` |
> | Task 2 三张表与 Schema | ✅ 完成 | `367c275` |
> | Task 3 `uuid` 派生 | ✅ 完成 | `ae8f288` |
> | Task 4 控件值转换器 | ✅ 完成 | `9013d85` |
> | Task 5 多维表格写路径 | ✅ 完成 | `a89529c` |
> | Task 6 审批 API 客户端 | ✅ 完成 | `8450281` |
> | Task 7 单条处理编排（含 60012 回捞） | ✅ 完成 | `cbfe626` |
> | Task 8 分批回写与毒记录隔离 | ✅ 完成 | `a48746c` |
> | Task 9 Redis 令牌桶限速 | ✅ 完成 | `5cc91aa` |
> | Task 10 dispatch Action 与路由 | ✅ 完成 | `67008f5` |
> | Task 11 后台 worker | ✅ 完成 | `f9b1723` |
> | Task 12 审计与可观测性 | ✅ 完成 | `2e1a007` |
> | Task 13 控制台配置页 | ⚠️ 范围受阻（见下） | |
> | Task 14 文档同步 | ✅ 完成 | `f1046fb` |
>
> **实机联调准备（用户追加需求，不在原计划里）**
>
> | 事项 | 状态 | 提交 |
> |---|---|---|
> | 按名自动匹配（`approval_match`，15 测试含真实响应形状） | ✅ 完成 | `bcca0f0` |
> | 首次派发自动建配置（`approval_provision` + dispatch 接线） | ✅ 完成 | `ea29217` |
> | 记录字段按列名重映射 + `records/batch_get` 替掉 record_id 过滤 | ✅ 完成（同上） | `ea29217` |
> | worker 播种待处理任务（队列从无到有） | ✅ 完成 | `984b6fd` |
> | 端到端 mock 实测 | ⬜ 未开始 | |
> | 设计文档同步（把实测结论写回 §5/§6/§12） | ⬜ 未开始 | |
>
> ### 断点（2026-09-28，上下文压缩前）
>
> **代码处于可编译、全绿状态**：`run_ci.py quick` 通过（790 lib 测试 + 597 前端
> 测试 + 架构门禁 + `schema_anchor` 离线对账 + clippy 零告警），最新提交 `984b6fd`。
>
> **下一步（按优先级）**：
>
> 1. **端到端 mock 实测**——用户明确要求「先用模拟数据实测」。目标：喂一份
>    **合成**的审批定义 + 合成列名，走完「自动建配置 → 单条派发 → 回填」，
>    并验证校验失败时**错误写回回填列**。真实定义 `D0557DA6-…` 不能直接用：
>    它含 `connect`/`attachmentV2`/`fieldList`，按设计 §6.2 属「人工值控件」，
>    且 `办公地点(Office location)` 是 `option: []` 且未链接外部数据源
>    ——派生不出选项，会被配置期校验正确拒绝。
> 2. **设计文档同步**：把本轮实测结论写回 §5/§6/§12（见下）。
> 3. （可选）Task 13a 的配置 CRUD——已被自动建配置**取代**，除非要做控制台。
>
> **本轮实测推翻/补充的文档结论**（都要写回设计文档）：
>
> - 官方文档说控件 `name` 「必须以 `@i18n@` 开头」，**实测是服务端解好的可读中文**
>   （`公司名称/Company name`）→ 按名匹配成立（依据见 `approval_match` 模块文档）。
> - `option` 是**多态**字段：缺失 / `null` / 数组 / 对象四种形态（同上）。
> - `externalData.key` 实测是**空字符串**，不能当取数源（同上）。
> - `records/batch_get` 的请求体**没有 `field_names`**（只出现在错误表里）。
> - 响应的 `fields` **按字段名**作键，而配置存 `field_id` → 必须边界重映射。
> - `record_id` 是**响应里的系统字段，不是可过滤字段** → 不能拿它当 `filter`。
> - `isEmpty` 的 `value` 必须是**空数组**。
> - `approvals get` **不返回 `is_external`**（只在 `approvals/search` 里），
>   而 `search_launchable` **只支持 user_access_token**、`tenant_access_token` 用不了
>   ——所以「三方定义」的判据改为「`approvals get` 拿不到 `form`」。
> - 外部选项的绑定登记在**台账表**，与派发表的 `field_id` 毫无关系，
>   只能按**列名**关联。

> ### Task 13 的范围缺口（实现期发现，计划本身漏列）
>
> 计划给 Task 13 列的文件清单**只有前端**，但 `feishu.approval` 目前只有一个
> `dispatch_approval` Action——**没有配置 CRUD**。没有 CRUD，`feishu_approval_config`
> 只能靠裸 SQL 填，控制台页面无从建起。
>
> 补齐需要**先**做的后端（计划未列）：
>
> 1. `approval` module 的配置 CRUD Action：创建 / 更新 / 删除 / 列表（四个，
>    每个一文件，走 `feishu.datasource.write` 同档权限）；
> 2. 字段映射的批量 upsert（一次请求写多条映射，与既有 `upsert_options` 同形态）；
> 3. 控件列表 Action：按配置调 `approvals get` 拉控件结构，供映射表单选控件并显示
>    `required`；**这一项正是设计 §6.1「保存期校验」的落地点**，也是为什么配置
>    保存必须经 Action 而不是直接写库；
> 4. 上面三个 Action **会进入 OpenAPI 契约**（受保护 Action，不依赖 `can_pull()`），
>    于是 Task 10 的「不进契约」结论只对 `dispatch` 本身成立——前端可复用生成类型，
>    `dispatch` 仍要手写类型。
>
> Task 13 因此拆成两段：**13a 后端配置 CRUD**（上面三项）与 **13b 前端页面**。
> 13a 未排期，13b 依赖 13a。
>
> **实现期对计划的修正**（都已落进代码注释）：
>
> - **Task 3 的 `uuid` 派生改用 `sha2` 手工格式化**，不用 UUIDv5：`uuid` crate 只开了
>   `v4` feature，加 `v5` 会改 `Cargo.lock` 并触发 MSRV 冷缓存验证。版本位标 `v8`
>   （自定义派生，不属于 RFC 定义的任何版本）。
> - **Task 2 的三张表不进 `infrastructure_definitions()`**（那是运行支撑表专用的定长
>   数组，`:296-309` 断言 9 个表名），走 addon 的 `ModuleSpec.table` 路径。启动验证：
>   `schema 同步 changes=3`，三张表创建成功。
> - **Task 4 新增三处显式拦截**（计划里只提了一处）：不支持的控件类型、**转换器与控件
>   类型不匹配**、未知转换器标识。第二处是测试逮到的真实缺陷——给 `date` 控件配
>   `direct` 会把原始毫秒时间戳当字符串发出去，飞书报 `1390001`，排查方向指向飞书而
>   不是这份配置。
> - **Task 4 的时区用固定偏移**，不引入时区数据库；RFC3339 用 Howard Hinnant 的
>   `civil_from_days` 整数算法（对负偏移与闰日都正确）。
> - **日期区间与单点日期的单元格形态不同**，必须先按控件类型分流再解析——顺序反了会
>   让区间单元格先撞上「期望毫秒时间戳」（测试逮到过）。
> - **本任务新增的模块都带文件级 `#![allow(dead_code)]`**：消费者（派发编排）在后续
>   任务才接入，这是仓库既有惯例（`outbound.rs:32`、`access/groups/table.rs`）。每处
>   都注明消费者落在哪个任务。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让多维表格里的一行业务数据由本系统自动创建对应飞书原生审批实例，并把 `serial_number` 回填到该行；创建失败时把可行动的失败原因写回同一字段，人工清空即可重试。

**Architecture:** 两个多维表格按钮（行内 `buttonField` 同步处理单条 / 页面 `buttonElement` 异步处理全表）打同一个 `public` 端点。系统用三张自有表承载配置、字段映射与**认领队列**——「是否已处理」的判据落在库里而不是多维表格字段上（使用方选择不加闸门字段，见 spec §6.3、§11 限制 1）。创建走 `uuid` 服务端幂等，`uuid` 由 `(base_token, table_id, approval_code, record_id)` 确定性派生为规范 UUID 形态并**持久化**，因为它是 `60012` 响应丢失时唯一的对账键。

**部署形态是单实例**（既有决策 A10，`src/infrastructure/feishu_pull.rs:8-13`：`cmd_cutover` 在 `deploy/deploy-blue-green.sh:1152` **先 `stop_pair` 停线上再起新的**，两实例不重叠）。因此**不做跨实例单飞、不做表级互斥锁**——照抄 `feishu_pull` 的做法：后台只有一个单线程循环，同一时刻只可能有一轮在跑；手动触发只往循环的 `watch`/`mpsc` 通道投一个信号（`feishu_pull.rs:24-32`），因此不需要任何锁。这也与「全仓无进程级锁/租约原语」的现状一致。

创建速率由 Redis 令牌桶限速 90/分钟（令牌桶仍放 Redis 而非进程内存：这是**成本控制**而非互斥——进程内桶重启即清零，会让重启后瞬间打爆飞书配额）。

**Tech Stack:** Rust 2021（MSRV 1.80）、axum（经 `yang-base` transport-axum）、sqlx (MySQL 8)、Redis 7、声明式 Schema（**无 SQL 迁移文件**）、React 19 + Vitest。

**Spec:** `docs/architecture/2026-09-28-feishu-approval-dispatch-design.md`

## Global Constraints

- **声明式 Schema，禁止 SQL 迁移文件**：`docs/contracts/SCHEMA.md:3` 明确本仓库不维护版本化迁移。新表只经 `TableSpec`/`fields!` 声明，启动时增量同步。
- **schema_sync 只增不删**：永不删除表、列、索引或约束（`docs/contracts/SCHEMA.md:12,30`）。外键规则被框架硬编码为 `RESTRICT` 且不可改（`crates/yang-base/src/table/definition.rs:628-655`）——**外键一旦声明即永久存在**。→ 本计划的三张表**不互相声明外键**，用 `config_id` 整数列 + 应用层校验。
- **一 Module 一表**：`ModuleSpec.table` 类型为 `Option<TableSpec>`（`crates/yang-base/src/definition/spec.rs:492,527`），`src/addon/feishu/mod.rs:41-43` 的注释明确写着「**一张表 = 一个 module 是框架的硬形状**」。既有做法是「只声明绑定表、不带 Action」的独立 module（`datasource::build_field_module()`）——本计划照抄该模式，三张表 = 三个 module，其中两个是纯绑定 module。
- **一 Action 一文件**，形态为「自包含 register」：恰好一个 `pub(super) async fn handle` + 一个 `pub(super) fn register(module, feishu)`；`actions/mod.rs` 只保留 `mod` 声明与 `ACTIONS` 数组。禁止在业务代码用 `#[derive(Action)]`。改完必须跑 `python scripts/check_architecture.py`。
- **生产代码禁止 `unsafe`、`unwrap()`、`expect()`**（`Cargo.toml` 设 `unwrap_used`/`expect_used` 为 deny）。`#[cfg(test)]` 同样受约束：**不得**用 `.expect(..)` 及其"取错误值"变体，取错误值一律用 `match` 或 `unwrap_or_else(|e| panic!(..))`。
- **不做表级互斥锁**：部署是单实例（决策 A10），后台只有一个单线程循环。手动触发（页面按钮）只往循环的通道投信号，由循环在同一个 `select!` 里接住——与 `feishu_pull.rs:24-32` 的手动触发**完全同一模式**。全仓无任何进程级锁/租约原语，不要为此新造一把。并发点击同一行由 `feishu_approval_task.record_id` 唯一索引 + `FOR UPDATE` 认领挡住。
- **令牌桶放 Redis**：注意这是**成本控制**而非互斥——进程内桶重启即清零，会让重启后瞬间打爆飞书 100 次/分钟的配额。
- **错误码字面量只能出现在 `outbound.rs`**：该文件自述「本文件是**唯一**出现飞书错误码字面量的地方」（`src/addon/feishu/domain/outbound.rs:38-40`）。审批域的码（`1395001`、`60012`、`1390001`、`1390013`、`1390015`、`1390003`）**必须加在那里**，不得散落在 approval 模块。
- **`outbound.rs` 的既有行为是已交付契约**：`outbound.rs:546-726` 有十余条单测锁定 bitable 路径的分类口径，尤其 `:649-658` 锁「官方建议重试的码即使 HTTP 400 也要 Retry」、`:813-819` 锁「非幂等请求永不重试」。扩表时这些断言**不得修改**，只加不改。
- **创建调用必须显式置 `idempotent: true`**（spec §7.3）。`outbound.rs:463` 在 `idempotent == false` 时对 Retry 类失败直接 `Give`，标 false 等于零重试层。
- **`uuid` 派生用已有的 `sha2`，不要动 `uuid` crate 的 feature**。`Cargo.toml:27` 的 `uuid = { version = "=1.12.1", features = ["v4"] }` **只开了 v4**，用 UUIDv5 需要加 `v5` feature（拉 `sha1`），会改 `Cargo.lock` 并触发 MSRV 冷缓存验证。改用 `sha2`（`Cargo.toml:23` 已依赖）取摘要前 16 字节手工格式化成 UUID 形态——同样确定性、同样规范形态、同样是 SHA 系（不是 MD5，不触发 `pnpm audit`），零依赖变更。
- **落库时序**（spec §5.2，根因 D 的修正，不可省）：`create` **之前**写 `state=creating` + `uuid`；`create` 成功后**立即**落 `instance_code`（`state=created`）；回写成功后才 `state=backfilled`。
- **回写分批 ≤100 条**（spec §7.1），不得把 search 的一整页 500 条一次提交。写侧串行（`bitable-overview.md:29` 官方建议）。
- **错误信息写回多维表格前必须裁剪**（spec §9.2）：带控件 id / 字段名级定位，**不得**原样回灌飞书响应体。
- **`base_token`/`table_id` 必须白名单校验**（spec §9.1）：只允许落在已配置的 `feishu_approval_config` 行内。
- **文档与注释一律中文**；提交遵循 Conventional Commits。
- **门禁**：每任务结束跑 `python scripts/run_ci.py quick`；涉及真实 MySQL/Redis 的行为补 `run_ci.py integration`（测试库名以 `_test` 结尾、Redis DB 15、`--test-threads=1`）。
- **CI 无飞书凭据**：所有需要真实飞书的验证进 spec §12 的待实测清单，**不得**写成会静默跳过的集成测试来假装覆盖。

## Review Focus

以下是 spec 隐含、但没有任务的测试会覆盖到的输入类别与失败模式。它们最可能在实际使用时咬人，每项都在归属任务的步骤里配了对应测试。

1. **`60012` 回捞路径本身失败**。`create` 返回 `60012` → 按 uuid `GET` 反查，但反查可能返回 `1390003`（实例确实不存在，说明是并发窗口）或网络失败。若把回捞失败直接落终态，就把「实例已存在」的本来可恢复状态写成了错误文案——而写进回填字段是**不可逆的扫描闸门**。合理预期：`1390003` 回到 `create` 重试（小退避、有限次），网络失败走可重试；只有重试耗尽才落终态。
2. **回写失败的子批里混着已成功和未成功的记录**。`batch_update` 全有全无语义，任一坏记录让整子批零条落库。合理预期：折半拆批直到定位毒记录，其余记录的状态是 `backfilled` 而非 `created` 悬置，且毒记录被单独置为可人工处理的失败态、不再每轮楔住整批。
3. **用户在多维表格里把回填字段手动改成非空**。使用方选择不加闸门字段，那么「审批编号」字段就是唯一的输出位。若有人手工往里填了东西，`isEmpty` 过滤会把它排除，但**库里没有 task 行的记录**仍应被扫到（扫描键是库表，不是字段）。合理预期：扫描集合由库表定义（spec §6.3），手动改字段不影响认领，且不会产生重复实例（`uuid` 幂等兜底）。
4. **必填缺失的记录每轮都被重算**。spec §6.3 明确把「不完整」建模为等待态（不写字段），代价是该记录每轮都被重新拉出。合理预期：这类记录不产生 API 调用（本地校验就拦下）、只增加扫描开销，且系统侧有计数可观测。
5. **令牌桶在蓝绿期间被新旧实例各自持有一份**。spec §7.7 要求令牌桶放 Redis 而非进程内存。合理预期：跨实例共享配额，创建速率不会因双实例翻倍。
6. **`is_external` 与非支持控件的前置校验把好配置拦下**。spec §12 的 M4 未实测，校验的确切判据未知。合理预期：校验失败时报错文本指出具体控件 id 与类型，让使用方能对照审批定义修正，而不是笼统报「配置非法」。

---

## 文件结构

### 新建

| 文件 | 职责 |
|---|---|
| `src/addon/feishu/approval/mod.rs` | `feishu.approval` 模块装配：三张表、Action 注册表、dispatch 路由 |
| `src/addon/feishu/approval/table.rs` | `feishu_approval_config` 表声明 |
| `src/addon/feishu/approval/actions/mod.rs` | Action 注册表（`ACTIONS` 数组） |
| `src/addon/feishu/approval/actions/dispatch.rs` | dispatch 端点（单条同步 / 全表异步分流） |
| `src/addon/feishu/domain/approval.rs` | 审批 API 客户端：`instances create` / `instances get` / `approvals get` |
| `src/addon/feishu/domain/approval_dispatch.rs` | 单条处理编排（校验 → 转换 → create → get → 回写） |
| `src/addon/feishu/domain/approval_convert.rs` | 控件值转换器（`direct`/`date`/`option`） |
| `src/addon/feishu/domain/approval_uuid.rs` | `uuid` 派生（UUIDv5） |
| `src/infrastructure/feishu_approval_worker.rs` | 后台 worker：认领 + 限速 + 批量处理 |

> **`domain/` 的边界**：机制代码进 `domain/`，业务用例流程进 Action。`approval.rs` 是出站客户端（机制），`approval_dispatch.rs` 是编排（机制，被 Action 与 worker 共用），`approval_convert.rs` 是纯函数（易测），`approval_uuid.rs` 是纯函数。

### 修改

| 文件 | 改动 |
|---|---|
| `src/addon/feishu/mod.rs` | 注册 `approval` module（参考现有 `datasource`/`option` 的装配，`mod.rs:27-48`） |
| `src/addon/feishu/domain/outbound.rs` | **扩表**：审批域可重试集合、`60012` 独立类别、`1390003`、`fatal_hint` 文案 |
| `src/addon/feishu/domain/bitable.rs` | 新增写路径 `records/batch_update`（现有只有 GET 类） |
| `src/addon/feishu/domain/mod.rs` | 导出新模块 |
| `src/infrastructure/schema.rs` | 运行支撑表断言同步（`:47-56` 与 `:289-303`） |
| `src/infrastructure/mod.rs` | 导出新 worker |
| `src/config/mod.rs` | `FeishuSettings` 加 4 项（`:568-626`） |
| `src/config/source.rs` | 对应绑定 |
| `src/bootstrap.rs` | 注册 worker（抄 `feishu_pull` 的注册点） |
| `docs/contracts/CONFIGURATION.md` | 新增配置项 |
| `docs/contracts/AUDIT.md` | 新增审计事件（若该契约按事件逐个登记） |
| `AGENTS.md` | 新增 Action/worker/module 的事实同步 |

---

## Task 1: `outbound.rs` 扩表（审批域错误码）

**这一步不改既有行为、不依赖其他任务，可以先合。**

**Files:**
- Modify: `src/addon/feishu/domain/outbound.rs`

**Interfaces:**
- Consumes: 既有 `FailureKind`、`classify`、`disposition`、`fatal_hint`
- Produces: `CODES_RETRYABLE_APPROVAL: &[i32]`、`CODE_UUID_CONFLICT: i32`、`CODE_INSTANCE_NOT_FOUND: i32`、`FailureKind::UuidConflict`

- [ ] **Step 1: 先写失败测试（红）**

在 `outbound.rs` 的测试模块末尾追加。**注意**：这条测试必须显式列举设计要求的码，不能只遍历常量表本身——现有 `:649-658` 的遍历式断言挡不住「设计声明可重试、常量表没登记」这类漂移。

```rust
/// 审批域码的分类口径。显式列举，避免"常量表自证"式的空转断言。
#[test]
fn approval_domain_codes_are_classified_as_designed() {
    let empty = BTreeMap::new();

    // 1395001：官方明文「降低请求频率，并重试」（instance/create.md:115）。
    // 它常以 HTTP 400 返回，所以必须走业务码分支而非 429 分支。
    assert!(matches!(
        classify(400, &empty, "{\"code\":1395001,\"msg\":\"there have been some errors\"}"),
        Some(FailureKind::Retry { .. })
    ));

    // 60012：uuid 冲突 = 实例已存在，不是失败。绝不能落 Fatal。
    assert!(matches!(
        classify(400, &empty, "{\"code\":60012,\"msg\":\"uuid conflict\"}"),
        Some(FailureKind::UuidConflict)
    ));

    // 1390003：实例不存在（回捞时用于区分"并发窗口"与"真的没建"）。
    assert!(matches!(
        classify(400, &empty, "{\"code\":1390003,\"msg\":\"instance code not found\"}"),
        Some(FailureKind::InstanceNotFound)
    ));

    // 1390001 / 1390013 / 1390015：终态，且必须给出可行动文案。
    for code in [1390001, 1390013, 1390015] {
        let body = format!("{{\"code\":{code},\"msg\":\"x\"}}");
        match classify(400, &empty, &body) {
            Some(FailureKind::Fatal { code: got }) => {
                assert_eq!(got, code);
                assert!(fatal_hint(code).is_some(), "码 {code} 缺少 fatal_hint 文案");
            }
            other => panic!("码 {code} 期望 Fatal，实际 {other:?}"),
        }
    }
}

/// 幂等请求的 Retry 必须真的退避重试。与既有 `:813-819` 的反向断言成对。
#[test]
fn idempotent_requests_do_retry_on_retryable_failure() {
    let policy = RetryPolicy::default();
    assert!(matches!(
        disposition(
            FailureKind::Retry { retry_after_seconds: None },
            1,
            true,
            policy
        ),
        Disposition::RetryAfter(_)
    ));
}
```

- [ ] **Step 2: 运行测试确认失败**

```powershell
cargo test --lib --locked outbound::tests::approval_domain_codes
cargo test --lib --locked outbound::tests::idempotent_requests_do_retry
```

预期：编译失败（`CODES_RETRYABLE_APPROVAL` 等未定义 / `FailureKind::UuidConflict` 变体不存在）。

- [ ] **Step 3: 加常量与枚举变体**

在 `outbound.rs` 的「飞行契约常量」区（`CODES_RETRYABLE` 附近，约 `:74`）追加：

```rust
/// 审批域瞬态错误。官方排查建议原文即「降低请求频率，并重试」（`instance/create.md:115`）。
///
/// **这不是频控码**——真正的频控是 `CODE_RATE_LIMITED` / `CODE_TOO_MANY_REQUEST`，
/// 由 429 或业务码分支处理。`1395001` 是审批服务自身的瞬态错误，常以 HTTP 400 返回。
///
/// 与 `CODES_RETRYABLE` 并列判断而不是合并：后者是多维表格域的语义，
/// 混在一起会让「这张表覆盖哪些域」变得不可读。
pub(crate) const CODES_RETRYABLE_APPROVAL: &[i32] = &[1395001];

/// uuid 冲突：该 uuid 已经创建过审批实例。
///
/// **语义是「响应丢失、实例已存在」，不是失败**（`instance/create.md:43`）。
/// 处置是用 uuid 反查实例详情取回 `instance_code` 与 `serial_number` 后继续，
/// 所以**不能**落 `Fatal`——`Fatal` 在本模块的含义是「配置/权限错了，别重试」，
/// 与这里要做的「一次查询后继续」正好相反。
pub(crate) const CODE_UUID_CONFLICT: i32 = 60012;

/// 实例不存在。回捞 uuid 时用它区分「并发窗口，实例确实还没建」与「实例已建但查询失败」。
pub(crate) const CODE_INSTANCE_NOT_FOUND: i32 = 1390003;
```

在 `FailureKind` 枚举中追加两个变体：

```rust
/// uuid 冲突：实例此前已创建成功，只是响应丢了。交上层按 uuid 反查，不重试、不写字段。
UuidConflict,
/// 实例不存在：回捞时用来区分并发窗口与真实未创建，交上层决定是否回到 create。
InstanceNotFound,
```

- [ ] **Step 4: 接入 `classify`、`disposition`、`fatal_hint`**

在 `classify` 的业务码分支里，**插在 `outbound.rs:275` 的 `Fatal` 兜底之前**：

```rust
} else if CODES_RETRYABLE_APPROVAL.contains(&code) {
    return Some(FailureKind::Retry {
        retry_after_seconds: rate_limit_reset_seconds(headers),
    });
} else if code == CODE_UUID_CONFLICT {
    return Some(FailureKind::UuidConflict);
} else if code == CODE_INSTANCE_NOT_FOUND {
    return Some(FailureKind::InstanceNotFound);
} else {
```

`disposition`：两个新变体都 `Give`（与 `TokenExpired`/`Fatal` 同分支），语义是「不重试，交上层处置」。在既有 `FailureKind::TokenExpired | FailureKind::Fatal { .. } => Disposition::Give` 这一行加上两个变体。

`fatal_hint`：补三条文案（现状对它们返回 `None`，运维只看到裸错误体）：

```rust
1390001 => Some("表单控件参数错误：用 approvals/get 核对控件 id/type 与取值形态"),
1390013 => Some("不支持自定义审批流程"),
1390015 => Some("审批定义已停用：去审批管理后台启用后重试"),
```

- [ ] **Step 5: 运行测试确认通过 + 既有测试全绿**

```powershell
cargo test --lib --locked outbound
```

预期：新增两条通过，**既有十余条全部仍然通过**（若既有断言变红，说明扩表动了共享语义，必须回头改成并列判断而不是合并）。

- [ ] **Step 6: 门禁与提交**

```powershell
python scripts/run_ci.py quick
git add -A
git commit -m "feat(feishu): outbound 扩审批域错误码表，60012 单列幂等命中类别"
```

---

## Task 2: 三张表与 Schema 断言

**Files:**
- Create: `src/addon/feishu/approval/mod.rs`、`src/addon/feishu/approval/table.rs`
- Create: `src/addon/feishu/approval/domain/task_table.rs`、`src/addon/feishu/approval/domain/field_map_table.rs`
- Modify: `src/addon/feishu/mod.rs`、`src/infrastructure/schema.rs`

**Interfaces:**
- Produces: `feishu_approval_config`、`feishu_approval_field_map`、`feishu_approval_task` 三张表定义

- [ ] **Step 1: 读现有表声明的写法**

```powershell
python -c "print(open('src/addon/feishu/datasource/table.rs',encoding='utf-8').read())"
```

对照 `src/addon/feishu/datasource/table.rs:10-78` 与 `src/addon/feishu/option/table.rs:16-100` 的 `TableSpec` + `fields!` 用法，**照抄风格**（注释密度、字段命名、索引声明方式）。

- [ ] **Step 2: 声明三张表**

按 spec §5.1 与 §5.2 的字段表声明。**三处容易写错的地方**：

1. `feishu_approval_task.record_id` 与 `uuid` 各自加**唯一索引**（认领去重靠它）。
2. `state` 用 `Radio` 类型（枚举），值是 `pending`/`creating`/`created`/`backfilled`/`terminal`。
3. **不声明任何外键**——`config_id` 是普通 `Int` + 索引。

- [ ] **Step 3: 表走 addon module 路径，不动 `infrastructure_definitions`**

`src/infrastructure/schema.rs:47` 的 `infrastructure_definitions() -> Result<[TableDefinition; 9], BaseError>` 是**运行支撑表专用**的定长数组，`:296-309` 有 `infrastructure_schema_is_complete_and_versionless` 精确断言 9 个表名。

**本计划的三张表是业务表，不进这个数组**——addon 的表经 `ModuleSpec.table` 贡献（`schema.rs:19` 的 `definitions.extend(infrastructure_definitions()?)` 之外还有 addon 那条路径）。所以 **`infrastructure_definitions()` 与其断言测试都不需要改**。若实现时发现必须改它，说明表放错了层。

一 module 一表（`src/addon/feishu/mod.rs:41-43` 的硬形状注释），照抄 `datasource::build_field_module()` 的「只声明绑定表、不带 Action」模式：

- `approval::build_module(...)` — `feishu_approval_config`，带 Action（dispatch）
- `approval::build_field_map_module()` — `feishu_approval_field_map`，只声明绑定表
- `approval::build_task_module()` — `feishu_approval_task`，只声明绑定表

三张表由 `FeishuContext` 跨表访问（`mod.rs:25-26` 的既有说明：`Registry::dispatch` 只向 Action 注入所在 module 的主表，其余表经 context 共享）。**照抄 `build_context` 的构造模式**（`mod.rs:51-59` 说明了为何两处必须共用同一个表定义构造函数——各写一遍会在加列时漏改一处）。

- [ ] **Step 4: 启动验证 schema 同步**

```powershell
# 需要本地 MySQL/Redis 已起（compose.yaml）
cargo run --locked
```

预期：启动日志显示三张新表已创建；再跑一次应显示无变更（幂等）。**若报冲突，检查是否与既有表名/列名撞车**。

- [ ] **Step 5: 门禁与提交**

```powershell
python scripts/run_ci.py quick
git commit -am "feat(feishu): 审批派发三张表（配置/字段映射/认领队列）"
```

---

## Task 3: `uuid` 派生（纯函数）

**Files:**
- Create: `src/addon/feishu/domain/approval_uuid.rs`
- Modify: `src/addon/feishu/domain/mod.rs`

**Interfaces:**
- Produces: `pub(crate) fn derive_uuid(base_token: &str, table_id: &str, approval_code: &str, record_id: &str) -> String`

- [ ] **Step 1: 写失败测试（红）**

```rust
#[test]
fn uuid_is_canonical_form() {
    let u = derive_uuid("appbcbWCzen6", "tblsRc9GRRX", "4202AD96-9EC1", "recqwIwhc6");
    // 官方建议形态 XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX（instance/create.md:43）
    assert_eq!(u.len(), 36);
    assert_eq!(u.matches('-').count(), 4);
    assert!(u.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
}

#[test]
fn uuid_is_stable_across_calls() {
    let a = derive_uuid("b", "t", "c", "r");
    let b = derive_uuid("b", "t", "c", "r");
    assert_eq!(a, b, "同输入必须同输出，否则幂等失效");
}

#[test]
fn uuid_differs_across_tables_for_same_record_id() {
    // record_id 只在单个多维表格内唯一（bitable-overview），全局可能撞车。
    // 不同 (base, table) 下的同一 record_id 必须得到不同 uuid。
    let a = derive_uuid("base1", "tbl1", "code", "rec_same");
    let b = derive_uuid("base2", "tbl2", "code", "rec_same");
    assert_ne!(a, b);
}

#[test]
fn uuid_differs_across_approval_codes() {
    // 换审批定义要能重发（spec §11 限制 4）。
    let a = derive_uuid("b", "t", "code_a", "r");
    let b = derive_uuid("b", "t", "code_b", "r");
    assert_ne!(a, b);
}
```

- [ ] **Step 2: 运行确认失败**

```powershell
cargo test --lib --locked approval_uuid
```

预期：编译失败（函数不存在）。

- [ ] **Step 3: 实现**

用**已有的 `sha2`** 取摘要前 16 字节，手工格式化成 UUID 形态。**不要用 UUIDv5**——那需要给 `uuid` 加 `v5` feature（`Cargo.toml:27` 只开了 `v4`），会改 lock 并触发 MSRV 冷缓存验证。

```rust
use sha2::{Digest, Sha256};

/// 由多维表格坐标与审批定义派生稳定 uuid。
///
/// 派生键必须包含 **(base_token, table_id)**：`record_id` 只在单个多维表格内唯一
/// （`bitable-overview.md:152`），全局可能撞车。只用 record_id 会让两张表的
/// 同 id 记录在首次创建时就吃 60012 并静默丢单。
///
/// 必须包含 **approval_code**：使用方换审批定义后，同一行应能作为新单子重发
/// （spec §11 限制 4）。
///
/// 输出是规范 UUID 形态（`instance/create.md:43` 的建议格式），取 SHA-256 前 16 字节
/// 并按 RFC 4122 的变体/版本位打标。用 SHA-256 而非 MD5：CI 有 `pnpm audit`。
pub(crate) fn derive_uuid(
    base_token: &str,
    table_id: &str,
    approval_code: &str,
    record_id: &str,
) -> String {
    let name = format!("{base_token}|{table_id}|{approval_code}|{record_id}");
    let digest = Sha256::digest(name.as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&digest[..16]);
    // 版本位（自定义 v8：本派生不属于 RFC 定义的任何版本）与变体位（RFC 4122）。
    b[6] = (b[6] & 0x0f) | 0x80;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13],
        b[14], b[15]
    )
}
```

> `b.copy_from_slice` 不会 panic（两段长度都是 16，编译期已知）；但仓库 `unwrap_used`/`expect_used` 是 deny，索引访问是安全的，不要改写成会 panic 的写法。

- [ ] **Step 4: 运行确认通过**

```powershell
cargo test --lib --locked approval_uuid
```

- [ ] **Step 5: 门禁与提交**

```powershell
python scripts/run_ci.py quick
git commit -am "feat(feishu): 审批 uuid 派生（UUIDv5，含表坐标与定义码）"
```

---

## Task 4: 控件值转换器（纯函数）

**Files:**
- Create: `src/addon/feishu/domain/approval_convert.rs`
- Modify: `src/addon/feishu/domain/mod.rs`

**Interfaces:**
- Produces: `pub(crate) fn build_form(snapshot: &FormSnapshot, map: &[FieldMap], cells: &RecordCells, tz: &str) -> Result<String, ConvertError>` —— 返回**压缩转义的 JSON 数组字符串**（`instance/create.md:30` 要求 `form` 是字符串而非对象）

- [ ] **Step 1: 写失败测试（红）**

覆盖 spec §6.2 表格里的每一种类型。至少：

```rust
#[test]
fn input_and_textarea_are_plain_strings() { /* value 是字符串 */ }

#[test]
fn number_is_number_not_string() { /* JSON 里不能是 "4" */ }

#[test]
fn date_is_rfc3339_with_offset() {
    // 多维表格给毫秒时间戳，审批要 RFC3339 + 偏移量。
    // 时区由配置显式给定，不猜。
}

#[test]
fn radio_uses_option_value_not_label() {
    // 选项文案与审批控件 option value 不一定同名，必须走 option_map。
}

#[test]
fn checkbox_is_option_value_array() { }

#[test]
fn contact_writes_open_ids() { }

#[test]
fn amount_carries_currency() { }

#[test]
fn missing_required_widget_is_reported_with_widget_id() {
    // 错误必须能定位到具体控件 id，否则使用方无从修正（spec §9.2）。
}

#[test]
fn output_is_compressed_json_array_string() {
    // form 是字符串，且必须是数组形态（instance/create.md:30 示例）。
    let s = build_form(/* ... */).unwrap_or_else(|e| panic!("{e:?}"));
    assert!(s.starts_with('['));
    let parsed: serde_json::Value = serde_json::from_str(&s).unwrap_or_else(|e| panic!("{e}"));
    assert!(parsed.is_array());
}
```

- [ ] **Step 2: 运行确认失败**

```powershell
cargo test --lib --locked approval_convert
```

- [ ] **Step 3: 实现**

按 `~/.claude/skills/lark-approval/references/lark-approval-instance-form-control-parameters.md` 的结构逐类型实现。**关键约束**：

- `approvals/get` 的 `form` **不是可直接提交的模板**（`lark-approval-initiate.md:10`），每个控件的 `value` 要按类型重新组装。
- 三类**不自动准备**的值（`lark-approval-instance-value-sourcing.md:88-97`）：`address` 的地理库 `id`、`connect` 的 `instance_code`、`attachmentV2`/`image` 的 file code。遇到这些控件，**返回明确的 `ConvertError`**，不要静默传空。
- 缺少必填值时返回的错误要带**控件 id**。

- [ ] **Step 4: 运行确认通过**

```powershell
cargo test --lib --locked approval_convert
```

- [ ] **Step 5: 门禁与提交**

```powershell
python scripts/run_ci.py quick
git commit -am "feat(feishu): 审批表单控件值转换器（含 date/option 与时区显式化）"
```

---

## Task 5: 多维表格写路径

**Files:**
- Modify: `src/addon/feishu/domain/bitable.rs`

**Interfaces:**
- Produces: `pub(crate) async fn batch_update_records(...) -> Result<...>`，以及 `pub(crate) async fn search_records(...)`（带 filter）

- [ ] **Step 1: 确认现状**

```powershell
python -c "import re;print(''.join(l for l in open('src/addon/feishu/domain/bitable.rs',encoding='utf-8') if 'open-apis' in l))"
```

预期：现有端点全是 GET（records/tables/fields/views），**没有 search、没有写**。该文件头注释说明了为什么没迁到 search 接口 —— 先读懂再动手。

- [ ] **Step 2: 写失败测试（红）**

用 mock transport（既有 `OutboundTransport` trait 可替换，见 `src/addon/feishu/domain/outbound.rs`）：

```rust
#[test]
fn batch_update_sends_post_with_records_array() {
    // 断言 URL 是 .../records/batch_update、method 是 POST、
    // body 是 {"records":[{"record_id":..,"fields":{..}}]}
}

#[test]
fn batch_update_is_marked_idempotent() {
    // 同值覆盖是幂等写，必须标 idempotent:true，否则 429/5xx 一次都不重试。
}

#[test]
fn search_uses_field_id_not_field_name_in_filter() {
    // filter 条件用 field_id，避免用户改列名后静默失配（spec §5.1）。
}
```

- [ ] **Step 3: 实现**

`batch_update`：`POST .../records/batch_update`，单批最多 1000（`batch_update.md:3`），但**本设计的分批上限是 100**（spec §7.1）——分批逻辑放在调用方（Task 8），本函数只负责发一批。

`search`：`POST .../records/search`，支持 `filter` 与 `field_names` 投影。注意该接口**单次最多 500 行**、20 次/秒（`search.md:3,15`）。

两个函数都要**标 `idempotent: true`**（回写是同值覆盖；search 是只读）。

- [ ] **Step 4: 运行确认通过**

```powershell
cargo test --lib --locked bitable
```

- [ ] **Step 5: 门禁与提交**

```powershell
python scripts/run_ci.py quick
git commit -am "feat(feishu): bitable 新增 records/search 与 batch_update"
```

---

## Task 6: 审批 API 客户端

**Files:**
- Create: `src/addon/feishu/domain/approval.rs`
- Modify: `src/addon/feishu/domain/mod.rs`

**Interfaces:**
- Consumes: Task 1 的 `CODES_RETRYABLE_APPROVAL`、`CODE_UUID_CONFLICT`、`CODE_INSTANCE_NOT_FOUND`、`FailureKind::{UuidConflict,InstanceNotFound}`
- Produces:
  - `pub(crate) async fn create_instance(outbound, tenant_token, approval_code, form, open_id, uuid) -> Result<String, CreateOutcome>`，其中 `pub(crate) enum CreateOutcome { Created { instance_code: String }, UuidConflict }`
  - `pub(crate) async fn get_instance(instance_id_or_uuid) -> Result<InstanceDetail, GetOutcome>`，`InstanceDetail { instance_code, serial_number: Option<String>, status }`，`GetOutcome::NotFound`
  - `pub(crate) async fn get_approval_definition(approval_code) -> Result<FormSnapshot, ...>`

- [ ] **Step 1: 写失败测试（红）**

```rust
#[test]
fn create_sends_form_as_json_string_not_object() {
    // instance/create.md:30 明文：form 是「JSON 数组，传值时需要压缩转义为字符串」。
    // 传成对象会被 1390001 拒掉。
}

#[test]
fn create_marks_request_idempotent() {
    // 依据 instance/create.md:43：同一 uuid 只能建一个实例。
    // outbound.rs:463 在 idempotent=false 时对 Retry 直接 Give —— 标 false 等于零重试层。
}

#[test]
fn create_maps_60012_to_uuid_conflict_not_error() {
    // 60012 落 FailureKind::UuidConflict，必须映射成 CreateOutcome::UuidConflict，
    // 绝不能冒泡成 Err。
}

#[test]
fn get_maps_1390003_to_not_found() { }

#[test]
fn serial_number_is_none_when_absent() {
    // serial_number 是否随创建立即可查未知（spec §12 M2）。
    // 缺失必须是 None 而不是报错。
}
```

- [ ] **Step 2: 运行确认失败**

```powershell
cargo test --lib --locked approval::
```

- [ ] **Step 3: 实现**

三个函数，请求构造**照抄 `bitable.rs` 的既有写法**（token 在 `send_with_retry` 之前取、失效时调 `invalidate_and_refresh` 重放一次，见 `bitable.rs:680-696`）。

**三处必须写对**：

1. `create_instance` 的 `OutboundRequest.idempotent = true`，并把依据写成注释（uuid 服务端幂等），防止后人按「POST 有副作用」的直觉改回 false。
2. `create_instance` 必须把 `FailureKind::UuidConflict` 单独映射成 `CreateOutcome::UuidConflict`——**不要**走 `Err` 路径。
3. `get_instance` 的路径参数 `:instance_id` **可以直接传 uuid**（`instance/get.md:26` 明文），这是回捞的基础。

- [ ] **Step 4: 运行确认通过**

```powershell
cargo test --lib --locked approval::
```

- [ ] **Step 5: 门禁与提交**

```powershell
python scripts/run_ci.py quick
git commit -am "feat(feishu): 审批 API 客户端（create/get 定义，60012 映射为幂等命中）"
```

---

## Task 7: 单条处理编排（含 60012 回捞）

**这是本计划最核心的任务。** spec §7.5 的回捞路径在这里落地。

**Files:**
- Create: `src/addon/feishu/domain/approval_dispatch.rs`
- Modify: `src/addon/feishu/domain/mod.rs`

**Interfaces:**
- Consumes: Task 2 的三张表、Task 3 的 `derive_uuid`、Task 4 的 `build_form`、Task 6 的 `create_instance`/`get_instance`
- Produces: `pub(crate) async fn dispatch_one(ctx, deps, config, record_id) -> DispatchResult`，`pub(crate) enum DispatchResult { Backfilled { serial_number }, Waiting(String), Terminal(String), Retryable }`

- [ ] **Step 1: 写失败测试（红）**

用 mock 审批客户端与 mock bitable：

```rust
#[tokio::test]
async fn uuid_conflict_retrieves_instance_and_backfills() {
    // 核心路径：create 返回 60012 → 按 uuid GET → 拿到 instance_code+serial_number
    // → 落库 state=created/backfilled → 回写编号。
    // 断言：绝不把 60012 写成错误文案。
}

#[tokio::test]
async fn uuid_conflict_then_not_found_returns_to_create() {
    // 60012 后 GET 返回 1390003（并发窗口）→ 应回到 create 重试（有限次），
    // 不是落终态。见 Review Focus 1。
}

#[tokio::test]
async fn missing_required_leaves_field_untouched() {
    // 必填缺失 → DispatchResult::Waiting，且【不回写任何字段】。
    // 这是 spec §6.3 的核心：用户先填业务字段、后填「申请人」时不能钉死该行。
}

#[tokio::test]
async fn terminal_failure_writes_filtered_message() {
    // 1390001 → 终态，写回的文案必须裁剪过（不含完整响应体）。
}

#[tokio::test]
async fn instance_code_is_persisted_immediately_after_create() {
    // 时序要求（spec §5.2）：create 成功后立即落库，
    // 不等回写成功。断言落库发生在回写之前。
}
```

- [ ] **Step 2: 运行确认失败**

```powershell
cargo test --lib --locked approval_dispatch
```

- [ ] **Step 3: 实现**

流程（严格按 spec §6.4 + §7.5）：

```
1. 先落库 state=creating + uuid（Task 2 的表，uuid 唯一索引）
2. 本地必填校验
     不完整 → DispatchResult::Waiting（不写字段）
3. build_form
4. create_instance
     Created{instance_code} → 立即落库 state=created + instance_code
     UuidConflict           → 【回捞】get_instance(uuid)
                                 ├ NotFound  → 回到 create（有限次、小退避）
                                 └ detail    → 落库 state=created + instance_code + serial_number
     Retryable              → DispatchResult::Retryable
     Fatal                  → DispatchResult::Terminal(裁剪后的文案)
5. serial_number 为空 → get_instance(instance_code)
     1390003 / 空值 → 退避重试（【绝不】写终态，见 spec §6.4）
6. 回写 batch_update（Task 8 负责分批，本函数只处理单条）
     成功 → state=backfilled
```

**回捞实现要点**：`CreateOutcome::UuidConflict` 分支里调 `get_instance(uuid)`——**传 uuid，不是 instance_code**（后者此时不存在）。一次调用同时拿回 `instance_code` 与 `serial_number`。

- [ ] **Step 4: 运行确认通过**

```powershell
cargo test --lib --locked approval_dispatch
```

- [ ] **Step 5: 门禁与提交**

```powershell
python scripts/run_ci.py quick
git commit -am "feat(feishu): 审批单条派发编排，60012 走 uuid 回捞而非终态"
```

---

## Task 8: 分批回写与毒记录隔离

**Files:**
- Modify: `src/addon/feishu/domain/approval_dispatch.rs`

**Interfaces:**
- Produces: `pub(crate) async fn backfill_batch(deps, pairs: &[(record_id, serial_number)]) -> BackfillReport`

- [ ] **Step 1: 写失败测试（红）**

```rust
#[tokio::test]
async fn batch_is_split_at_hundred() {
    // 断言 250 条被拆成 3 批（100/100/50），绝不一次提交 500。
}

#[tokio::test]
async fn failing_chunk_is_halved_to_isolate_poison_record() {
    // 一个子批整体失败（batch_update 全有全无）→ 对半拆 → 定位毒记录
    // → 其余记录仍落 backfilled，毒记录单独落失败态。
    // 见 Review Focus 2。
}

#[tokio::test]
async fn poison_record_does_not_block_later_chunks() {
    // 毒记录不得楔住整批：后续子批照常处理。
}

#[tokio::test]
async fn envelope_failure_is_retryable_not_terminal() {
    // 信封 code 非 0 ⇒ 该子批零条落库，可整体重试（同值覆盖，幂等）。
}
```

- [ ] **Step 2: 运行确认失败**

```powershell
cargo test --lib --locked backfill
```

- [ ] **Step 3: 实现**

- 分批上限 **100**（`const BACKFILL_CHUNK: usize = 100;`），注释写明依据：`batch_update` 单批上限 1000，但官方对 `1254607` 的建议就是降低 page_size；且全有全无语义下批越小、毒记录影响面越小。
- 失败降级：子批失败 → 对半拆递归 → 单条。
- 单条也失败 → 该记录落 `terminal` + 裁剪后的文案。
- 写侧串行（`bitable-overview.md:29`）。
- 重试间隔 0.5–1 秒（`1254291` 的建议）。

- [ ] **Step 4: 运行确认通过**

```powershell
cargo test --lib --locked backfill
```

- [ ] **Step 5: 门禁与提交**

```powershell
python scripts/run_ci.py quick
git commit -am "feat(feishu): 回写分批 100 与毒记录折半隔离"
```

---

## Task 9: Redis 令牌桶限速

**Files:**
- Create: `src/addon/feishu/domain/approval_rate_limit.rs`
- Modify: `src/addon/feishu/domain/mod.rs`

**Interfaces:**
- Produces: `pub(crate) async fn acquire_create_slot(redis, cfg) -> Result<()>`

- [ ] **Step 1: 写失败测试（红）**

```rust
#[tokio::test]
async fn token_bucket_limits_to_configured_rate() {
    // 90/分钟 = 1.5/秒。连取 3 次应至少等待约 1 秒（或第 3 次不可立即取到）。
}

#[tokio::test]
async fn bucket_state_lives_in_redis_not_process() {
    // spec §7.7：蓝绿期间新旧实例并存，进程内桶会让配额翻倍。
    // 断言：两个独立 client 看到同一份桶状态。
}
```

- [ ] **Step 2: 实现**

Redis 令牌桶（Lua 脚本原子取令牌）。速率取 `FeishuSettings.approval_create_rate_per_minute`（默认 90，留 10% 余量于官方的 100）。

**关键理由写进注释**：100 次/分钟是应用级配额（spec §12 M5 未实测是应用级还是租户级），而同一对 `app_id`/`app_secret` 下的所有配置共享这一个桶。

- [ ] **Step 3: 运行确认通过 + 门禁与提交**

```powershell
cargo test --lib --locked approval_rate_limit
python scripts/run_ci.py quick
git commit -am "feat(feishu): 审批创建全局令牌桶（Redis，跨实例共享）"
```

---

## Task 10: dispatch Action 与路由

**Files:**
- Create: `src/addon/feishu/approval/actions/mod.rs`、`src/addon/feishu/approval/actions/dispatch.rs`
- Modify: `src/addon/feishu/approval/mod.rs`

**Interfaces:**
- Produces: `POST /api/v1/feishu/approval/dispatch`

- [ ] **Step 1: 用脚手架起文件**

```powershell
python scripts/new_action.py src/addon/feishu/approval/actions dispatch --title "创建飞书审批实例" --method POST --path /api/v1/feishu/approval/dispatch
```

预期：生成骨架，且稳定返回「尚未实现」错误。**必须补齐强类型输入输出与业务逻辑**。

- [ ] **Step 2: 写失败测试（红）**

```rust
#[tokio::test]
async fn unconfigured_base_table_is_rejected() {
    // spec §9.1：base_token/table_id 必须在已配置行内，否则越权。
    // 这是安全测试，不能省。
}

#[tokio::test]
async fn record_id_present_routes_to_sync_single() {
    // 带 record_id → 同步处理单条 → 返回 serial_number。
}

#[tokio::test]
async fn record_id_absent_routes_to_async_accept() {
    // 不带 → 落任务 → 立即返回 accepted。
}

#[tokio::test]
async fn response_envelope_is_flat_for_workflow_reference() {
    // spec §4.1：response_value 只能引用声明过的字段，响应必须扁平。
}
```

- [ ] **Step 3: 实现**

- `register` 用 `.public()`（跳过框架 JWT）+ `ManagementTokenMiddleware`，**照抄 `src/addon/feishu/option/actions/upsert_options.rs:146`**。
- 分流：`record_id.is_some()` → Task 7 的 `dispatch_one`；否则 → 落任务 + 返回 accepted。
- 响应恒 HTTP 200，`{code, msg, data}` 信封（`approval_options.rs:337-374` 的既有约定）。
- 单条链路有内部处理预算（照抄 `approval_options.rs:50` 的 2.5 秒注释风格；官方 HTTP 节点上限 60 秒，单条 create+get 远在预算内）。

- [ ] **Step 4: 跑架构门禁**

```powershell
python scripts/check_architecture.py
```

预期：通过（一 Action 一文件、恰好一个 `handle` + 一个 `register`、`ACTIONS` 数组一致）。

- [ ] **Step 5: 门禁与提交**

```powershell
python scripts/run_ci.py quick
git commit -am "feat(feishu): 审批派发端点（单条同步 / 全表异步分流）"
```

---

## Task 11: 后台 worker

**Files:**
- Create: `src/infrastructure/feishu_approval_worker.rs`
- Modify: `src/infrastructure/mod.rs`、`src/bootstrap.rs`

**Interfaces:**
- Consumes: Task 7 的 `dispatch_one`、Task 8 的 `backfill_batch`、Task 9 的令牌桶
- Produces: 后台认领循环

- [ ] **Step 1: 抄骨架**

```powershell
python -c "print(open('src/infrastructure/feishu_pull.rs',encoding='utf-8').read())"
```

对照 `feishu_pull.rs:127-137` 的 shutdown 处理（watch 通道 + JoinHandle）。**注意**：它的 shutdown 只对自己 watch 通道负责、**没有「处理中」状态**——本 worker 必须有，因为蓝绿 `docker stop`（SIGTERM → 10 秒 SIGKILL，`deploy/deploy-blue-green.sh:337`）会静默杀掉处理中的批次。

- [ ] **Step 2: 写失败测试（红）**

```rust
#[tokio::test]
async fn claim_sql_takes_pending_and_expired_lease() {
    // 照搬 authorization_outbox 的形态（outbox.rs:102,125）：
    // (state='pending' AND available_at <= now) OR (state='creating' AND lease_until <= now)
    // FOR UPDATE SKIP LOCKED
    // 断言：过期租约能被重新抢占（进程崩溃后不会永久死锁）。
}

#[tokio::test]
async fn second_batch_is_rejected_while_first_running() {
    // spec §7.7：同一配置的批量任务串行，第二轮直接返回已受理。
}

#[tokio::test]
async fn worker_releases_lease_on_graceful_shutdown() {
    // 优雅关闭时把处理中的行放回可认领状态，而不是留给租约超时。
}
```

- [ ] **Step 3: 实现 + 注册**

- 认领循环：Task 2 的 `feishu_approval_task`，SQL 形态照抄 `src/infrastructure/authorization/outbox.rs:102,125`。
- 扫描集合（spec §6.3）：**库表里无终态/已回填任务的记录 ∪ 库中处于可重试状态的记录**。多维表格的「审批编号为空」只作输出字段，**不作扫描键**。
- 在 `src/bootstrap.rs` 的 worker 阶段注册，参与 readiness 与关闭预算。
- 新增配置项（spec §8）：`approval_dispatch_enabled`、`approval_scan_interval_seconds`、`approval_create_rate_per_minute`、`approval_base_timezone`。**同步 `docs/contracts/CONFIGURATION.md`**。

- [ ] **Step 4: 运行确认通过 + 门禁**

```powershell
cargo test --lib --locked feishu_approval_worker
python scripts/run_ci.py quick
```

- [ ] **Step 5: 提交**

```powershell
git commit -am "feat(feishu): 审批派发后台 worker（认领/租约/限速/扫描集合）"
```

---

## Task 12: 审计与可观测性

**Files:**
- Modify: `src/addon/feishu/domain/approval_dispatch.rs`、`src/infrastructure/feishu_approval_worker.rs`
- Modify: `docs/contracts/AUDIT.md`（若该契约按事件逐个登记）

- [ ] **Step 1: 实现审计**

- 每条创建成功落一条审计事件：`config_id`、`record_id`、`instance_code`。
- 批量任务落一条任务级审计：受理人、批次数、成功/失败计数。
- **不挂 Step-up**（spec §9.4）：系统对系统的自动化动作，无自然人操作者。

- [ ] **Step 2: 实现指标**

按 `docs/contracts/OBSERVABILITY.md`：

- 创建成功/失败计数（**按错误码分桶**）
- 令牌桶等待时长
- 任务队列深度
- 租约超时次数

- [ ] **Step 3: 实现告警**

按 `docs/contracts/SLO.md` 的格式加规则（`ops/prometheus/`，CI 用 promtool 校验）：

- 可重试重试耗尽
- `60012` 回捞失败
- `1390015`（定义停用）——这类会让整批失败，值得告警

- [ ] **Step 4: 验证 promtool**

```powershell
python scripts/run_ci.py full
```

- [ ] **Step 5: 提交**

```powershell
git commit -am "feat(feishu): 审批派发审计与可观测性"
```

---

## Task 13: 控制台配置页

**Files:**
- Modify: `frontend/src/features/feishu/{types.ts, api.ts}`
- Create: `frontend/src/features/feishu/components/ApprovalConfigDialog.tsx`
- Create: `frontend/tests/features/feishu/approval-config.test.ts`

- [ ] **Step 1: 重新生成契约产物**

```powershell
python scripts/dump_openapi.py
```

（前端侧别名 `pnpm gen:contracts`）。两个生成物**禁止手改**。

- [ ] **Step 2: 写失败测试（红）**

```ts
// frontend/tests/features/feishu/approval-config.test.ts
it('映射项用 field_id 而不是字段名作为 key', () => { /* ... */ });
it('保存前校验必填控件都有映射', () => { /* ... */ });
it('展示控件类型以提示转换器选择', () => { /* ... */ });
```

- [ ] **Step 3: 实现**

配置页照抄 `DatasourceFormDialog.tsx` 的形态。**注意**：映射项的 key 是 `field_id`，UI 上展示 `field_name` 供人阅读——两者都存，但提交给后端的是 id（spec §5.1）。

- [ ] **Step 4: 跑前端门禁**

```powershell
pnpm --dir frontend check
```

- [ ] **Step 5: 提交**

```powershell
git commit -am "feat(frontend): 审批派发配置页"
```

---

## Task 14: 文档同步

**Files:**
- Modify: `AGENTS.md`、`docs/contracts/CONFIGURATION.md`、`docs/contracts/AUDIT.md`、`docs/architecture/feishu-option-ingest.md`（若需要交叉引用）

- [ ] **Step 1: 同步 AGENTS.md**

按该文件的既有颗粒度补：新增的 Action、worker、module、配置项、以及三张表。**这是硬要求**——该文件是仓库的单一事实源。

- [ ] **Step 2: 同步配置契约**

`docs/contracts/CONFIGURATION.md` 补 4 个新配置项，说明默认值与约束。

- [ ] **Step 3: 交叉引用**

在 `docs/architecture/feishu-option-ingest.md` 加一句指向本设计（两者共享「多维表格工作流能力边界」的实测事实，避免后来者重复实测）。

- [ ] **Step 4: 全量门禁**

```powershell
python scripts/run_ci.py full
```

- [ ] **Step 5: 提交**

```powershell
git commit -am "docs(feishu): 同步审批派发设计与实施文档"
```

---

## 落地顺序（不可乱）

1. 飞书应用加 scope → **重新发布**（不发布则后续全部失败）。
2. 审批管理后台创建目标审批定义（只含 API 支持的控件）→ 拿 `approval_code`。
3. 多维表格加「申请人」（人员）与「审批编号」（文本）两个字段。
4. 部署 Task 1–12 的成果 → 在控制台配一条 `feishu_approval_config`（保存时会做 spec §6.1 的前置校验，这一步即校验点）。
5. 配工作流的两个按钮（spec §4.1 的可照抄配置）。
6. 先在**一行测试数据**上用行内按钮跑通，再用页面按钮跑存量。

## 回滚

把 `approval_dispatch_enabled` 置 false 即停止新任务。**已创建的审批实例不能通过本系统撤回**（要撤回需逐单调 `instances cancel`）——这是回滚的硬边界，需要向使用方说明。

## 上线前必须实测（spec §12）

CI 无飞书凭据，以下无法自动化，**上线前用真实凭据跑一次**：

| # | 待测 | 阻塞什么 |
|---|---|---|
| M1 | 工作流 HTTP 节点按状态码还是响应体 `code` 判成败 | 决定鉴权失败会不会静默显示成功 |
| M2 | `serial_number` 创建后立即可查性 | Task 7 的退避参数 |
| M3 | `60012` 回捞（`instance/get` 传 uuid）的真实响应 | Task 7 的核心路径 |
| M4 | `is_external` 字段名、不支持控件的确切判据 | spec §6.1 的前置校验 |
| M5 | 100 次/分钟是应用级还是租户级 | 令牌桶是否需要跨应用协调 |
| M6 | 单条链路端到端耗时 | 确认在 60 秒超时内 |

# 飞书审批「同批聚合派发」实施计划（v2）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给审批派发配置加一个默认关闭的开关，打开后把同一次批量勾选内、除明细字段外转换值完全一致的记录合成一个审批实例，明细每条记录一行，同编号回填、每组一条通知。

**Architecture:** 组行持有组实例身份（uuid/instance_code/serial/effect_phase），leader 任务行只做租约与认领；播种先做组维护再按当前勾选新建；处理走组专用确认/收尾，事件先查组表；开关关闭只停止新建组，已有组仍能收尾。

**Tech Stack:** Rust 2021（axum + sqlx/MySQL + Redis）、React 19 + TypeScript、飞书审批 OpenAPI、`scripts/run_ci.py` 门禁。

**Spec:** `docs/architecture/2026-10-31-feishu-approval-aggregation-design.md`（v2，2026-10-31 复核后）

## Global Constraints

- 仓库禁止新增 SQL 迁移文件；schema 由声明式同步驱动，只增不删。
- 配置列 `aggregate_batch` 必须 `Switch::new().require(true).default(false)`；`group_seq` 必须 `require(true).default(0).sortable(true)`；`group_id` 可空；组表 `effect_phase` 必须 `require(true).default("ready")`。
- 组实例身份（`uuid/instance_code/serial_number`）**只写组行**；leader/成员任务行禁止写组 instance_code。
- 新表必须遵循「一张表 = 一个 module」，并接入 `FeishuContext` 与全部构造点。
- 生产 Rust 禁止 `unsafe`/`unwrap()`/`expect()`；错误用 `anyhow` 传播；文档与注释用中文。
- 所有新增 `#[ignore]` 测试必须逐个登记 `scripts/run_ci.py` 的 `INTEGRATION`；登记集合必须与源码中的 `#[ignore]` 函数集合完全相等。
- `cargo test --lib` 一次只能带一个过滤名；多个测试分多次跑。
- 每个新增分支至少做一次摘除式变异（删掉判断确认测试变红，再恢复）。
- 本机禁止并行 cargo/pnpm；前端改动推送前跑 `pnpm --dir frontend check` 并重跑 `python scripts/dump_openapi.py`。
- 不读取 `deploy/config.cloud*.toml` 与 `~/.claude` 下的配置。

## Review Focus

1. 开关关闭、存量任务行（`group_id=NULL`）时，播种/认领/处理/回填/通知与现在一致；已存在的组仍能由 leader 收尾。
2. 组实例身份只落组行；单条点击组 leader/成员会摘出并提升新 leader，`force_solo` 能建出第二实例。
3. `paused` 落 leader 与全体成员，轮询零认领；`review` 组点击后保留组 uuid 并靠 60012/按 uuid 回捞恢复。
4. 播种先解散「全组都在本次勾选内、`effect_phase=ready`、无租约」的组，再计算候选；勾选外的组不动。
5. 空明细成员整组暂停；选项映射按成员并集；`group_seq` 排序可用。
6. 事件先查组表、uuid 兜底也先查组表；任务行命中但 `group_id` 非空时仍走组 fan-out。

---

### Task 0: worker transport 抽象（测试前置）

**Files:**
- Modify: `src/infrastructure/feishu_approval_worker.rs`

**Interfaces:**
- Produces: `RoundRunner.transport: Arc<dyn OutboundTransport>`（照 `feishu_event_worker.rs` 的既有形态）；构造点改为注入具体实现。

- [ ] **Step 1: 改类型并修构造点**

把 `transport: Arc<HttpClientTransport>` 改为 `Arc<dyn OutboundTransport>`；`run` 的构造处保持不变（`Arc<HttpClientTransport>` 可自动强转）。检查同文件所有 `runner.transport` 调用点（`send`/`clone` 用法）仍可编译。

- [ ] **Step 2: 跑编译与单测**

Run: `cargo test --lib`
Expected: PASS（无行为变化）。

- [ ] **Step 3: Commit**

```bash
git add src/infrastructure/feishu_approval_worker.rs
git commit -m "refactor(feishu): worker 传输改为 trait 对象以支持注入"
```

---

### Task 1: schema、状态与 FeishuContext 接入

**Files:**
- Modify: `src/addon/feishu/approval/table.rs`（`aggregate_batch`）
- Modify: `src/addon/feishu/approval/domain/task_table.rs`（`group_id`、`group_seq`、`paused`）
- Create: `src/addon/feishu/approval/domain/group_table.rs`
- Modify: `src/addon/feishu/approval/mod.rs`（`build_group_module`）
- Modify: `src/addon/feishu/mod.rs`（装配）
- Modify: `src/addon/feishu/domain/context.rs`（`approval_groups()`，`new` 增加参数）
- Modify: 构造点 `src/addon/feishu/mod.rs`、`src/addon/feishu/approval/actions/mod.rs`、`actions/dispatch.rs`、`actions/list_events.rs`、`src/addon/feishu/datasource/mod.rs`
- Modify: `tests/feishu_approval_dispatch_integration.rs`（`reset_database` 加新表）

**Interfaces:**
- Produces: 配置列 `aggregate_batch`；任务列 `group_id/group_seq`；状态 `"paused"`；表 `feishu_approval_group`（含 `effect_phase`）；`FeishuContext::approval_groups()`。

- [ ] **Step 1: 写失败测试**

`task_table.rs`：

```rust
#[test]
fn group_columns_and_paused_state_are_declared() {
    let definition = definition();
    assert!(definition.field("group_id").is_some());
    let group_seq = definition.field("group_seq").unwrap();
    assert!(group_seq.is_required());
    assert!(group_seq.is_sortable(), "load_members 按 group_seq 排序依赖该位");
    assert_eq!(group_seq.default_value(), Some(&serde_json::json!(0)));
    let spec = table_spec().unwrap();
    let state = spec.fields.iter().find(|f| f.name.as_str() == "state").unwrap();
    assert!(state.options.iter().any(|(value, _)| value == "paused"));
}
```

`approval/table.rs`：断言 `aggregate_batch` 必填、默认 false。`group_table.rs`：断言 `unique_named` 复合键、`uuid` 唯一、`effect_phase` 必填且默认 `"ready"`。`context.rs` 或集成测试：`FeishuContext::approval_groups()` 可访问。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib group_columns_and_paused_state_are_declared`
Expected: FAIL。

- [ ] **Step 3: 实现声明**

- `task_table.rs` 加列；`state` options 追加 `("paused", "已暂停")`，同步既有精确断言列表。
- `approval/table.rs` 加 `aggregate_batch`。
- `group_table.rs` 按设计 §4.2 声明全部列与索引。
- `approval/mod.rs`、`feishu/mod.rs` 装配；`context.rs` 新增字段与访问器，更新 5 处构造点。
- `tests/feishu_approval_dispatch_integration.rs` 的 `reset_database` 加 `feishu_approval_group`。

> 若上一阶段已生成旧版 Task 1（无 `effect_phase`/`sortable`/context 接入），在本任务中直接补齐，不另起迁移。

- [ ] **Step 4: 跑测试**

Run: `cargo test --lib group_columns_and_paused_state_are_declared`
Run: `cargo test --lib aggregate_batch`
Run: `cargo test --lib group_table`
Expected: 全部 PASS。

- [ ] **Step 5: Commit**

```bash
git add src/addon/feishu tests/feishu_approval_dispatch_integration.rs
git commit -m "feat(feishu): 聚合组表、执行阶段与状态列"
```

---

### Task 2: 配置开关的 CRUD 与校验

**Files:**
- Modify: `src/addon/feishu/approval/actions/create_config.rs`
- Modify: `src/addon/feishu/approval/actions/update_config.rs`
- Modify: `src/addon/feishu/approval/actions/list_configs.rs`
- Modify: `src/addon/feishu/domain/approval_provision.rs`
- Modify: `src/infrastructure/feishu_approval_worker.rs`（播种 `select_fields`）

**Interfaces:**
- Produces: `check_aggregate_batch(aggregate_batch: bool, batch_scope_field_id: Option<&str>) -> Vec<String>`；`CreateConfigInput.aggregate_batch: bool`；`UpdateConfigInput.aggregate_batch: Option<bool>`；`ConfigItem.aggregate_batch: bool`。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn rejects_aggregate_without_batch_scope() {
    assert!(check_aggregate_batch(true, None)
        .iter()
        .any(|error| error.contains("批量范围复选框列")));
}

#[test]
fn accepts_aggregate_with_batch_scope() {
    assert!(check_aggregate_batch(true, Some("fldScope")).is_empty());
}
```

`update_config.rs`：只提交 `aggregate_batch=true` 而库内无范围列 → `ParamInvalid`；清空范围列且聚合仍 true → `ParamInvalid`；只关开关 → 成功且审计摘要含该键。`list_configs.rs`：投影含 `aggregate_batch`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib check_aggregate_batch`
Expected: FAIL。

- [ ] **Step 3: 实现**

- `approval_provision.rs` 加纯校验函数。
- `create_config.rs`：输入加字段；`build_plan` 后用 `plan.batch_scope_field_id` 校验；同事务 `update_in_tx` 写入开关。
- `update_config.rs`：输入、至少一项清单、事务内合并库内值校验、写库与审计摘要。
- `list_configs.rs`：四处投影加字段。
- worker 播种 `select_fields` 加 `"aggregate_batch"`。

- [ ] **Step 4: 跑测试**

Run: `cargo test --lib check_aggregate_batch`
Run: `cargo test --lib list_configs`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/addon/feishu src/infrastructure/feishu_approval_worker.rs
git commit -m "feat(feishu): 聚合开关的配置 CRUD 与校验"
```

---

### Task 3: 一致性签名与附件源标识

**Files:**
- Modify: `src/addon/feishu/domain/approval_convert.rs`
- Modify: `src/addon/feishu/domain/approval_attachment.rs`

**Interfaces:**
- Produces:
  - `prepare_definition<'a>(form_snapshot: &str, widgets: &'a [WidgetMap]) -> Result<(Vec<FormWidget>, BTreeMap<&'a str, &'a WidgetMap>), ConvertError>`
  - `build_signature(input: &FormInput<'_>) -> Result<String, ConvertError>`
  - `source_identities(widgets: &[WidgetMap], cells: &Map<String, Value>) -> Result<BTreeMap<String, Vec<String>>, ConvertError>`（`"<file_token>|<name>|<size>"`，源顺序）

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn signature_excludes_nested_field_list_anywhere() {
    // workGroup 内嵌 fieldList 的子控件差异不改变签名。
}

#[test]
fn signature_normalizes_number_forms_and_encodes_missing_as_null() {
    // 100 与 "100" 同签名；必填缺失编码 null。
}

#[test]
fn signature_separates_attachment_source_identity() {
    // 不同 file_token 或 size → 不同签名。
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib signature_`
Expected: FAIL。

- [ ] **Step 3: 实现**

抽 `prepare_definition`；`build_form` 复用；`build_signature` 递归时遇到**任意深度** `fieldList` 整棵跳过，控件组递归，空值写 `null`，不抛必填错误；附件用 `source_identities` 填充 `FormInput.attachment_codes`。禁止 `transfer`。

- [ ] **Step 4: 跑测试**

Run: `cargo test --lib signature_`
Run: `cargo test --lib source_identities`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/addon/feishu/domain/approval_convert.rs src/addon/feishu/domain/approval_attachment.rs
git commit -m "feat(feishu): 聚合派发的一致性签名"
```

---

### Task 4: 组键、分组规划与组表仓储（含事务变体）

**Files:**
- Create: `src/addon/feishu/domain/approval_aggregation.rs`

**Interfaces:**
- Produces:
  - `signature_hash(signature_json: &str) -> String`（32 hex）
  - `group_key(config_id: i64, signature_hash: &str, record_ids: &[String]) -> String`（32 hex）
  - `group_uuid(base_token, table_id, approval_code, group_key) -> String`
  - `GroupCandidate { record_id: String, signature: String }`
  - `PlannedGroup { group_key: String, signature_hash: String, members: Vec<String> }`
  - `plan_groups(config_id: i64, candidates: &[GroupCandidate]) -> Vec<PlannedGroup>`
  - `members_all_selected(members: &[String], selected: &HashSet<String>) -> bool`
  - 仓储（`&mut sqlx::Transaction<'_, MySql>` 变体带 `_in_tx` 后缀）：
    - `insert_group_in_tx(tx, config_id, group_key, signature_hash, uuid)`
    - `save_created_in_tx(tx, config_id, group_key, instance_code)`
    - `save_backfilled_in_tx(tx, config_id, group_key, serial_number)`
    - `mark_group_phase_in_tx(tx, config_id, group_key, phase, instance_code: Option<&str>)`
    - `delete_group_in_tx(tx, config_id, group_key)`
    - `delete_groups_for_config_in_tx(tx, config_id)`
    - `load_group(context, config_id, group_key) -> Option<Record>`
    - `load_group_by_instance(context, instance_code) -> Option<Record>`
    - `load_group_by_uuid(context, uuid) -> Option<Record>`
    - `list_groups_for_config(context, config_id) -> Vec<Record>`
    - `load_members(context, config_id, group_key) -> Vec<Record>`（按 `group_seq` 升序）
    - `set_members_state_in_tx(tx, config_id, group_key, state, reason: Option<&str>)`
    - `delete_task_rows_in_tx(tx, task_ids: &[i64])`（先删 `feishu_approval_task_applicant` 再删任务行）

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn plan_groups_merges_identical_signatures_in_scan_order() { /* A,C 一组；B 一组 */ }

#[test]
fn group_key_depends_on_member_set() { /* 顺序无关、集合相关 */ }

#[test]
fn group_uuid_is_deterministic_and_36_chars() { /* 同输入同输出 */ }

#[test]
fn members_all_selected_requires_every_member() { /* 少一个即 false */ }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib plan_groups_`
Run: `cargo test --lib group_key_`
Run: `cargo test --lib group_uuid_`
Run: `cargo test --lib members_all_selected_`
Expected: FAIL。

- [ ] **Step 3: 实现**

纯函数按接口实现；仓储照 `approval_execution/repository.rs` 的裸 SQL 风格，每处裸 SQL 加 `// tenant-boundary: raw-sql feishu-approval-group-<id>`。`load_members` 用 `order_by("group_seq", Asc)`（依赖 Task 1 的 sortable）。

- [ ] **Step 4: 跑测试**

四个过滤名分四次 `cargo test --lib <name>`。
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/addon/feishu/domain/approval_aggregation.rs
git commit -m "feat(feishu): 聚合组键与组表仓储"
```

---

### Task 5: 播种与组维护

**Files:**
- Modify: `src/infrastructure/feishu_approval_worker.rs`
- Test: worker 内 `#[ignore]` 真实库测试

**Interfaces:**
- Consumes: Task 2 开关、Task 3 签名、Task 4 规划与仓储。
- Produces:
  - `async fn maintain_groups(runner, config, pending: &[&bitable::RecordItem], applicant: Option<&str>) -> anyhow::Result<usize>`
  - `async fn seed_aggregate_groups(runner, config, pending, applicant) -> anyhow::Result<SeedOutcome>`

- [ ] **Step 1: 写失败测试（全部登记 run_ci.py）**

```rust
#[tokio::test] #[ignore = "real mysql"]
async fn aggregate_seed_creates_one_group_with_two_members() {}
#[tokio::test] #[ignore = "real mysql"]
async fn aggregate_seed_only_runs_on_batch_click() {}
#[tokio::test] #[ignore = "real mysql"]
async fn aggregate_off_keeps_one_instance_per_record() {}
#[tokio::test] #[ignore = "real mysql"]
async fn batch_click_dissolves_only_fully_selected_ready_groups() {}
#[tokio::test] #[ignore = "real mysql"]
async fn batch_click_reopens_review_group_preserving_group_phase() {}
#[tokio::test] #[ignore = "real mysql"]
async fn submitted_group_with_cleared_member_serial_reopens_leader_for_backfill() {}
#[tokio::test] #[ignore = "real mysql"]
async fn aggregate_on_still_reopens_plain_review_task() {}
#[tokio::test] #[ignore = "real mysql"]
async fn aggregate_off_dissolves_unsubmitted_group_into_individual_tasks() {}
#[tokio::test] #[ignore = "real mysql"]
async fn dissolve_removes_member_rows_and_applicant_rows() {}
```

把 9 个名字逐个加入 `scripts/run_ci.py` 的 `INTEGRATION`（按仓库自检要求，集合相等）。

- [ ] **Step 2: 跑测试确认失败**

Run: `python scripts/run_ci.py integration`
Expected: FAIL。

- [ ] **Step 3: 实现**

`seed_config` 改为：批量点击时依次执行
1. `SELECT ... feishu_approval_config ... FOR UPDATE` 锁配置行；
2. 非组任务行 `reopen`（现有 `reopens_on_click` 语义，含 `backfilled` 且回填列为空）；
3. `maintain_groups`：已提交组补回填、review/lookup 组 reopen、可解散组（全组在本次勾选内、组行 `effect_phase=ready`、无租约）解散并删 applicant/成员/组行，摘除有自身实例或回填的成员；
4. 仅当 `aggregate_batch=true` 时 `seed_aggregate_groups`：**解散之后**重新计算候选（勾选 + 回填空 + 无任务行），逐条 `rekey_cells_by_field_id` → `resolve_external_options` → `source_identities` → `build_signature`（映射装载用 `widget_maps_from_rows`），签名失败用 `error:{record_id}` 单独成组；`plan_groups`；一个事务插组行与 leader/member 行；给 `touched` 落发起人。
单条点击分支不改：不维护组、不新建组。

- [ ] **Step 4: 跑测试**

Run: `python scripts/run_ci.py integration`
Expected: PASS（8 个新用例）。

- [ ] **Step 5: Commit**

```bash
git add src/infrastructure/feishu_approval_worker.rs scripts/run_ci.py
git commit -m "feat(feishu): 批量播种的组维护与分组"
```

---

### Task 6: 多行明细表单组装

**Files:**
- Modify: `src/addon/feishu/domain/approval_convert.rs`

**Interfaces:**
- Produces:
  - `GroupFormRow<'a> { cells: &'a Map<String, Value>, attachment_codes: &'a BTreeMap<String, Vec<String>> }`
  - `GroupFormInput<'a> { widgets: &'a [WidgetMap], form_snapshot: &'a str, rows: &'a [GroupFormRow<'a>], timezone_offset: FixedOffset }`
  - `build_group_form(input: &GroupFormInput<'_>) -> Result<String, ConvertError>`
- 语义：顶层 `fieldList` **恰好 N 行**；任一成员明细为空 → `ConvertError::BadValue { widget_id, reason: "记录 <id> 的明细为空" }`；进入某行后的嵌套 `fieldList` 只接收该行的单行切片；非明细取 `rows[0]`；选项使用调用方传入的并集 `WidgetMap`。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn group_form_emits_exactly_one_row_per_member() {}
#[test]
fn group_form_rejects_a_member_with_empty_detail() {}
#[test]
fn nested_field_list_inside_a_row_uses_that_rows_values() {}
#[test]
fn group_form_uses_first_row_for_non_detail_widgets() {}
#[test]
fn group_form_fills_every_detail_widget_with_n_rows() {}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib group_form_`
Expected: FAIL。

- [ ] **Step 3: 实现**

`assemble_definition` 改为接收 `rows: &[GroupFormRow]` 与 `inside_detail_row: bool`；`build_form` 包装单行；`build_group_form` 顶层逐行强制输出（空明细则报错，不跳过）；进入行内递归时传 `&rows[i..i+1]`。

- [ ] **Step 4: 跑测试**

Run: `cargo test --lib group_form_`
Run: `cargo test --lib approval_convert`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/addon/feishu/domain/approval_convert.rs
git commit -m "feat(feishu): 多行明细表单组装"
```

---

### Task 7: 执行层：认领、组确认、组收尾、删除与摘组

**Files:**
- Modify: `src/addon/feishu/domain/approval_execution/repository.rs`
- Modify: `src/addon/feishu/domain/approval_execution/transport.rs`
- Modify: `src/addon/feishu/approval/actions/delete_config.rs`
- Modify: `src/addon/feishu/approval/actions/dispatch.rs`（单条点击摘组 + force_solo）
- Test: 各文件内 `#[ignore]` 真实库测试

**Interfaces:**
- Produces:
  - `claim_ready`：批量（`only_id=None`）加 `AND group_seq=0`；单条（`Some`）不限；阶段条件允许组 leader 的 `lookup`。
  - `begin_group_effect(context, claim, group_key, phase)`（组行阶段 + leader 阶段）
  - `confirm_group_effect(context, claim, group_key, phase, instance: Option<&str>)`（实例码只写组行；leader 只写阶段）
  - `finish_group(context, claim, group_key, state, serial: Option<&str>, reason: Option<&str>, retry: bool) -> anyhow::Result<bool>`（一个事务：leader 状态/attempts/available_at/清租约、全体成员状态、组行 serial/阶段；接受 `ready/creating/lookup/created`）
  - `detach_from_group(context, task_id) -> anyhow::Result<DetachOutcome>`（锁配置行、检查租约、摘出、必要时提升 leader 或删除组行）
  - `dispatch_single(..., force_solo: bool)`

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test] #[ignore = "real mysql"]
async fn claim_ready_skips_group_members_in_batch_mode() {}
#[tokio::test] #[ignore = "real mysql"]
async fn claim_ready_by_id_allows_group_member() {}
#[tokio::test] #[ignore = "real mysql"]
async fn claim_ready_accepts_group_leader_in_lookup_phase() {}
#[tokio::test] #[ignore = "real mysql"]
async fn reopen_resets_paused_rows() {}
#[tokio::test] #[ignore = "real mysql"]
async fn finish_group_clears_lease_and_writes_member_states() {}
#[tokio::test] #[ignore = "real mysql"]
async fn delete_config_removes_group_rows_members_and_applicants() {}
#[tokio::test] #[ignore = "real mysql"]
async fn delete_config_allows_unsubmitted_paused_group() {}
#[tokio::test] #[ignore = "real mysql"]
async fn delete_config_blocks_paused_group_with_instance() {}
#[tokio::test] #[ignore = "real mysql"]
async fn detaching_leader_promotes_next_member() {}
#[tokio::test] #[ignore = "real mysql"]
async fn force_solo_creates_second_instance_for_submitted_group_member() {}
```

逐个登记 `scripts/run_ci.py`。

- [ ] **Step 2: 跑测试确认失败**

Run: `python scripts/run_ci.py integration`
Expected: FAIL。

- [ ] **Step 3: 实现**

按接口实现；`delete_config` 活跃白名单加 `paused` 但区分组行是否有 `instance_code`：未提交的暂停组放行并随事务清理，已建实例的暂停组仍阻塞；删除事务内调用 `delete_groups_for_config_in_tx` 与 applicant 清理。`dispatch.rs` 的单条入口在任务属于组时先 `detach_from_group`，并把 `force_solo=true` 传下去。

- [ ] **Step 4: 跑测试**

Run: `python scripts/run_ci.py integration`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/addon/feishu/domain/approval_execution src/addon/feishu/approval/actions scripts/run_ci.py
git commit -m "feat(feishu): 组认领、组确认收尾与摘组"
```

---

### Task 8: 组派发（创建、回捞、批量回填、扩展字段）

**Files:**
- Modify: `src/addon/feishu/domain/approval_dispatch.rs`
- Modify: `src/addon/feishu/domain/approval_execution/transport.rs`（`GroupExecutionTransport`）

**Interfaces:**
- Produces:
  - `GroupMemberInput<'a> { record_id: &'a str, cells: &'a Map<String, Value>, attachment_codes: BTreeMap<String, Vec<String>> }`
  - `GroupDispatchInput<'a>`：`fields/context/execution/coordinates/group_key/group_uuid/instance_code/backfill_field_name/六个可选字段/approval_code/widgets/form_snapshot/timezone_offset/applicant_field/applicant_open_id/members`
  - `dispatch_group(transport, sleeper, tokens, backfill, input) -> DispatchResult`
  - `classify_group_failure(...) -> GroupOutcome`（Paused/Retryable，禁止 `backfill.write`）
- 关键规则：create 的 `uuid` 用组 `uuid`；组确认不写 leader `instance_code`；`60012` → 组 `lookup` → 按组 uuid 回捞；`BackfillReport.failed` 非空 → `CreatedPendingBackfill`；成功回填后逐成员 `write_extended`（六个扩展字段 + 发起人列）。

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test]
async fn group_dispatch_sends_one_create_with_two_detail_rows() {}
#[tokio::test]
async fn group_dispatch_never_writes_instance_code_to_leader() {}
#[tokio::test]
async fn group_dispatch_recovers_by_group_uuid_on_60012() {}
#[tokio::test]
async fn group_dispatch_returns_created_pending_backfill_on_partial_failure() {}
#[tokio::test]
async fn group_dispatch_fatal_writes_no_backfill_cells() {}
#[tokio::test]
async fn group_dispatch_writes_extended_fields_and_applicant_per_member() {}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib group_dispatch_`
Expected: FAIL。

- [ ] **Step 3: 实现**

用同文件的 `ScriptedTransport`/`RecordingBackfill`（不要用 `actions/dispatch.rs` 私有测试模块里的 `ReplayTransport`）。`GroupExecutionTransport` 照 `ExecutionTransport` 改写：begin/confirm 调组专用函数；发生成时传组 uuid。

- [ ] **Step 4: 跑测试**

Run: `cargo test --lib group_dispatch_`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/addon/feishu/domain/approval_dispatch.rs src/addon/feishu/domain/approval_execution/transport.rs
git commit -m "feat(feishu): 组实例创建、回捞与批量回填"
```

---

### Task 9: worker 组处理、暂停持久化与组通知

**Files:**
- Modify: `src/infrastructure/feishu_approval_worker.rs`
- Test: worker 内 `#[ignore]` + 单元测试

**Interfaces:**
- Produces:
  - `ClaimedTask` 增加 `group_id: Option<String>`、`group_seq: i64`、`effect_phase: String`、`instance_code: Option<String>`，`load_task` 同步投影。
  - `async fn process_group(runner, task) -> TaskOutcome`（带 `persisted: bool`，让 `run_round` 跳过通用 `record_outcome`）
  - `fn group_pause_message(size: usize, failing: &[(String, String)], reclick: bool) -> String`
  - `async fn group_notification(...) -> WorkerNotification`（一次渲染、预解析成员行号）

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn pause_message_mentions_reclick() {}
#[tokio::test] #[ignore = "real mysql"]
async fn group_pause_marks_leader_and_members_paused() {}
#[tokio::test] #[ignore = "real mysql"]
async fn group_pause_is_not_reclaimed_by_poll() {}
#[tokio::test] #[ignore = "real mysql"]
async fn group_success_backfills_same_serial_and_writes_extensions() {}
#[tokio::test] #[ignore = "real mysql"]
async fn group_notification_is_sent_once_with_member_row_summary() {}
#[tokio::test] #[ignore = "real mysql"]
async fn absent_member_in_created_group_is_detached_and_rest_backfilled() {}
```

逐个登记 `scripts/run_ci.py`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib pause_message_`
Run: `python scripts/run_ci.py integration`
Expected: FAIL。

- [ ] **Step 3: 实现**

- `process_one`：`task.group_id.is_some()` → `process_group`。
- 读组行/成员；恢复顺序与错误分类按设计 §4.6；签名复核仅 `group.effect_phase='ready'`；选项映射并集；空明细 → 暂停。
- 成功/暂停都用 `finish_group`；暂停写 leader+成员 `state='paused'`，不写回填列。
- 组通知：发送前解析成员行号，一次渲染；暂停消息含失败成员行号 + 原因 + 重新点击提醒；跳过请求日志去重。
- `run_round` 对 `persisted=true` 的组结论跳过通用 `record_outcome`/`finish`。

- [ ] **Step 4: 跑测试**

Run: `cargo test --lib pause_message_`
Run: `python scripts/run_ci.py integration`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/infrastructure/feishu_approval_worker.rs scripts/run_ci.py
git commit -m "feat(feishu): 组处理、暂停持久化与组通知"
```

---

### Task 10: 事件组优先与 uuid 兜底

**Files:**
- Modify: `src/infrastructure/feishu_event_worker.rs`

**Interfaces:**
- Produces: `MissingTaskResolution::FoundGroup { config_id: i64, group_key: String }`；`fan_out_group_event(runner, group, event) -> EventOutcome`。

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test] #[ignore = "real mysql"]
async fn event_by_instance_code_updates_all_member_records() {}
#[tokio::test] #[ignore = "real mysql"]
async fn event_before_instance_code_uses_group_uuid_fallback() {}
#[tokio::test] #[ignore = "real mysql"]
async fn task_row_hit_with_group_id_still_fans_out_to_members() {}
```

登记 `run_ci.py`。

- [ ] **Step 2: 跑测试确认失败**

Run: `python scripts/run_ci.py integration`
Expected: FAIL。

- [ ] **Step 3: 实现**

`process_event_inner` 先查组表（instance_code），未命中再查任务表；`resolve_missing_task` 的 uuid 兜底先查组表；任务行命中但 `group_id` 非空时按组处理。fan-out 逐成员 `write_extended` 并更新成员行状态。

- [ ] **Step 4: 跑测试**

Run: `python scripts/run_ci.py integration`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/infrastructure/feishu_event_worker.rs scripts/run_ci.py
git commit -m "feat(feishu): 组事件优先与 uuid 兜底"
```

---

### Task 11: 前端开关、暂停标签与契约

**Files:**
- Modify: `frontend/src/features/feishu/components/ApprovalConfigDialog.tsx`
- Modify: `frontend/src/features/feishu/api.ts`
- Modify: `frontend/src/features/feishu/types.ts`
- Modify: `frontend/src/features/feishu/components/ApprovalConfigDetails.tsx`
- Modify: `frontend/src/features/feishu/views/ApprovalRequestsPage.tsx`（`paused: "已暂停"`）
- Modify: `frontend/tests/features/feishu/components/approval-config-dialog.test.tsx`
- Modify: `frontend/tests/features/feishu/views/approval-configs-page.test.tsx`
- Modify: `frontend/e2e/approval-config-business.spec.ts`、`frontend/e2e-production/approval-config-business.spec.ts`
- Generated: `frontend/contracts/openapi.json`、`frontend/src/engine/contracts/api-types.ts`

**Interfaces:**
- Consumes: 后端 `aggregate_batch` 与 `paused`。

- [ ] **Step 1: 写失败测试**

```ts
it("does not allow aggregate without a batch scope column", async () => {});
it("sends aggregate_batch on create and as a patch on update", async () => {});
it("renders paused tasks with a Chinese label", async () => {});
```

- [ ] **Step 2: 跑测试确认失败**

Run: `pnpm --dir frontend test -- approval-config-dialog`
Run: `pnpm --dir frontend test -- approval-configs-page`
Expected: FAIL。

- [ ] **Step 3: 实现**

对话框加复选框（未选范围列时禁用）；create/update 映射；类型与详情页；任务列表补 `paused` 标签；E2E 夹具补字段。

- [ ] **Step 4: 生成契约并跑检查**

Run: `python scripts/dump_openapi.py`
Run: `pnpm --dir frontend check`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add frontend
git commit -m "feat(feishu): 聚合开关与暂停态前端"
```

---

### Task 12: 文档、门禁与真机验证

**Files:**
- Modify: `AGENTS.md`
- Modify: `docs/architecture/2026-09-28-feishu-approval-dispatch-design.md`（§5.1/§5.2 指向新设计）
- Modify: `docs/guides/BUSINESS_USER_GUIDE.md`（聚合与暂停语义）

- [ ] **Step 1: 更新文档**

补 `feishu_approval_group`、`aggregate_batch`、`group_id/group_seq`、`paused`、组维护与回滚语义；使用手册补「暂停组需重新批量点击」「单条点击会摘组」。

- [ ] **Step 2: 核对 ignored 登记**

Run: `python scripts/run_ci.py --self-test`
Expected: PASS；逐个核对 Task 5/7/9/10 的测试名都已登记。

- [ ] **Step 3: 门禁**

Run: `python scripts/run_ci.py quick`
Run: `python scripts/run_ci.py integration`
Run: `python scripts/run_ci.py full`
Expected: 全部 PASS。

- [ ] **Step 4: 摘除式变异**

逐条删除并确认测试变红后恢复：开关短路、`group_seq=0` 批量认领、组行实例码归属、查找组表优先、暂停持久化、解散全选条件、空明细报错、`BackfillReport.failed` 检查、签名排除明细。把结果写进 PR 说明。

- [ ] **Step 5: 真机验证（开启开关前，阻塞上线）**

按设计 §9 清单执行：2/10/50/100/200 行；空明细/稀疏行/不等长；amount/dateInterval/number/formula/contact/department/telephone/外部选项/附件；60012 回捞、事件、审批端展示。

- [ ] **Step 6: Commit**

```bash
git add AGENTS.md docs src
git commit -m "docs(feishu): 聚合派发文档与验证清单"
```

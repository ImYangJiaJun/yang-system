# 飞书审批「同批聚合派发」实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给审批派发配置加一个默认关闭的开关，打开后把同一次批量勾选内、除明细字段外转换值完全一致的记录合成一个审批实例，明细每条记录一行，同编号回填、每组一条通知。

**Architecture:** 新增一张组表承载组 uuid/实例码/编号，任务表加 `group_id/group_seq` 把成员行挂在组上；播种期算一致性签名并分组，认领只认 leader 行；处理期一次 `create` 出 N 行明细、一次批量回填 N 条记录；事件按实例码或 uuid 兜底命中组后 fan-out 到全部成员。开关关闭时所有新分支短路，存量行为不变。

**Tech Stack:** Rust 2021（axum + sqlx/MySQL + Redis）、React 19 + TypeScript、飞书审批 OpenAPI、`scripts/run_ci.py` 门禁。

**Spec:** `docs/architecture/2026-10-31-feishu-approval-aggregation-design.md`

## Global Constraints

- 仓库禁止新增 SQL 迁移文件；schema 由 `src/infrastructure/schema.rs` 的声明式同步驱动，只增不删。
- 配置列 `aggregate_batch` 必须 `Switch::new().require(true).default(false)`；任务列 `group_seq` 必须 `require(true).default(0)`；`group_id` 可空。已有数据的表加「必填且无默认值」的新列会拒绝启动。
- 新表必须遵循「一张表 = 一个 module」：新增 `group_table.rs` 并在 `src/addon/feishu/approval/mod.rs` 与 `src/addon/feishu/mod.rs` 装配。
- 生产 Rust 代码禁止 `unsafe`、`unwrap()`、`expect()`；错误用 `anyhow` 传播。
- 输入结构体带 `#[serde(deny_unknown_fields)]`，新增字段必须同时改 action 输入与前端 payload。
- 仓库文档与注释使用中文。
- 新 `#[ignore]` 测试或 `tests/*.rs` 必须登记进 `scripts/run_ci.py` 的 `INTEGRATION`，否则 `--self-test` 与 CI 直接失败。
- 每个新增分支至少做一次摘除式变异（删掉该判断确认对应测试变红，再改回）。
- 本机禁止并行跑 cargo 与 pnpm；前端改动推送前必须 `pnpm --dir frontend check` 并重跑 `python scripts/dump_openapi.py`。
- 不读取 `deploy/config.cloud*.toml` 与 `~/.claude` 下的配置。

## Review Focus

1. 开关关闭、存量任务行（`group_id=NULL, group_seq=0`）时，播种/认领/处理/回填/通知与现在完全一致。
2. 单条点击组内成员或 leader 时，用记录自己的 uuid 独立建单，不覆盖组行的 uuid/instance_code。
3. 暂停组在批量点击后按当前勾选重组；`effect_phase ∈ {creating, lookup, created}` 或 `review` 的组永不解散。
4. 超过 100 条成员的组，读取与回填按分片执行，部分失败重试后收敛到全量。
5. 事件早于 `instance_code` 落库到达时（`event.uuid` = 组 uuid），仍能命中组并把扩展字段 fan-out 到全部成员记录。

---

### Task 1: schema 与状态枚举

**Files:**
- Modify: `src/addon/feishu/approval/table.rs`（配置表加 `aggregate_batch`）
- Modify: `src/addon/feishu/approval/domain/task_table.rs`（`group_id`、`group_seq`、`paused`）
- Create: `src/addon/feishu/approval/domain/group_table.rs`
- Modify: `src/addon/feishu/approval/mod.rs`（`build_group_module`）
- Modify: `src/addon/feishu/mod.rs`（装配新 module）
- Modify: `tests/feishu_approval_dispatch_integration.rs`（`reset_database` 的 DROP TABLE 列表）

**Interfaces:**
- Produces: 配置列 `aggregate_batch`；任务列 `group_id`/`group_seq`；状态值 `"paused"`；表 `feishu_approval_group`（列见设计 §4.2）。

- [ ] **Step 1: 写失败的表声明测试**

在 `task_table.rs` 的 `#[cfg(test)]` 里加：

```rust
#[test]
fn group_columns_and_paused_state_are_declared() {
    let definition = definition();
    assert!(definition.field("group_id").is_some());
    let group_seq = definition
        .field("group_seq")
        .unwrap_or_else(|| panic!("group_seq 字段必须存在"));
    assert!(group_seq.is_required());
    assert_eq!(group_seq.default_value(), Some(&serde_json::json!(0)));
    let spec = table_spec().unwrap_or_else(|error| panic!("表声明应有效: {error}"));
    let state = spec
        .fields
        .iter()
        .find(|field| field.name.as_str() == "state")
        .unwrap_or_else(|| panic!("state 字段必须存在"));
    assert!(state.options.iter().any(|(value, _)| value == "paused"));
}
```

在 `approval/table.rs` 的测试里加：

```rust
#[test]
fn aggregate_batch_defaults_to_false_and_is_required() {
    let definition = definition();
    let field = definition
        .field("aggregate_batch")
        .unwrap_or_else(|| panic!("aggregate_batch 字段必须存在"));
    assert!(field.is_required());
    assert_eq!(field.default_value(), Some(&serde_json::json!(false)));
}
```

在 `group_table.rs` 的测试里断言 `unique_named("uk_feishu_approval_group_config_key", [config_id, group_key])` 与 `uuid` 唯一。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib group_columns_and_paused_state_are_declared aggregate_batch_defaults_to_false_and_is_required`
Expected: FAIL（字段不存在/编译失败）。

- [ ] **Step 3: 实现声明**

`task_table.rs` 在 `uuid` 后加：

```rust
group_id => Str::new().title("聚合组键").max_length(64).filterable(true),
group_seq => Int::new().title("组内序号").require(true).default(0),
```

`state` 的 options 追加 `("paused", "已暂停")`，并把既有精确断言 `state_radio_carries_the_unnotified_value_alongside_terminal` 的期望列表同步加上 `"paused"`。`approval/table.rs` 在 `batch_scope_field` 附近加：

```rust
aggregate_batch => Switch::new()
    .title("同批聚合派发")
    .require(true)
    .default(false),
```

`group_table.rs` 照 `task_table.rs` 的结构声明设计 §4.2 的列与复合唯一键，并提供 `pub(crate) fn table_spec() -> Result<TableSpec, BaseError>`。`approval/mod.rs` 与 `feishu/mod.rs` 照现有 `build_task_module` 的形态装配。`tests/feishu_approval_dispatch_integration.rs` 的 `reset_database` 列表加 `feishu_approval_group`。

- [ ] **Step 4: 跑测试与启动 smoke**

Run: `cargo test --lib group_columns_and_paused_state_are_declared aggregate_batch_defaults_to_false_and_is_required`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/addon/feishu tests/feishu_approval_dispatch_integration.rs
git commit -m "feat(feishu): 聚合派发的组表与开关列"
```

---

### Task 2: 配置开关的后端 CRUD 与校验

**Files:**
- Modify: `src/addon/feishu/approval/actions/create_config.rs`
- Modify: `src/addon/feishu/approval/actions/update_config.rs`
- Modify: `src/addon/feishu/approval/actions/list_configs.rs`
- Modify: `src/addon/feishu/domain/approval_provision.rs`
- Modify: `src/infrastructure/feishu_approval_worker.rs`（播种 `select_fields`）

**Interfaces:**
- Consumes: 配置列 `aggregate_batch`（Task 1）。
- Produces: `pub(crate) fn check_aggregate_batch(aggregate_batch: bool, batch_scope_field_id: Option<&str>) -> Vec<String>`（`approval_provision.rs`）；`CreateConfigInput.aggregate_batch: bool`；`UpdateConfigInput.aggregate_batch: Option<bool>`；`ConfigItem.aggregate_batch: bool`。

- [ ] **Step 1: 写失败测试**

`create_config.rs` 测试模块：

```rust
#[test]
fn rejects_aggregate_without_batch_scope() {
    let errors = check_aggregate_batch(true, None);
    assert!(errors.iter().any(|error| error.contains("批量范围复选框列")));
}

#[test]
fn accepts_aggregate_with_batch_scope() {
    assert!(check_aggregate_batch(true, Some("fldScope")).is_empty());
}
```

`update_config.rs` 测试模块：用现有配置行夹具断言「只提交 `aggregate_batch=true` 而库内无范围列 → `ParamInvalid`」；「清空范围列同时聚合仍为 true → `ParamInvalid`」；「只提交 `aggregate_batch=false` → 成功且审计摘要含该键」。

`list_configs.rs`：断言 `CONFIG_ITEM_COLUMNS` 含 `"aggregate_batch"`，且投影 JSON 含该键。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib check_aggregate_batch list_`
Expected: FAIL（函数/字段不存在）。

- [ ] **Step 3: 实现**

- `approval_provision.rs` 加：

```rust
pub(crate) fn check_aggregate_batch(
    aggregate_batch: bool,
    batch_scope_field_id: Option<&str>,
) -> Vec<String> {
    if aggregate_batch && batch_scope_field_id.is_none() {
        vec!["开启同批聚合前必须先配置批量范围复选框列".to_string()]
    } else {
        Vec::new()
    }
}
```

- `create_config.rs`：输入加 `#[serde(default)] pub(super) aggregate_batch: bool`；`build_plan` 后调用 `check_aggregate_batch(input.aggregate_batch, plan.batch_scope_field_id.as_deref())`，错误并入现有 `ParamInvalid`；在 `insert_plan` 之后的同事务 `update_in_tx` 块里 `columns.insert("aggregate_batch", json!(input.aggregate_batch))`。
- `update_config.rs`：输入加 `aggregate_batch: Option<bool>`；加入「至少提供一个更新字段」清单（`update_config.rs:98-110`）；当本次提交了 `aggregate_batch` 或 `batch_scope_field` 时，在同一事务里把库内 `aggregate_batch` 与 `batch_scope_field` 一并查出，计算生效值后调用 `check_aggregate_batch`；写库与审计摘要同步加键。
- `list_configs.rs`：`CONFIG_ITEM_COLUMNS`、`ConfigItem`、手工赋值、响应 `json!` 四处加字段。
- `feishu_approval_worker.rs:928-938` 的 `select_fields` 加 `"aggregate_batch"`。

- [ ] **Step 4: 跑测试**

Run: `cargo test --lib check_aggregate_batch && cargo test --lib list_configs`
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
  - `fn prepare_definition<'a>(form_snapshot: &str, widgets: &'a [WidgetMap]) -> Result<(Vec<FormWidget>, BTreeMap<&'a str, &'a WidgetMap>), ConvertError>`（把 `build_form` 255-299 行的解析与映射校验抽出）
  - `pub(crate) fn build_signature(input: &FormInput<'_>) -> Result<String, ConvertError>`
  - `pub(crate) fn source_identities(widgets: &[WidgetMap], cells: &Map<String, Value>) -> Result<BTreeMap<String, Vec<String>>, ConvertError>`（每文件编码 `"<file_token>|<name>|<size>"`，保持源顺序）

- [ ] **Step 1: 写失败测试**

在 `approval_convert.rs` 测试模块：

```rust
#[test]
fn signature_ignores_detail_subtree_and_normalizes_values() {
    // 两个 cells：非明细字段数值写法不同（100 与 "100"），明细子控件值不同。
    let a = signature_for(record_with_non_detail(json!(100), "行A"));
    let b = signature_for(record_with_non_detail(json!("100"), "行B"));
    assert_eq!(a, b);
}

#[test]
fn signature_encodes_missing_required_as_null() {
    let missing = signature_for(record_with_required_absent());
    assert!(missing.contains("null"));
}

#[test]
fn signature_separates_different_attachment_files() {
    let a = signature_for(record_with_attachment("fileA", "a.pdf", 10));
    let b = signature_for(record_with_attachment("fileB", "a.pdf", 10));
    assert_ne!(a, b);
}
```

在 `approval_attachment.rs` 测试模块加 `source_identities_preserves_order_and_size`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib signature_ && cargo test --lib source_identities`
Expected: FAIL（函数不存在）。

- [ ] **Step 3: 实现**

- 抽 `prepare_definition`，`build_form` 改为调用它，行为不变。
- `build_signature`：`prepare_definition` → 从定义树收集顶层 `fieldList` 的 `walk()` widget_id 集合 → 递归遍历定义：遇到 `fieldList` 整棵跳过；遇到控件组递归子节点；其余已映射控件用 `assemble_widget`（cell 非空时）并收集 `{id,type,value}`；cell 为空（无论必填）写入 `{"id":..., "type":..., "value": null}`；按定义顺序写入 `BTreeMap<String, Value>` 后 `serde_json::to_string`。`FormInput.attachment_codes` 必须由调用方填入 `source_identities` 的结果。
- `source_identities`：对 `is_media` 控件调用 `attachments()`，把 `Attachment{file_token,name,size}` 编码为字符串列表。
- 签名禁止调用 `transfer`。

- [ ] **Step 4: 跑测试**

Run: `cargo test --lib signature_ && cargo test --lib source_identities`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/addon/feishu/domain/approval_convert.rs src/addon/feishu/domain/approval_attachment.rs
git commit -m "feat(feishu): 聚合派发的一致性签名"
```

---

### Task 4: 组键、分组规划与组表仓储

**Files:**
- Create: `src/addon/feishu/domain/approval_aggregation.rs`

**Interfaces:**
- Consumes: `approval_uuid::derive_uuid`、`FeishuContext::approval_tasks()` / 组表 module。
- Produces:
  - `pub(crate) fn signature_hash(signature_json: &str) -> String`（SHA-256 前 32 hex）
  - `pub(crate) fn group_key(config_id: i64, signature_hash: &str, record_ids: &[String]) -> String`（排序后拼接再哈希，返回 32 hex）
  - `pub(crate) fn group_uuid(base_token: &str, table_id: &str, approval_code: &str, group_key: &str) -> String`
  - `pub(crate) struct GroupCandidate { pub(crate) record_id: String, pub(crate) signature: String }`
  - `pub(crate) struct PlannedGroup { pub(crate) group_key: String, pub(crate) signature_hash: String, pub(crate) members: Vec<String> }`
  - `pub(crate) fn plan_groups(config_id: i64, candidates: &[GroupCandidate]) -> Vec<PlannedGroup>`（按 `signature` 分组，保持输入顺序）
  - 仓储：`insert_group`、`load_group`、`load_group_by_instance`、`load_group_by_uuid`、`save_created`、`save_backfilled`、`delete_group`、`delete_groups_for_config`、`load_members`、`set_members_state`、`list_unsubmitted_groups`（签名见设计 §4.1/§4.4；全部 `async fn ... (context: &FeishuContext, ...) -> anyhow::Result<...>`）

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn plan_groups_merges_identical_signatures_in_scan_order() {
    let groups = plan_groups(7, &[
        candidate("recA", "sig-1"), candidate("recB", "sig-2"), candidate("recC", "sig-1"),
    ]);
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].members, vec!["recA", "recC"]);
    assert_eq!(groups[1].members, vec!["recB"]);
}

#[test]
fn group_key_depends_on_member_set() {
    let a = group_key(7, "sig", &["recA".into(), "recB".into()]);
    let b = group_key(7, "sig", &["recA".into()]);
    assert_ne!(a, b);
    assert_eq!(a, group_key(7, "sig", &["recB".into(), "recA".into()]));
}

#[test]
fn group_uuid_is_deterministic_and_36_chars() {
    let uuid = group_uuid("base", "tbl", "APPROVAL", "k");
    assert_eq!(uuid, group_uuid("base", "tbl", "APPROVAL", "k"));
    assert_eq!(uuid.len(), 36);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib plan_groups_ group_key_ group_uuid_`
Expected: FAIL（模块不存在）。

- [ ] **Step 3: 实现**

纯函数按接口实现；仓储照 `approval_execution/repository.rs` 的裸 SQL 风格，每处裸 SQL 加 `// tenant-boundary: raw-sql feishu-approval-group-<id>`。`load_members` 用 `where_eq("config_id")` + `where_eq("group_id")`，按 `group_seq` 升序。

- [ ] **Step 4: 跑测试**

Run: `cargo test --lib plan_groups_ group_key_ group_uuid_`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/addon/feishu/domain/approval_aggregation.rs
git commit -m "feat(feishu): 聚合组键与组表仓储"
```

---

### Task 5: 播种分组

**Files:**
- Modify: `src/infrastructure/feishu_approval_worker.rs`
- Test: `src/infrastructure/feishu_approval_worker.rs` 内 `#[cfg(test)] #[ignore]`（真实 MySQL）

**Interfaces:**
- Consumes: `aggregate_batch`（Task 2）、`build_signature`/`source_identities`（Task 3）、`plan_groups` 与仓储（Task 4）。
- Produces: `async fn seed_aggregate_groups(runner: &RoundRunner, config: &Record, pending: &[&bitable::RecordItem], applicant: Option<&str>) -> anyhow::Result<SeedOutcome>`。

- [ ] **Step 1: 写失败的集成测试**

在 worker 模块的测试里（`#[ignore]`）：

```rust
#[tokio::test]
#[ignore = "real mysql"]
async fn aggregate_seed_creates_one_group_with_two_members() {
    // 同签名两条记录、一条不同签名，走 seed_aggregate_groups。
    // 断言：组表 2 行；同签名两行任务 group_id 相同、group_seq 为 0/1；
    //       不同签名单独成组；每条任务行 state=pending。
}

#[tokio::test]
#[ignore = "real mysql"]
async fn aggregate_seed_only_runs_on_batch_click() {
    // 单条点击（clicked_record_id=Some）不创建组行。
}

#[tokio::test]
#[ignore = "real mysql"]
async fn aggregate_off_keeps_one_instance_per_record() {
    // aggregate_batch=false：两条同签名记录仍是两条独立任务行，组表零行。
}

#[tokio::test]
#[ignore = "real mysql"]
async fn batch_click_dissolves_unsubmitted_groups_but_keeps_review_groups() {
    // 未提交组被解散重建；review/creating 组保持原组键与原成员。
}

#[tokio::test]
#[ignore = "real mysql"]
async fn submitted_group_with_cleared_member_serial_is_reopened_for_backfill() {
    // 组行已有 instance_code、某成员回填列被清空：点击后 leader 回到 pending、effect_phase=created。
}
```

把两个测试名登记进 `scripts/run_ci.py` 的 `INTEGRATION`。

- [ ] **Step 2: 跑测试确认失败**

Run: `YANG_SYSTEM_TEST_DATABASE_URL=... YANG_SYSTEM_TEST_REDIS_URL=... python scripts/run_ci.py integration`
Expected: FAIL（组行不存在）。

- [ ] **Step 3: 实现**

- `seed_config` 读取 `aggregate_batch: bool`；仅当 `aggregate_batch && matches!(trigger, RoundTrigger::Manual(_)) && clicked_record_id.is_none()` 时走 `seed_aggregate_groups`，否则原逻辑不变。
- `seed_aggregate_groups`：
  1. 用 `load_tasks_by_record` 排除已有任务行的记录（沿用现有 helper）。
  2. 逐条 `bitable::rekey_cells_by_field_id` → `approval_provision::resolve_external_options` → `source_identities` → `build_signature`；映射装载用 `approval_dispatch::widget_maps_from_rows`（`approval_dispatch.rs:1281`）。签名失败时用 `format!("error:{record_id}")` 作为签名（单独成组）。
  3. `plan_groups`；对每个组：若 `(config_id, group_key)` 已存在组行则跳过。
  4. **解散可安全解散的未提交组**：`list_unsubmitted_groups` 过滤 `effect_phase=ready` 且非 `review` 且无租约；组内已有自身 `instance_code` 或回填的行摘除 `group_id/group_seq`；其余成员行与组行删除。`effect_phase ∈ {creating, lookup, created}` 或 `review` 的组保持不动。
  5. **已提交组补回填**：组行 `instance_code` 非空、但任一成员回填列为空的组，重新打开 leader（`state=pending`、`effect_phase=created`）。
  6. 一个事务内 `insert_group` + 逐行插入 leader（`group_seq=0`）与成员（`group_seq=1..N`），`uuid` 按记录派生，`state=pending`。
  7. 对 `applicant` 给本轮 `touched` 的行落发起人（复用 `persist_applicant` 的既有作用域）。
- 批量点击时若 `aggregate_batch` 关闭，保持现有逐条播种。

- [ ] **Step 4: 跑测试**

Run: `python scripts/run_ci.py integration`
Expected: PASS（含两个新用例）。

- [ ] **Step 5: Commit**

```bash
git add src/infrastructure/feishu_approval_worker.rs scripts/run_ci.py
git commit -m "feat(feishu): 批量播种按签名分组"
```

---

### Task 6: 多行明细表单组装

**Files:**
- Modify: `src/addon/feishu/domain/approval_convert.rs`

**Interfaces:**
- Produces:
  - `pub(crate) struct GroupFormRow<'a> { pub(crate) cells: &'a Map<String, Value>, pub(crate) attachment_codes: &'a BTreeMap<String, Vec<String>> }`
  - `pub(crate) struct GroupFormInput<'a> { pub(crate) widgets: &'a [WidgetMap], pub(crate) form_snapshot: &'a str, pub(crate) rows: &'a [GroupFormRow<'a>], pub(crate) timezone_offset: FixedOffset }`
  - `pub(crate) fn build_group_form(input: &GroupFormInput<'_>) -> Result<String, ConvertError>`

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn group_form_emits_one_detail_row_per_record() {
    let form = group_form_for(&["recA", "recB"]);
    let detail = find_widget(&form, "detail");
    assert_eq!(detail["value"].as_array().unwrap().len(), 2);
}

#[test]
fn nested_field_list_inside_a_row_stays_single_row() {
    // workGroup 内嵌 fieldList：顶层两条记录，每条记录里的嵌套明细仍 1 行。
}

#[test]
fn group_form_uses_first_row_for_non_detail_widgets() {
    // 非明细控件取 rows[0] 的值。
}

#[test]
fn group_form_omits_optional_detail_only_when_all_rows_empty() {
    // 任一行有值就输出 N 行；全部为空且可选取舍容器。
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib group_form_ nested_field_list_inside_a_row`
Expected: FAIL。

- [ ] **Step 3: 实现**

把 `assemble_definition` 改成 `assemble_definition(definition, mappings, rows: &[GroupFormRow<'_>], in_detail_row: bool)`：顶层 `fieldList` 在 `in_detail_row=false` 时对每行判 `has_value` 并 push 一行；`in_detail_row=true` 时只用 `rows[0]` push 一行；非明细/控件组用 `rows[0]`。`build_form` 包装成单行 `GroupFormRow` 保持签名不变；`build_group_form` 传全部行。

- [ ] **Step 4: 跑测试**

Run: `cargo test --lib group_form_ && cargo test --lib approval_convert`
Expected: PASS（既有单行断言同步更新为单行输入的等价结果）。

- [ ] **Step 5: Commit**

```bash
git add src/addon/feishu/domain/approval_convert.rs
git commit -m "feat(feishu): 多行明细表单组装"
```

---

### Task 7: 认领过滤、暂停态重开与删除保护

**Files:**
- Modify: `src/addon/feishu/domain/approval_execution/repository.rs`
- Modify: `src/addon/feishu/approval/actions/delete_config.rs`
- Test: 两个文件内的 `#[ignore]` 真实库测试

**Interfaces:**
- Consumes: 任务列 `group_seq`、状态 `paused`（Task 1）。
- Produces: `claim_ready` 新增语义：`only_id` 为 `None` 时只认 `group_seq=0`；`Some(id)` 时不受限制。

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test]
#[ignore = "real mysql"]
async fn claim_ready_skips_group_members_in_batch_mode() { /* member 不被认领，leader 被认领 */ }

#[tokio::test]
#[ignore = "real mysql"]
async fn claim_ready_by_id_allows_group_member() { /* only_id 命中 group_seq=1 */ }

#[tokio::test]
#[ignore = "real mysql"]
async fn reopen_resets_paused_rows() { /* state=paused → pending */ }

#[tokio::test]
#[ignore = "real mysql"]
async fn delete_config_rejects_active_paused_group() { /* 返回「仍有在途任务」 */ }

#[tokio::test]
#[ignore = "real mysql"]
async fn delete_config_removes_group_rows_and_members() { /* 非活跃配置删除后组表与该配置任务行均为空 */ }
```

登记进 `scripts/run_ci.py` 的 `INTEGRATION`。

- [ ] **Step 2: 跑测试确认失败**

Run: `python scripts/run_ci.py integration`
Expected: FAIL。

- [ ] **Step 3: 实现**

- `claim_ready` 的 SELECT 加 `AND (? IS NOT NULL OR group_seq = 0)`，把 `only_id` 绑定一次（现有 SQL 已绑定两次，改绑三次中的对应位置）。
- 过期租约清扫与 `finish` 不变。
- `reopen` 的 `state IN (...)` 加 `'paused'`。
- `delete_config` 的活跃状态白名单加 `"paused"`；删除任务行的同一事务里调用 `approval_aggregation::delete_groups_for_config` 删除组表行。

- [ ] **Step 4: 跑测试**

Run: `python scripts/run_ci.py integration`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/addon/feishu/domain/approval_execution/repository.rs src/addon/feishu/approval/actions/delete_config.rs scripts/run_ci.py
git commit -m "feat(feishu): 聚合组的认领过滤与暂停态"
```

---

### Task 8: 组派发（创建、回捞、批量回填）

**Files:**
- Modify: `src/addon/feishu/domain/approval_dispatch.rs`

**Interfaces:**
- Consumes: `build_group_form`（Task 6）、组表仓储（Task 4）、`Backfill::write_many`。
- Produces:
  - `pub(crate) struct GroupMemberInput<'a> { pub(crate) record_id: &'a str, pub(crate) cells: &'a Map<String, Value>, pub(crate) attachment_codes: BTreeMap<String, Vec<String>> }`
  - `pub(crate) struct GroupDispatchInput<'a>`：复制 `DispatchInput` 除 `record_id/cells/applicant` 外的全部字段（`fields, context, execution, coordinates, applicant_field, backfill_field_name, 六个可选字段, approval_code, widgets, form_snapshot, timezone_offset`），追加 `group_key: &'a str`、`group_uuid: &'a str`、`instance_code: Option<&'a str>`、`applicant_open_id: &'a str`、`members: &'a [GroupMemberInput<'a>]`
  - `pub(crate) async fn dispatch_group(transport: &dyn OutboundTransport, sleeper: &dyn Sleeper, tokens: &TenantTokenProvider, backfill: &dyn Backfill, input: &GroupDispatchInput<'_>) -> DispatchResult`

- [ ] **Step 1: 写失败测试**（用现有 `ReplayTransport`/假 Backfill 形态）

```rust
#[tokio::test]
async fn group_dispatch_sends_one_create_with_two_detail_rows() { /* 断言 create 请求 body 的 fieldList value 长度 2 */ }

#[tokio::test]
async fn group_dispatch_recovers_by_group_uuid_on_60012() { /* create 返回 60012，随后 get(uuid) 命中 */ }

#[tokio::test]
async fn group_dispatch_backfills_all_members_with_same_serial() { /* write_many 收到 N 条同值 */ }

#[tokio::test]
async fn group_dispatch_backfills_more_than_hundred_members_in_chunks() {
    // 150 个成员，第二批首次失败一次：重试后全部成员收到同一编号。
}

#[tokio::test]
async fn group_dispatch_returns_waiting_on_convert_error() { /* 空明细 → Waiting 且零 create */ }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib group_dispatch_`
Expected: FAIL。

- [ ] **Step 3: 实现**

- 恢复分支：`input.instance_code` 或 `effect_phase ∈ {creating, lookup}` 时按实例码/组 uuid `get_instance`，跳过创建与签名复核，取 `serial_number` 后回填。
- 否则 `build_group_form`（附件 code 由调用方 transfer 后填入）→ `validate_connections` → `create_instance`（`uuid = input.group_uuid`）→ `60012` 按 uuid 回捞 → 落实例码 → `get_instance` 取编号 → `backfill_in_chunks` 写全部成员 → `DispatchResult::Backfilled`。
- 转换错误返回 `DispatchResult::Waiting`（由 worker 决定暂停），可重试出站错误返回 `Retryable`。

- [ ] **Step 4: 跑测试**

Run: `cargo test --lib group_dispatch_`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/addon/feishu/domain/approval_dispatch.rs
git commit -m "feat(feishu): 组实例创建与批量回填"
```

---

### Task 9: worker 组处理、暂停与通知

**Files:**
- Modify: `src/infrastructure/feishu_approval_worker.rs`
- Test: worker 内 `#[ignore]` 真实库测试 + 单元测试

**Interfaces:**
- Consumes: `dispatch_group`（Task 8）、`build_group_form`、组表仓储（Task 4）、任务状态 `paused`（Task 1）。
- Produces: `async fn process_group(runner: &RoundRunner, task: &ClaimedTask) -> TaskOutcome`；`fn group_pause_message(group_size: usize, failing: &[(String, String)]) -> String`。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn pause_message_mentions_reclick() {
    let message = group_pause_message(2, &[("第 12 行".into(), "明细必填缺失".into())]);
    assert!(message.contains("重新点击"));
}

#[tokio::test]
#[ignore = "real mysql"]
async fn group_pause_writes_no_backfill_and_marks_all_members_paused() { /* 两条成员行都 paused，回填列仍空 */ }

#[tokio::test]
#[ignore = "real mysql"]
async fn group_success_backfills_every_member_with_same_serial() { /* 两条记录同编号，组行有 instance_code */ }

#[tokio::test]
#[ignore = "real mysql"]
async fn group_notification_is_sent_once_for_the_group() { /* 请求日志里只有一条组通知 */ }
```

登记进 `scripts/run_ci.py`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib pause_message_ && python scripts/run_ci.py integration`
Expected: FAIL。

- [ ] **Step 3: 实现**

- `process_one`：`task.group_id.is_some()` 时转 `process_group`。
- `process_group`：`load_members` → 按 100 分片 `records/batch_get` → `rekey_cells_by_field_id` → 重新解析外部选项（冲突判配置错误）→ 逐成员 `preflight` → 逐成员 `transfer` 拿真实 code → 复核签名（仅 `instance_code` 为空且 `attempts=0` 且 `effect_phase=ready`）→ `dispatch_group` → 按结论持久化：
  - `Backfilled`：`save_backfilled` + 全部成员行 `state=backfilled, serial_number=...`。
  - `Waiting/Terminal`：`set_members_state("paused", Some(reason))`，不写任何回填列。
  - `Retryable`：保持 `pending`，由现有退避逻辑处理。
  - `CreatedPendingBackfill`：`save_created`，成员行保持 `pending`，`effect_phase=created`。
- 通知：组只发一条，跳过 `http_path_notified` 的请求日志去重；暂停消息用 `group_pause_message` 附「修好后请重新点击批量派发」。行号编辑按每成员生成（复用 `approval_row_edit::RowNumberEdit`）。成功消息附组内条数与成员行号摘要。

- [ ] **Step 4: 跑测试**

Run: `cargo test --lib pause_message_ && python scripts/run_ci.py integration`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/infrastructure/feishu_approval_worker.rs scripts/run_ci.py
git commit -m "feat(feishu): 组任务处理、暂停与组级通知"
```

---

### Task 10: 事件 fan-out 与 uuid 兜底

**Files:**
- Modify: `src/infrastructure/feishu_event_worker.rs`
- Test: 文件内 `#[ignore]` 真实库测试

**Interfaces:**
- Consumes: `load_group_by_instance` / `load_group_by_uuid` / `load_members`（Task 4）。
- Produces: `async fn fan_out_group_event(runner: &EventRunner, group: &Record, event: &PendingEvent) -> EventOutcome`。

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test]
#[ignore = "real mysql"]
async fn event_by_instance_code_updates_all_member_records() { /* 两条成员扩展字段都写 */ }

#[tokio::test]
#[ignore = "real mysql"]
async fn event_arriving_before_instance_code_uses_uuid_fallback() { /* 事件 uuid = 组 uuid，仍命中 */ }
```

登记进 `scripts/run_ci.py`。

- [ ] **Step 2: 跑测试确认失败**

Run: `python scripts/run_ci.py integration`
Expected: FAIL。

- [ ] **Step 3: 实现**

- 任务表按 `instance_code` 未命中后，按 `instance_code` 查组表；`resolve_missing_task` 的 uuid 兜底追加 `load_group_by_uuid`。
- 命中组后取成员行，逐成员构造 `DispatchInput` 并调用 `approval_dispatch::write_extended`（`approval_dispatch.rs:999`），同时更新成员行 `instance_status/last_synced_at`。不为组另写一套扩展字段转换。

- [ ] **Step 4: 跑测试**

Run: `python scripts/run_ci.py integration`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/infrastructure/feishu_event_worker.rs scripts/run_ci.py
git commit -m "feat(feishu): 组实例事件 fan-out 与 uuid 兜底"
```

---

### Task 11: 前端开关与契约

**Files:**
- Modify: `frontend/src/features/feishu/components/ApprovalConfigDialog.tsx`
- Modify: `frontend/src/features/feishu/api.ts`
- Modify: `frontend/src/features/feishu/types.ts`
- Modify: `frontend/src/features/feishu/components/ApprovalConfigDetails.tsx`
- Modify: `frontend/tests/features/feishu/components/approval-config-dialog.test.tsx`
- Modify: `frontend/e2e/approval-config-business.spec.ts`、`frontend/e2e-production/approval-config-business.spec.ts`（夹具补字段）
- Generated: `frontend/contracts/openapi.json`、`frontend/src/engine/contracts/api-types.ts`

**Interfaces:**
- Consumes: `create_config`/`update_config` 的 `aggregate_batch` 字段（Task 2）、`list_configs` 的 `aggregate_batch`。

- [ ] **Step 1: 写失败的 Vitest**

在 `approval-config-dialog.test.tsx` 加：

```ts
it("does not allow aggregate without a batch scope column", async () => { /* 未选范围列时复选框 disabled */ });
it("sends aggregate_batch on create and as a patch on update", async () => { /* 载荷键断言 */ });
```

- [ ] **Step 2: 跑测试确认失败**

Run: `pnpm --dir frontend test -- approval-config-dialog`
Expected: FAIL。

- [ ] **Step 3: 实现**

- 对话框：`const [aggregateBatch, setAggregateBatch] = useState(initialConfig?.aggregateBatch ?? false)`；第 3 步在批量范围控件下方加 Checkbox「同批聚合派发」，`disabled={!batchScope}`；提交时 create 带 `aggregateBatch`，update 按原值比较决定是否发送（`undefined` = 不改）。
- `api.ts`：`CreateApprovalConfigInput.aggregateBatch?: boolean`；`UpdateApprovalConfigPatch.aggregateBatch?: boolean`；wire 键 `aggregate_batch`；解析 `raw.aggregate_batch`。
- `types.ts`：`ApprovalConfigItem.aggregateBatch?: boolean`。
- 详情页加只读行「同批聚合派发：是/否」。
- E2E 夹具补 `aggregate_batch: false`。
- 跑 `python scripts/dump_openapi.py` 生成契约并提交两个生成物。

- [ ] **Step 4: 跑检查**

Run: `pnpm --dir frontend check`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add frontend
git commit -m "feat(feishu): 聚合开关的前端配置"
```

---

### Task 12: 文档、全量门禁与真机验证

**Files:**
- Modify: `AGENTS.md`（feishu 表/配置清单）
- Modify: `docs/architecture/2026-09-28-feishu-approval-dispatch-design.md`（§5.1/§5.2 指向新设计）
- Modify: `src/addon/feishu/approval/domain/task_table.rs` 等处的既有测试（如断言列表需补值）

- [ ] **Step 1: 更新文档**

在 `AGENTS.md` 的 feishu 段补：`feishu_approval_group` 纯绑定表、config 的 `aggregate_batch`、task 的 `group_id/group_seq`、状态 `paused`；在 2026-09-28 设计 §5.1/§5.2 各加一句「同批聚合见 2026-10-31 设计」。

- [ ] **Step 2: 静态与单元门禁**

Run: `python scripts/run_ci.py quick`
Expected: PASS（架构检查、fmt、lib 单测、前端 typecheck/Vitest）。

- [ ] **Step 3: 真实依赖集成**

Run: `python scripts/run_ci.py integration`
Expected: PASS（含任务 5/7/9/10 的全部 `#[ignore]` 用例）。

- [ ] **Step 4: 推送前全量**

Run: `python scripts/run_ci.py full`
Expected: PASS（clippy `-D warnings`、前端完整 check、两套 Playwright）。

- [ ] **Step 5: 摘除式变异**

对以下守卫逐条删除并确认对应测试变红，再恢复：`aggregate_batch` 短路、`group_seq=0` 认领过滤、签名不含明细、`60012` 组回捞、暂停不写回填、组事件 uuid 兜底。把结果写进 PR 说明。

- [ ] **Step 6: 真机验证（部署后，技术测试定义）**

按设计 §9 的清单执行：2/10/50/100/200 行明细创建与回读、空明细成员、嵌套明细、多行附件、外部选项、组事件推送、清空编号重试。行数上限与超长请求体是阻塞上线的验证项。

- [ ] **Step 7: Commit**

```bash
git add AGENTS.md docs src
git commit -m "docs(feishu): 聚合派发文档与门禁"
```

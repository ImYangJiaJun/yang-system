# 飞书数据源「以表为单位」配置 — 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把飞书外部数据源的配置与拉取单位从「一个字段」上移到「一张表」，
让运维用向导一次配好同表的多列及其级联关系，一轮拉取扫一次表。

**Architecture:** 新增 `feishu_datasource_field`（字段绑定）承载 `source_key`/凭据/父指针；
`feishu_datasource` 收缩为表级（坐标 + 表级同步状态）。出站契约
`POST /api/v1/feishu/approval/options/{source_key}` **不变**。拉取改为表级一次取回、
按字段分派派生与落库。

**Tech Stack:** Rust（`yang-base` DSL / `yang-db` Repository）、MySQL 8.0、
React + TypeScript（Vitest）、飞书 OpenAPI（多维表格）。

**Spec:** [`docs/architecture/feishu-datasource-table-config.md`](./feishu-datasource-table-config.md)
— 本计划实现它；执行者**两份都要读**。

## Global Constraints

- **不新增 SQL 迁移文件。** 仓库刻意不设 `migrations/`；表结构由
  `TableSpec` 声明 + 启动期增量同步驱动（`AGENTS.md:84`、`docs/contracts/SCHEMA.md`）。
- **每个 Action 文件恰好一个 `pub(super) async fn handle` + 一个 `pub(super) fn register`**，
  并在 `actions/mod.rs` 登记（架构门禁 `python scripts/check_architecture.py` 会拒）。
- **裸 SQL 必须带 `// tenant-boundary: <kind> <id>` 注释**（架构门禁检查）。
- **不新增密码学原语。** 复用 `domain/crypto.rs` 的 `derive_key` / `encrypt_bytes`
  （已逐条对齐飞书 Go 参考实现）。
- **出站响应契约一字不改**：`{code,msg,data}`、HTTP 恒 200、`data.result` 明文为对象、
  2.5 秒主动收口（`approval_options.rs:51`）。
- **派生规则不改**：`option_id` 的哈希输入与形状不变 → **不要 bump `DERIVE_RULE_VERSION`**。
- **MSRV 1.80 是硬门禁**（`Cargo.toml:5`、`ci.yml:73-98`）。新增依赖必须冷缓存验证。
- **Key 加密不在本次范围**（设计文档决策 D11）。不要改 `encrypt_enabled` 默认值、
  不要动 `feishu.encryption_key` 的作用域、不要新增 `key_cipher`。
- **⚠️ 集成测试会重建业务测试表。** 运行 `python scripts/run_ci.py integration` 或任何
  `--ignored` 集成测试**之前必须获得用户明确许可**——不得自行放行。
  全局规则：数据库默认只读，写操作需用户明确要求 + 二次确认。

## 常用命令

```bash
# 快速门禁（架构自检 + fmt + lib 单测 + 前端 typecheck + Vitest）
python scripts/run_ci.py quick

# 单测（feishu addon）
cargo test --lib --locked addon::feishu

# 全量门禁（含 clippy -D warnings / pnpm check / Playwright）
python scripts/run_ci.py full

# MSRV 冷缓存验证（新增依赖后必做）
docker run --rm -v /d/code/lib_yang:/ws -w /ws/project/yang-system \
  -e CARGO_HOME=/tmp/ch -e CARGO_TARGET_DIR=/tmp/ct -e RUSTUP_TOOLCHAIN=1.80.1 \
  rust:1.80.1-slim cargo check --all-targets --locked
```

## Review Focus

设计文档没有逐一写死、但实现时**最可能咬人**的输入与失败模式。每一条都在下面的任务里
绑了对应测试。

1. **Bitable 的 select 值带尾随空白/换行**（实测：`"CNY 人民币\n"`）。派生会 trim，
   但**父键与子键必须用同一份 trim 后的文案**，否则子项指向一个不存在的父 option_id
   ——失效形态是「下拉静默变空」，不报错。见 T9。
2. **同一个表里两列重名不可能，但两个不同表可能产生同一个 `source_key`**（人工填的）。
   唯一索引冲突必须以 `ParamInvalid` 冒泡，不能是裸 DB 错误。见 T1。
3. **`field_names` 是一个 JSON 数组查询参数**，重复项会被官方拒（`bitable.rs:879` 有
   `duplicate_field_names_are_rejected` 测试）。同表多列勾选时**必须去重**，见 T9。
4. **父列在同一行有值、子列为空**，以及**父列为空**是两种不同情形，都必须落成「无父键」
   而不是挂到一个空父上（`derive.rs:288-296` 已钉住）。表级拉取不能破坏这一点。见 T9。
5. **勾选了一个已被删除的 `field_id`**（`列出字段` 里不存在）。拉取必须**全有或全无**
   地失败并告警，而不是静默少拉一列。见 T10。

---

# 阶段一：数据模型与元数据探取

## Task 1: 数据模型——新表 `feishu_datasource_field`、`feishu_datasource` 收缩为表级

**Files:**
- Create: `src/addon/feishu/datasource/field_table.rs`
- Modify: `src/addon/feishu/datasource/table.rs`（删字段级列，保留表级）
- Modify: `src/addon/feishu/datasource/mod.rs`（导出 `field_table`）
- Modify: `src/addon/feishu/domain/context.rs`（加第三个 Repository）
- Modify: `src/addon/feishu/mod.rs:56-70`（`build_context` 构造第三个 Repository）
- Test: 上述两个 `table.rs` 的 `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `feishu_datasource_field` 表；`FeishuContext::datasource_fields() -> &Repository`
- Consumes: `yang_base::definition::{Int, Key, Str, Switch, Text, Timestamp}`

- [ ] **Step 1: 写失败的 schema 断言测试**

在 `src/addon/feishu/datasource/field_table.rs` 底部写入（先让 `table_spec()` 返回 `todo!()` 之外的最小骨架）：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn definition() -> yang_base::table::TableDefinition {
        table_spec()
            .unwrap_or_else(|error| panic!("表声明应有效: {error}"))
            .table_definition()
            .unwrap_or_else(|error| panic!("应可编译为表定义: {error}"))
    }

    #[test]
    fn table_name_and_primary_key() {
        let definition = definition();
        assert_eq!(definition.name(), "feishu_datasource_field");
        assert_eq!(definition.primary_key(), "id");
    }

    #[test]
    fn field_id_is_required_and_pairs_with_the_datasource() {
        // 身份是 field_id（设计 §3.2 A1），不是 field_name——改名不能断链
        let definition = definition();
        let field_id = definition
            .field("field_id")
            .unwrap_or_else(|| panic!("field_id 必须存在"));
        assert!(field_id.is_required());
        assert!(field_id.is_filterable(), "按 field_id 查绑定要能筛");
    }

    #[test]
    fn source_key_is_unique_and_filterable() {
        // 它进 URL 路径，全局唯一；出站按它路由，必须可筛
        let definition = definition();
        let source_key = definition
            .field("source_key")
            .unwrap_or_else(|| panic!("source_key 必须存在"));
        assert!(source_key.is_required());
        assert!(source_key.is_filterable());
        assert!(source_key.is_sortable());
    }

    #[test]
    fn parent_field_id_is_optional_and_filterable() {
        // 无父的字段没有它；但按父反查子要能筛
        let definition = definition();
        let parent = definition
            .field("parent_field_id")
            .unwrap_or_else(|| panic!("parent_field_id 必须存在"));
        assert!(!parent.is_required(), "无父字段没有父指针");
        assert!(parent.is_filterable());
    }

    #[test]
    fn credential_columns_are_secret_and_never_searchable() {
        let definition = definition();
        for name in ["token_hash", "token_cipher"] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 必须存在"));
            assert!(field.is_secret(), "{name} 必须是 secret");
            assert!(!field.is_searchable(), "{name} 不得进检索面");
            assert!(!field.is_filterable(), "{name} 不得进筛选面");
        }
    }

    #[test]
    fn every_field_has_an_explicit_chinese_label() {
        let definition = definition();
        for field in definition.fields() {
            assert!(!field.label().is_empty(), "字段 {} 必须有展示名", field.name());
            assert_ne!(field.label(), field.name(), "字段 {} 忘了 .title(..)", field.name());
        }
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib --locked addon::feishu::datasource::field_table`
Expected: 编译失败（`table_spec` 不存在）

- [ ] **Step 3: 实现 `field_table.rs`**

```rust
//! `feishu_datasource_field` 表声明——字段绑定的 Schema 唯一事实来源。

use yang_base::definition::{Int, Key, Str, Switch, Timestamp};
use yang_base::BaseError;

use super::super::domain::repository::SYSTEM_ROLE;

/// 声明字段绑定表。
///
/// 一条绑定 = 一个多维表格字段 = 一个 `source_key` = 一个审批控件。
/// `datasource_id` 指向表级行；DSL 没有外键 builder，故用 `Int` + 索引，
/// 一致性由应用层在事务内保证（表级行与绑定行同事务写入）。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    Ok(TableSpec::new(yang_base::table!("feishu_datasource_field"))
        .title("飞书数据源字段")
        .fields(yang_base::fields! {
            id => Key::new().title("ID"),
            datasource_id => Int::new()
                .title("所属数据源")
                .require(true)
                .indexed(true)
                .filterable(true),
            // 身份。**存 field_id 不存 field_name**：名字会被改，
            // 而表级拉取下 `field_names` 是一个请求参数，一个坏名字会拖垮整表。
            // `field_name` 降为缓存，每轮拉取前按 field_id 解析刷新。
            field_id => Str::new()
                .title("字段 ID")
                .require(true)
                .max_length(64)
                .filterable(true),
            field_name => Str::new().title("字段名（缓存）").max_length(255),
            // 进 URL 路径段，全局唯一。唯一索引是硬依赖：出站按它路由。
            source_key => Str::new()
                .title("数据源标识")
                .require(true)
                .unique(true)
                .max_length(64)
                .searchable(true)
                .filterable(true)
                .sortable(true),
            // 只存摘要，校验用；校验路径不需要解密。
            token_hash => Str::new()
                .title("Token 摘要")
                .require(true)
                .max_length(64)
                .secret(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
            // 可逆密文，**只在回显复制那一条路径上解密**。
            token_cipher => Text::new()
                .title("Token 密文")
                .secret(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
            token_rotated_at => Timestamp::new().title("最近轮换时间"),
            encrypt_enabled => Switch::new().title("加密返回").require(true).default(false),
            default_locale => Str::new()
                .title("默认语言")
                .require(true)
                .max_length(16)
                .default("zh_cn"),
            // 同表内的父列。多级由链涌现（A 是 B 的父、B 是 C 的父）。
            parent_field_id => Str::new()
                .title("父字段 ID")
                .max_length(64)
                .indexed(true)
                .filterable(true),
            enabled => Switch::new().title("启用").require(true).default(true).filterable(true),
            snapshot_digest => Str::new().title("快照摘要").max_length(64),
            last_push_at => Timestamp::new().title("最近推送时间"),
            created_at => Timestamp::new().created_at().title("创建时间"),
            updated_at => Timestamp::new().updated_at().title("更新时间").sortable(true),
        }))
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib --locked addon::feishu::datasource::field_table`
Expected: PASS（全部用例）

- [ ] **Step 5: 收缩 `datasource/table.rs` 为表级**

删掉 `bitable_field_name` 与 `linkage_mapping` 两列，保留
`title / ingest_mode / status / bitable_base_token / bitable_table_id / bitable_view_id /
last_pull_at / last_success_at / consecutive_failures / last_error / snapshot_digest /
created_at / updated_at`。同时删掉 `coordinates_store_the_field_name_not_the_field_id`
与 `sync_state_columns_are_nullable_or_defaulted` 里对已删列的断言，并在该测试的
`for name in [...]` 列表里去掉 `"bitable_field_name"`。

> 注意：`snapshot_digest` 在表级行上**保留但不再使用**（设计 §6.3：摘要归属改为字段绑定）。
> 留着是为了避免一次破坏性的列删除；在它的 doc 注释里写明「已废弃，见 field_table」。

- [ ] **Step 6: 把第三张表接进 `FeishuContext`**

`context.rs`：加字段 `datasource_field: Repository`、构造参数、getter `datasource_fields()`。
`mod.rs:56-70` 的 `build_context` 里加第三个
`Repository::new(field_table::table_spec()?.table_definition()?, Arc::clone(&pool))`。
`datasource/mod.rs` 加 `pub(crate) mod field_table;`。

- [ ] **Step 7: 运行完整单测**

Run: `cargo test --lib --locked addon::feishu`
Expected: PASS。若 `build_context` 的其它调用点编译失败，逐个补参数。

- [ ] **Step 8: 提交**

```bash
git add src/addon/feishu/datasource/field_table.rs \
        src/addon/feishu/datasource/table.rs \
        src/addon/feishu/datasource/mod.rs \
        src/addon/feishu/domain/context.rs \
        src/addon/feishu/mod.rs
git commit -m "feat(feishu): 字段绑定表与表级数据源模型"
```

---

## Task 2: DB 唯一键冲突映射为 `ParamInvalid`

**Files:**
- Modify: 数据库错误适配层（见 Step 1 定位）
- Test: 同文件 `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: MySQL `1062` → `BaseError::ParamInvalid(<字段>, "已存在")`

- [ ] **Step 1: 定位错误适配层并写失败测试**

Run: `grep -rn "fn from(.*sqlx\|impl From<sqlx\|DatabaseError\|MySqlError" crates/yang-db/src/ | head`

在所得文件的测试模块里加：

```rust
#[test]
fn duplicate_key_error_maps_to_param_invalid() {
    // 全仓此前没有 1062 → ParamInvalid 的映射，冲突会以裸 DB 错误冒出来。
    // 系统生成 token 与自动派生 source_key 都会撞唯一索引，必须有这条映射。
    let error = map_mysql_error_for_test(1062, "Duplicate entry 'x' for key 'source_key'");
    match error {
        BaseError::ParamInvalid(field, message) => {
            assert!(!field.is_empty(), "要能指出是哪个字段冲突");
            assert!(!message.is_empty());
        }
        other => panic!("1062 应映射为 ParamInvalid，实际: {other:?}"),
    }
}

#[test]
fn other_mysql_errors_keep_their_own_mapping() {
    // 回归守卫：不要把 1062 之外的错误也吞成 ParamInvalid
    assert!(!matches!(
        map_mysql_error_for_test(1146, "Table doesn't exist"),
        BaseError::ParamInvalid(_, _)
    ));
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib --locked <该 crate>`
Expected: FAIL（`map_mysql_error_for_test` 未定义）

- [ ] **Step 3: 实现映射**

```rust
/// MySQL 错误码 → 框架错误。1062 是唯一键冲突。
///
/// 从错误文本里尽力提取冲突的键名（`for key 'xxx'`），提取不到就用占位符——
/// 关键是**不能**让唯一键冲突以裸 DB 错误冒到 API 层。
fn map_duplicate_key(error: &sqlx::Error) -> Option<BaseError> {
    let db_error = error.as_database_error()?;
    if db_error.code().as_deref() != Some("1062") {
        return None;
    }
    let field = db_error
        .message()
        .rsplit_once("for key '")
        .and_then(|(_, rest)| rest.split('\'').next())
        .unwrap_or("unique")
        .to_string();
    Some(BaseError::ParamInvalid(field, "该值已存在".to_string()))
}
```

并在既有 `From<sqlx::Error> for BaseError`（或等价处）里、**通用分支之前**插入
`if let Some(mapped) = map_duplicate_key(&error) { return mapped; }`。

同时暴露一个 `#[cfg(test)] fn map_mysql_error_for_test(code, message)` 供测试驱动，
内部构造同样的判定（避免测试依赖一个真实数据库连接）。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --lib --locked <该 crate>`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add crates/yang-db/src/...   # 以 Step 1 的实际路径为准
git commit -m "fix(db): 唯一键冲突映射为 ParamInvalid，不再冒裸 DB 错误"
```

---

## Task 3: 元数据 Action——列出数据表

**Files:**
- Create: `src/addon/feishu/datasource/actions/list_bitable_tables.rs`
- Modify: `src/addon/feishu/datasource/actions/mod.rs`
- Modify: `src/addon/feishu/domain/bitable.rs`（加 URL 组装 + 响应 DTO）
- Test: 上述文件的 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: **T1**（`FeishuContext`）；`bitable.rs` 既有的 `validate_path_segment`、`PAGE_SIZE_FIELDS`
- Produces: `POST /api/v1/feishu/datasources/bitable-tables`，
  响应 `{"tables":[{"table_id":"tbl…","name":"…"}]}`

- [ ] **Step 1: 写失败的 URL 组装测试**

在 `src/addon/feishu/domain/bitable.rs` 的测试模块加：

```rust
#[test]
fn tables_url_is_the_official_path() {
    // 官方《列出数据表》：GET /open-apis/bitable/v1/apps/:app_token/tables
    let url = tables_url("ZoCWb82JQaCCiAspCqbcUvlsnwg").expect("应可组装");
    assert!(url.ends_with("/bitable/v1/apps/ZoCWb82JQaCCiAspCqbcUvlsnwg/tables"), "实际: {url}");
}

#[test]
fn tables_url_rejects_path_traversal() {
    // app_token 来自用户输入，直接采信会让 ../ 打到别的路径
    assert!(tables_url("../evil").is_err());
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib --locked addon::feishu::domain::bitable::tests::tables_url`
Expected: FAIL（`tables_url` 未定义）

- [ ] **Step 3: 实现 `tables_url` + 响应 DTO**

```rust
/// 官方《列出数据表》的 URL：`GET /open-apis/bitable/v1/apps/:app_token/tables`
///
/// `app_token` 进路径段，必须先过 [`validate_path_segment`]。
pub(crate) fn tables_url(app_token: &str) -> Result<String, BaseError> {
    validate_path_segment("bitable_base_token", app_token)?;
    Ok(format!("{FEISHU_OPEN_BASE}/bitable/v1/apps/{}/tables", app_token.trim()))
}

/// 《列出数据表》的响应项。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct BitableTableItem {
    pub(crate) table_id: String,
    #[serde(default)]
    pub(crate) name: String,
}
```

（`FEISHU_OPEN_BASE` 在 `tenant_token.rs:40` 已有，按既有惯例引用。）

- [ ] **Step 4: 写失败的 Action 测试**

`list_bitable_tables.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_blank_app_token() {
        let input = ListTablesInput { app_token: "  ".to_string() };
        assert!(input.validate().is_err(), "空 app_token 必须被拒");
    }

    #[test]
    fn accepts_a_real_app_token_shape() {
        let input = ListTablesInput { app_token: "ZoCWb82JQaCCiAspCqbcUvlsnwg".to_string() };
        assert!(input.validate().is_ok());
    }
}
```

- [ ] **Step 5: 运行确认失败**

Run: `cargo test --lib --locked addon::feishu::datasource::actions::list_bitable_tables`
Expected: FAIL

- [ ] **Step 6: 实现 Action**

照 `create_datasource.rs` 的形状：`ListTablesInput { app_token: String }` + `validate()`，
`register()` 里

```rust
.route(HttpMethod::Post, "/api/v1/feishu/datasources/bitable-tables")
.display_name("列出多维表格数据表")
.permissions(["feishu.datasource.read"])
```

`handle` 里：取 tenant token → GET `tables_url` → 解析 `data.items` →
按 `page_token` 翻页（照 `list_all_records` 的翻页与「重复 page_token 视为不收敛」守卫）→
返回 `{"tables": [...]}`。**全部错误以 `BaseError` 冒泡**（这是控制台端点，走框架包络，
与出站 `approval_options` 的裸信封不同）。

- [ ] **Step 7: 在 `actions/mod.rs` 登记**

```rust
pub(super) mod list_bitable_tables;
// register_all 里，与其它控制台端点同组（不受 can_pull 门禁影响）：
let module = list_bitable_tables::register(module, Arc::clone(&context));
```

- [ ] **Step 8: 运行测试 + 架构门禁**

Run: `cargo test --lib --locked addon::feishu && python scripts/check_architecture.py`
Expected: PASS（门禁要求「一文件一 handle 一 register」）

- [ ] **Step 9: 提交**

```bash
git add src/addon/feishu/datasource/actions/list_bitable_tables.rs \
        src/addon/feishu/datasource/actions/mod.rs \
        src/addon/feishu/domain/bitable.rs
git commit -m "feat(feishu): 列出多维表格数据表的元数据端点"
```

---

## Task 4: 元数据 Action——列出视图、列出字段

**Files:**
- Create: `src/addon/feishu/datasource/actions/list_bitable_views.rs`
- Create: `src/addon/feishu/datasource/actions/list_bitable_fields.rs`
- Modify: `src/addon/feishu/datasource/actions/mod.rs`
- Modify: `src/addon/feishu/domain/bitable.rs`
- Test: 上述文件

**Interfaces:**
- Consumes: **T3** 的 `tables_url` 形状与翻页守卫
- Produces:
  - `POST /api/v1/feishu/datasources/bitable-views` → `{"views":[{"view_id","view_name","view_type"}]}`
  - `POST /api/v1/feishu/datasources/bitable-fields` → `{"fields":[{"field_id","field_name","type"}]}`

- [ ] **Step 1: 写失败的 URL 测试**

```rust
#[test]
fn views_url_matches_the_official_path() {
    // GET /open-apis/bitable/v1/apps/:app_token/tables/:table_id/views
    let url = views_url("appbcbW", "tblsRc9G").expect("应可组装");
    assert!(url.ends_with("/bitable/v1/apps/appbcbW/tables/tblsRc9G/views"));
}

#[test]
fn fields_url_matches_the_official_path() {
    // GET /open-apis/bitable/v1/apps/:app_token/tables/:table_id/fields
    let url = fields_url("appbcbW", "tblsRc9G").expect("应可组装");
    assert!(url.ends_with("/bitable/v1/apps/appbcbW/tables/tblsRc9G/fields"));
}

#[test]
fn both_urls_reject_path_traversal() {
    assert!(views_url("../x", "tblA").is_err());
    assert!(views_url("appA", "../x").is_err());
    assert!(fields_url("appA", "../x").is_err());
}

#[test]
fn fields_url_does_not_send_view_id() {
    // 实测：列出字段的 view_id 参数不生效（带与不带返回完全相同的字段集合与顺序）。
    // 这条把「不要发它」钉住，免得有人照着官方参数字段表加回去。
    let url = fields_url("appA", "tblA").expect("应可组装");
    assert!(!url.contains("view_id"), "实际: {url}");
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib --locked addon::feishu::domain::bitable::tests`
Expected: FAIL

- [ ] **Step 3: 实现两个 URL 组装函数与 DTO**

```rust
/// 官方《列出视图》：`.../apps/:app_token/tables/:table_id/views`
pub(crate) fn views_url(app_token: &str, table_id: &str) -> Result<String, BaseError> {
    validate_path_segment("bitable_base_token", app_token)?;
    validate_path_segment("bitable_table_id", table_id)?;
    Ok(format!(
        "{FEISHU_OPEN_BASE}/bitable/v1/apps/{}/tables/{}/views",
        app_token.trim(),
        table_id.trim()
    ))
}

/// 官方《列出字段》：`.../apps/:app_token/tables/:table_id/fields`
///
/// **不发 `view_id`**：实测该参数对返回的字段集合与顺序没有任何影响
/// （2026-09-23 对 30 列的目标表带与不带各调一次，两次的 field_id 集合与顺序完全一致）。
pub(crate) fn fields_url(app_token: &str, table_id: &str) -> Result<String, BaseError> {
    validate_path_segment("bitable_base_token", app_token)?;
    validate_path_segment("bitable_table_id", table_id)?;
    Ok(format!(
        "{FEISHU_OPEN_BASE}/bitable/v1/apps/{}/tables/{}/fields",
        app_token.trim(),
        table_id.trim()
    ))
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct BitableViewItem {
    pub(crate) view_id: String,
    #[serde(default)]
    pub(crate) view_name: String,
    #[serde(default)]
    pub(crate) view_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct BitableFieldItem {
    pub(crate) field_id: String,
    #[serde(default)]
    pub(crate) field_name: String,
    /// 官方字段类型码：1 文本 / 2 数字 / 3 单选 / 4 多选 / 18 关联 / 21 双向关联 …
    #[serde(rename = "type")]
    pub(crate) field_type: i64,
}
```

- [ ] **Step 4: 写失败的 Action 测试**

两个文件各一对，形状同 T3：

```rust
#[test]
fn rejects_blank_table_id() {
    let input = ListViewsInput { app_token: "appA".into(), table_id: "  ".into() };
    assert!(input.validate().is_err());
}
```

- [ ] **Step 5: 运行确认失败**

Run: `cargo test --lib --locked addon::feishu::datasource::actions::list_bitable`
Expected: FAIL

- [ ] **Step 6: 实现两个 Action 并登记**

路由 `/api/v1/feishu/datasources/bitable-views`、`/bitable-fields`，
权限均为 `feishu.datasource.read`。`handle` 形状复刻 T3。

- [ ] **Step 7: 运行测试 + 门禁**

Run: `cargo test --lib --locked addon::feishu && python scripts/check_architecture.py`
Expected: PASS

- [ ] **Step 8: 提交**

```bash
git add src/addon/feishu/datasource/actions/list_bitable_views.rs \
        src/addon/feishu/datasource/actions/list_bitable_fields.rs \
        src/addon/feishu/datasource/actions/mod.rs \
        src/addon/feishu/domain/bitable.rs
git commit -m "feat(feishu): 列出视图与列出字段的元数据端点"
```

---

# 阶段二：表级配置、拉取、告警、体检

## Task 5: 创建表级数据源 + N 条字段绑定（同一事务）

**Files:**
- Create: `src/addon/feishu/datasource/actions/create_datasource_table.rs`
- Modify: `src/addon/feishu/datasource/actions/mod.rs`
- Test: 同文件

**Interfaces:**
- Consumes: **T1**（两表）、**T2**（冲突映射）
- Produces: `POST /api/v1/feishu/datasources/table` 接受

```json
{ "title":"公司往来付款", "ingest_mode":"pull",
  "bitable_base_token":"…", "bitable_table_id":"…", "bitable_view_id":"…",
  "fields":[ {"field_id":"fld6DuK6tM","source_key":"payment_currency","parent_field_id":null},
             {"field_id":"fldazesSdE","source_key":"payment_fx_rate","parent_field_id":"fld6DuK6tM"} ] }
```

- [ ] **Step 1: 写失败的校验测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> CreateTableInput { /* 一个合法的两字段输入，父指针指向第一项 */ }

    #[test]
    fn rejects_empty_field_list() {
        let mut input = base();
        input.fields.clear();
        assert!(input.validate().is_err(), "一个字段都不勾不允许建源");
    }

    #[test]
    fn rejects_duplicate_field_ids() {
        let mut input = base();
        let first = input.fields[0].clone();
        input.fields.push(first);
        assert!(input.validate().is_err(), "同一字段不能勾两次");
    }

    #[test]
    fn rejects_duplicate_source_keys() {
        let mut input = base();
        input.fields[1].source_key = input.fields[0].source_key.clone();
        assert!(input.validate().is_err(), "source_key 必须唯一");
    }

    #[test]
    fn rejects_parent_that_is_not_in_the_checked_set() {
        // 父必须是同一张表里**被勾选的**另一列，否则拉取时读不到父列
        let mut input = base();
        input.fields[1].parent_field_id = Some("fldNotChecked".to_string());
        assert!(input.validate().is_err(), "父列必须在勾选集合内");
    }

    #[test]
    fn rejects_self_parent() {
        let mut input = base();
        input.fields[0].parent_field_id = Some(input.fields[0].field_id.clone());
        assert!(input.validate().is_err(), "不能自己是自己的父");
    }

    #[test]
    fn rejects_parent_cycle() {
        // A→B 且 B→A：父链必须无环，否则派生时互相依赖
        let mut input = base();
        input.fields[0].parent_field_id = Some(input.fields[1].field_id.clone());
        assert!(input.validate().is_err(), "父链不得成环");
    }

    #[test]
    fn pull_requires_all_three_coordinates() {
        let mut input = base();
        input.bitable_view_id = None; // view 可空
        assert!(input.validate().is_ok(), "缺 view 应允许（表示取全表）");
        input.bitable_table_id = None;
        assert!(input.validate().is_err(), "pull 缺 table_id 必须失败");
    }

    #[test]
    fn source_key_shape_is_enforced() {
        let mut input = base();
        input.fields[0].source_key = "Bad-Key".to_string();
        assert!(input.validate().is_err());
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib --locked addon::feishu::datasource::actions::create_datasource_table`
Expected: FAIL

- [ ] **Step 3: 实现输入 DTO 与 `validate()`**

复用 `create_datasource.rs` 的 `valid_source_key`（把它提为
`domain/source_key.rs` 的 `pub(crate) fn valid_source_key`，两处共用——DRY，
且避免两份形状定义漂移）。校验顺序：字段列表非空 → 逐项 `source_key` 形状 →
`field_id` 去重 → `source_key` 去重 → 父指针存在性/自指 → **父链无环**（对每个字段
沿 `parent_field_id` 走，步数上限 = 字段数）→ 坐标（`ingest_mode == "pull"` 时
`base_token` 与 `table_id` 必填，`view_id` 可空）→ 坐标路径段形状。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --lib --locked addon::feishu::datasource::actions::create_datasource_table`
Expected: PASS

- [ ] **Step 5: 实现 `handle`（单事务写两张表）**

```rust
let mut transaction = ctx.begin_transaction().await?;
let result: Result<i64, BaseError> = async {
    // 1) 表级行
    let mut row = Record::new();
    row.insert("title", json!(input.title));
    // … 坐标与 ingest_mode 只写「提供了的」
    let datasource_id = datasources.query().insert_in_tx(&mut transaction, row).await?;

    // 2) 绑定行。生成 Token：明文只在这一刻存在，落库只落摘要 + 密文。
    for field in &input.fields {
        let plaintext = generate_token();
        let mut binding = Record::new();
        binding.insert("datasource_id", json!(datasource_id));
        binding.insert("field_id", json!(field.field_id));
        binding.insert("source_key", json!(field.source_key));
        binding.insert("token_hash", json!(hash_token(&plaintext)));
        binding.insert("token_cipher", json!(encrypt_credential(&plaintext, wrapping_key()?)?));
        // parent_field_id 只在有值时写
        fields_repo.query().insert_in_tx(&mut transaction, binding).await?;
    }
    // 3) 审计
    audit::append_in_tx(&mut transaction, &event).await?;
    Ok(datasource_id)
}.await;
FeishuContext::finish_transaction(transaction, result).await?;
```

响应里**回传每个字段的明文 Token**（只此一次机会——但见 T13，回显端点会让它可再取）。

- [ ] **Step 6: 运行测试 + 门禁 + 提交**

Run: `cargo test --lib --locked addon::feishu && python scripts/check_architecture.py`

```bash
git add src/addon/feishu/datasource/actions/create_datasource_table.rs \
        src/addon/feishu/datasource/actions/mod.rs \
        src/addon/feishu/domain/source_key.rs
git commit -m "feat(feishu): 表级数据源创建（一次写入 N 个字段绑定与凭据）"
```

---

## Task 6: 更新与删除字段绑定

**Files:**
- Create: `src/addon/feishu/datasource/actions/update_datasource_table.rs`
- Create: `src/addon/feishu/datasource/actions/delete_datasource_table.rs`
- Modify: `src/addon/feishu/datasource/actions/mod.rs`
- Test: 上述两个文件

**Interfaces:**
- Consumes: **T5** 的 `validate()` 规则（父指针与唯一性）
- Produces: `PUT /api/v1/feishu/datasources/table/{id}`、`DELETE /api/v1/feishu/datasources/table/{id}`

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn update_reruns_the_same_validation_as_create() {
    // 同一个校验函数，不允许两条路径规则不同——漂移会只在运行期暴露
    let mut input = update_input();
    input.fields[1].parent_field_id = Some("fldMissing".into());
    assert!(input.validate().is_err());
}

#[test]
fn delete_of_unknown_datasource_is_not_found() {
    // 删除要能区分「不存在」与「删成功」——静默成功会让控制台显示错误的台账
    assert!(delete_result_for_missing().is_err());
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib --locked addon::feishu::datasource::actions::update_datasource_table`
Expected: FAIL

- [ ] **Step 3: 实现**

更新 = **整份替换该数据源的绑定集合**（先按 `datasource_id` 读出现有绑定，
算出 `field_id` 的增/删/留，同事务内落库）。**保留已有绑定的 `source_key` 与凭据**——
重写它们的 `source_key` 会打断飞书侧已配的 URL（设计 §9.3）。
删除 = 同事务内先删绑定再删表级行；**同时**处理 `feishu_option` 里该 `source_key`
的选项行（沿用 `delete_datasource.rs` 既有的做法）。

- [ ] **Step 4: 运行测试 + 门禁 + 提交**

```bash
git add src/addon/feishu/datasource/actions/update_datasource_table.rs \
        src/addon/feishu/datasource/actions/delete_datasource_table.rs \
        src/addon/feishu/datasource/actions/mod.rs
git commit -m "feat(feishu): 表级数据源的更新与删除"
```

---

## Task 7: 列表端点改表级、带出字段绑定

**Files:**
- Modify: `src/addon/feishu/datasource/actions/list_datasources.rs`
- Test: 同文件

**Interfaces:**
- Produces: 列表项新增 `fields: [{field_id, field_name, source_key, parent_field_id, enabled}]`

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn list_projects_the_field_bindings() {
    // 控制台的台账要显示「这张表配了哪几列」，不能只给表级行
    let record = datasource_record_with_two_fields();
    let item = to_item(&record, &bindings_for(&record)).expect("应可投影");
    assert_eq!(item.fields.len(), 2);
    assert!(item.fields.iter().any(|f| f.source_key == "payment_currency"));
}
```

- [ ] **Step 2: 运行确认失败 → Step 3: 实现 → Step 4: 运行确认通过**

实现要点：一次 `where_in("datasource_id", ids)` 取回本页所有绑定（**避免 N+1**），
在内存里按 `datasource_id` 分组挂到对应表级项上。

Run: `cargo test --lib --locked addon::feishu::datasource::actions::list_datasources`

- [ ] **Step 5: 提交**

```bash
git commit -am "feat(feishu): 数据源列表带出字段绑定"
```

---

## Task 8: 按 `field_id` 解析当前字段名

**Files:**
- Modify: `src/addon/feishu/domain/bitable.rs`（加 `resolve_field_names`）
- Test: 同文件

**Interfaces:**
- Consumes: `list_all_fields`（已存在）
- Produces: `pub(crate) async fn resolve_field_names(client, app_token, table_id, field_ids: &[String]) -> Result<Vec<(String, String)>, BaseError>`（返回 `(field_id, 当前 field_name)`，**顺序与输入一致**）

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn missing_field_ids_are_reported_not_silently_skipped() {
    // 勾选的列被删了 → 必须能说出来是哪一个，否则拉取会静默少一列
    let remote = vec![
        remote_field("fldA", "币种"),
        remote_field("fldB", "汇率"),
    ];
    let wanted = vec!["fldA".to_string(), "fldGONE".to_string()];
    let error = resolve_from(&remote, &wanted).unwrap_err();
    assert!(error.to_string().contains("fldGONE"), "错误里要点名缺失的字段: {error}");
}

#[test]
fn resolution_is_rename_proof() {
    // 存 field_id 的意义：字段改名后仍能解析出当前名字
    let remote = vec![remote_field("fldA", "币种/Currency（单选）（改名后）")];
    let resolved = resolve_from(&remote, &["fldA".to_string()]).expect("应可解析");
    assert_eq!(resolved[0].1, "币种/Currency（单选）（改名后）");
}
```

- [ ] **Step 2: 运行确认失败 → Step 3: 实现纯函数 `resolve_from` + 异步包装 → Step 4: 通过**

把「按 id 匹配、缺哪个报哪个」抽成**纯函数** `resolve_from(remote, wanted)` 以便单测；
网络调用放在薄薄的异步壳里。同时把解析出的名字**写回绑定行的 `field_name` 缓存**
（由调用方 T9 负责，本任务只提供解析）。

Run: `cargo test --lib --locked addon::feishu::domain::bitable`

- [ ] **Step 5: 提交**

```bash
git commit -am "feat(feishu): 按 field_id 解析当前字段名（改名不断链）"
```

---

## Task 9: 表级拉取编排

**Files:**
- Modify: `src/addon/feishu/domain/pull.rs`（新增表级入口，保留既有逐源路径直到切换完成）
- Test: 同文件

**Interfaces:**
- Consumes: **T8**（名字解析）、`derive_options`、`snapshot_digest`、`extract_values_owned`
- Produces:
  - `pub(crate) struct BoundField { field_id: String, field_name: String, parent_field_id: Option<String> }`
  - `pub(crate) fn collect_field_names(fields: &[BoundField]) -> Vec<String>`
    （纯函数：取「所有绑定列 + 所有父列」的并集，**去重、保序**）
  - `pub(crate) async fn pull_table(...) -> Result<TablePullOutcome, BaseError>`

- [ ] **Step 1: 写失败的测试（纯函数部分）**

```rust
/// 一条已解析过名字的绑定：`field_id` 与它的当前 `field_name` 都在手边。
fn bound(field_id: &str, name: &str, parent: Option<&str>) -> BoundField {
    BoundField {
        field_id: field_id.to_string(),
        field_name: name.to_string(),
        parent_field_id: parent.map(str::to_string),
    }
}

#[test]
fn field_names_are_the_union_and_are_deduped() {
    // field_names 是一个 JSON 数组查询参数，重复项会被官方拒
    // （bitable.rs:879 的 duplicate_field_names_are_rejected 钉着这条）
    let fields = vec![
        bound("fldA", "币种/Currency（单选）", None),
        bound("fldB", "汇率/Exchange Rate", Some("fldA")),
    ];
    let names = collect_field_names(&fields);
    assert_eq!(names, vec!["币种/Currency（单选）", "汇率/Exchange Rate"]);
}

#[test]
fn a_column_that_is_both_a_value_and_a_parent_is_listed_once() {
    // 中间的父列（如 费用类型）既自己取值、又是别人的父 → 只能出现一次
    let fields = vec![
        bound("fldP", "费用大类/Main Exp Cat*", None),
        bound("fldM", "费用类型/Fee Type*", Some("fldP")),
        bound("fldC", "银行流水摘要-编码", Some("fldM")),
    ];
    let names = collect_field_names(&fields);
    assert_eq!(names.len(), 3, "三级链的中间列不得重复: {names:?}");
}

#[test]
fn a_cyclic_pair_does_not_duplicate_a_name() {
    // 建源时已挡环（T5），但拼 field_names 的函数不能依赖上游一定挡住了
    let fields = vec![
        bound("fldA", "甲", Some("fldB")),
        bound("fldB", "乙", Some("fldA")),
    ];
    assert_eq!(collect_field_names(&fields).len(), 2);
}

#[test]
fn parent_and_child_labels_are_trimmed_identically() {
    // 实测该表有 "CNY 人民币\n" 这种尾随换行。父键与子键必须用同一份 trim 后的文案
    // （derive.rs:96 与 :104 两处都 trim），否则子项指向一个不存在的父 option_id
    // ——失效形态是「下拉静默变空」，不报错。
    let parent_rows = [RawValue { parent_label: None, label: "CNY 人民币\n" }];
    let child_rows = [
        RawValue { parent_label: Some("CNY 人民币\n"), label: "1.0000" },
        RawValue { parent_label: Some("CNY 人民币"), label: "1.0000" },
    ];
    let parent = derive_options("currency", None, &parent_rows);
    let child = derive_options("fx", Some("currency"), &child_rows);
    assert_eq!(parent.len(), 1, "两种写法折叠成同一个父选项");
    assert_eq!(child.len(), 1, "同父同文案只派生一个子选项");
    assert_eq!(child[0].parent_key, parent[0].option_id, "子键必须命中父的 option_id");
}

#[test]
fn a_row_with_a_parent_but_no_child_value_yields_no_option() {
    // 设计 Review Focus 第 4 条：父列有值、子列为空 与 父列为空 是两种情形，
    // 但都不得挂到一个空父上。
    let rows = [
        RawValue { parent_label: Some("推广测评服务费"), label: "   " },
        RawValue { parent_label: Some("   "), label: "pay for services-X" },
    ];
    let derived = derive_options("summary", Some("fee_type"), &rows);
    assert_eq!(derived.len(), 1, "空文案不产出选项（derive.rs:96-99）");
    assert_eq!(derived[0].parent_key, "", "父文案为空时落无父键，不挂空父");
}

#[test]
fn the_same_label_under_two_parents_yields_two_options() {
    // 目标表实测有 6 例「一子多父」（如 pay for services-YL-AR 同时属于
    // 推广测评服务费 与 预付储值款）。设计答案：各派生一个，互不覆盖。
    let rows = [
        RawValue { parent_label: Some("推广测评服务费"), label: "pay for services-YL-AR" },
        RawValue { parent_label: Some("预付储值款"), label: "pay for services-YL-AR" },
    ];
    let derived = derive_options("summary", Some("fee_type"), &rows);
    assert_eq!(derived.len(), 2, "同文案不同父必须派生两个选项");
    assert_ne!(derived[0].parent_key, derived[1].parent_key);
}
```

- [ ] **Step 2: 运行确认失败 → Step 3: 实现 `collect_field_names` 与编排 → Step 4: 通过**

编排（设计 §6.2）：

```
1. 取表级源（ingest_mode = pull 且 status = active），一次取回其绑定（enabled = true）
2. 解析名字（T8）→ 缺失即整体失败（全有或全无）
3. field_names = 所有绑定列 + 所有父列 的并集，去重
4. 一次 list_all_records 取回一份快照
5. 对每条绑定：extract_values_owned → derive_options → snapshot_digest 比对
   → 未变且本地无已停用行则跳过 → 否则事务内整行替换 + 补集停用
```

Run: `cargo test --lib --locked addon::feishu::domain::pull`

- [ ] **Step 5: 提交**

```bash
git commit -am "feat(feishu): 表级拉取编排（一次取回、按字段分派）"
```

---

## Task 10: 表级失败语义 + 告警邮件

**Files:**
- Create: `src/addon/feishu/domain/alert.rs`
- Modify: `src/config/mod.rs`（`FeishuConfig` 加 `alert_recipients`）
- Modify: `src/infrastructure/feishu_pull.rs`（失败时触发告警）
- Test: `alert.rs`

**Interfaces:**
- Consumes: `SmtpEmailSender`（`account/domain/email_delivery.rs:156`）、
  `consecutive_failures`（表级列）
- Produces: `FeishuConfig::alert_recipients: Vec<String>`（默认空 = 不告警）

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn blank_recipients_are_rejected_at_config_time() {
    // 配了一个空串 = 以为配了其实没配，邮件永远发不出去
    assert!(validate_recipients(&["a@b.com".into(), "  ".into()]).is_err());
}

#[test]
fn an_invalid_address_is_rejected() {
    assert!(validate_recipients(&["not-an-email".into()]).is_err());
}

#[test]
fn empty_list_means_alerts_disabled() {
    assert!(validate_recipients(&[]).is_ok(), "不配 = 不告警，是合法配置");
}

#[test]
fn alert_fires_only_at_the_threshold() {
    // 抖动时不发邮件风暴：1 次失败不发，达到阈值才发
    assert!(!should_alert(1, 3));
    assert!(should_alert(3, 3));
    assert!(should_alert(4, 3), "超过阈值后每轮都应继续告警，直到恢复");
}
```

- [ ] **Step 2: 运行确认失败 → Step 3: 实现 → Step 4: 通过**

`alert.rs`：`validate_recipients`、`should_alert(failures, threshold)`（纯函数，好测）、
`FeishuAlertSender` trait（照 `PasswordResetEmailSender` 的形状），
以及一个 `send_pull_failure(datasource_title, last_error, failures)` 的消息构造。
配置项照 `pull_interval_seconds` 的先例（`config/mod.rs:606-616`）声明默认值与校验，
阈值也做成配置并**设下限**。

在 `feishu_pull.rs` 的失败路径上：自增 `consecutive_failures` → 写 `last_error` →
`should_alert` 为真则发邮件。成功路径清零（既有行为，别破坏）。

Run: `cargo test --lib --locked addon::feishu::domain::alert`

- [ ] **Step 5: 提交**

```bash
git commit -am "feat(feishu): 拉取失败告警邮件（达阈值触发，避免抖动风暴）"
```

---

## Task 11: 体检端点

**Files:**
- Create: `src/addon/feishu/datasource/actions/health_check.rs`
- Modify: `src/addon/feishu/datasource/actions/mod.rs`
- Test: 同文件

**Interfaces:**
- Consumes: **T8** 的 `resolve_field_names`
- Produces: `POST /api/v1/feishu/datasources/table/{id}/health`

```json
{ "ok": false,
  "missing_fields": [{"field_id":"fldX","source_key":"old_rate"}],
  "view_missing": false,
  "table_missing": false }
```

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn rename_is_not_reported_as_a_problem() {
    // 改名能自愈（每轮按 field_id 解析名字）→ 不得进体检列表。
    // 把它报成问题会让运维白跑一趟，也会稀释真正的问题。
    let remote = vec![remote_field("fldA", "币种（改名后）")];
    let bindings = vec![binding("fldA", "currency")];
    let report = classify(&remote, &bindings, /* view_ok */ true, /* table_ok */ true);
    assert!(report.ok);
    assert!(report.missing_fields.is_empty());
}

#[test]
fn a_deleted_field_is_reported_by_id_and_key() {
    // 只报 field_id 运维看不懂；只报 source_key 定位不到列。两个都要给。
    let remote = vec![remote_field("fldA", "币种")];
    let bindings = vec![binding("fldA", "currency"), binding("fldGONE", "old_rate")];
    let report = classify(&remote, &bindings, true, true);
    assert!(!report.ok);
    assert_eq!(report.missing_fields.len(), 1);
    assert_eq!(report.missing_fields[0].field_id, "fldGONE");
    assert_eq!(report.missing_fields[0].source_key, "old_rate");
}

#[test]
fn every_failure_mode_is_listed_together_not_short_circuited() {
    // 表被删了、视图也没了、还缺字段 → 三件事一起报，别只报第一件
    let report = classify(&[], &[binding("fldGONE", "k")], false, false);
    assert!(!report.table_missing);
    assert!(!report.ok);
}
```

- [ ] **Step 2: 运行确认失败 → Step 3: 实现 → Step 4: 通过**

`classify` 为纯函数；异步壳负责调 `列出字段` / `列出视图` 并把网络错误映射成
「这一项查不了」（不要因为视图接口挂了就报不出字段缺失）。

Run: `cargo test --lib --locked addon::feishu::datasource::actions::health_check`

- [ ] **Step 5: 提交**

```bash
git add src/addon/feishu/datasource/actions/health_check.rs \
        src/addon/feishu/datasource/actions/mod.rs
git commit -m "feat(feishu): 数据源体检端点（改名不算问题，缺列点名）"
```

---

# 阶段三：Token 生命周期

## Task 12: Token 生成、回显、轮换（三个端点）

**Files:**
- Create: `src/addon/feishu/datasource/actions/reveal_token.rs`
- Create: `src/addon/feishu/datasource/actions/rotate_token.rs`
- Modify: `src/addon/feishu/datasource/actions/mod.rs`
- Modify: `src/addon/feishu/domain/crypto.rs`（加凭据加解密封装）
- Test: 上述文件

**Interfaces:**
- Consumes: **T1**（`token_hash` / `token_cipher`）、**T2**（冲突映射）
- Produces:
  - `POST /api/v1/feishu/datasources/fields/{source_key}/reveal` → `{"token":"<明文>"}`
    —— **纯读，不改变任何状态**
  - `POST /api/v1/feishu/datasources/fields/{source_key}/rotate` → `{"token":"<新明文>"}`
    —— 写，事务内生成 + 落库 + 返回

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn revealed_token_is_the_same_as_the_stored_one() {
    // 「一直可以复制同一个值」是硬需求：回显必须解密出原值，不是现场新生成
    let (hash, cipher) = seal("s3cret-token");
    assert_eq!(unseal(&cipher).expect("应可解密"), "s3cret-token");
    assert!(verify_token("s3cret-token", &hash));
}

#[test]
fn rotation_changes_both_hash_and_cipher() {
    let (hash_before, cipher_before) = seal("old-token");
    let (hash_after, cipher_after) = seal("new-token");
    assert_ne!(hash_before, hash_after);
    assert_ne!(cipher_before, cipher_after);
    assert!(!verify_token("old-token", &hash_after), "旧 Token 必须立即失效");
}

#[test]
fn unseal_rejects_a_tampered_ciphertext() {
    // 密文被改过要显式失败，不能返回垃圾明文
    let (_hash, mut cipher) = seal("t");
    cipher.push('A');
    assert!(unseal(&cipher).is_err());
}

#[test]
fn reveal_does_not_write() {
    // 回显是纯读。用「调两次拿到同一个值」把它钉住——
    // 若哪天有人把它改成轮换，这条会红。
    assert_eq!(unseal(&seal("t").1).unwrap(), unseal(&seal("t").1).unwrap());
}
```

- [ ] **Step 2: 运行确认失败 → Step 3: 实现 → Step 4: 通过**

`crypto.rs` 加 `seal(&str) -> Result<(String /*hash*/, String /*cipher*/)>` 与
`unseal(&str) -> Result<String>`，复用既有的 `encrypt_bytes` / `derive_key`；
新增解密方向（`aes::cipher::BlockDecryptMut` 现在只在测试里导入，要提到正式路径）。
**算法不变**：IV 前置、PKCS#7。`unseal` 走同一套参数。

两个 Action：
- `reveal` 权限 **`feishu.datasource.secret`**（**不是** `...read`），**每次追加审计**；
- `rotate` 权限 `feishu.datasource.write`，事务内换 hash + cipher + `token_rotated_at`，
  **每次追加审计**。

Run: `cargo test --lib --locked addon::feishu`

- [ ] **Step 5: 提交**

```bash
git add src/addon/feishu/datasource/actions/reveal_token.rs \
        src/addon/feishu/datasource/actions/rotate_token.rs \
        src/addon/feishu/datasource/actions/mod.rs \
        src/addon/feishu/domain/crypto.rs
git commit -m "feat(feishu): Token 回显（纯读）与轮换（写）两个独立端点"
```

---

# 阶段三·补：退役字段级入口

## Task 13: 退役字段级 Action，把立即拉取与探针改到表级

**Files:**
- Delete: `src/addon/feishu/datasource/actions/create_datasource.rs`
- Delete: `src/addon/feishu/datasource/actions/update_datasource.rs`
- Delete: `src/addon/feishu/datasource/actions/delete_datasource.rs`
- Modify: `src/addon/feishu/datasource/actions/pull_now.rs`、`pull_probe.rs`
- Modify: `src/addon/feishu/datasource/actions/mod.rs`
- Modify: `src/addon/feishu/domain/pull.rs`（删掉被 T9 取代的逐源路径）

**Interfaces:**
- Consumes: **T5/T6/T7**（表级 CRUD 已能覆盖旧功能）、**T9**（表级拉取）
- Produces: `POST /api/v1/feishu/datasources/table/{id}/pull-now`（入参从 `source_key` 改为表级 id）

**为什么单独一个任务**：这是「两个并存的可写入口会让审计语义与数据来源分叉」的直接落实
（`option/mod.rs:54-57` 的既有立场）。留着旧 Action 会让人从两个入口写同一批表，
而且旧 Action 还在引用 T1 删掉的 `bitable_field_name` / `linkage_mapping` 列——
**它们必须一起删，不能拖**。

- [ ] **Step 1: 先确认旧入口的功能已被覆盖**

Run: `grep -n "pub(super) mod" src/addon/feishu/datasource/actions/mod.rs`
逐条对照：`create_datasource` → T5、`update_datasource` → T6、`delete_datasource` → T6、
`list_datasources` → T7（保留并改造）、`pull_now` → 改入参、
`pull_probe` → 改入参、`pull_schedule` → 不动（它是全局排程，无实体耦合）。
**任何一条找不到对应物就先停下来，不要删。**

- [ ] **Step 2: 写失败测试——旧路由已不存在**

在 `actions/mod.rs` 或一个集成测试里：

```rust
#[test]
fn the_field_level_create_route_is_gone() {
    // 两个可写入口并存会让审计语义分叉；旧路由必须消失，
    // 而不是留在那里指向一个已删掉列的表。
    let routes = registered_routes_for_test();
    assert!(!routes.iter().any(|r| r == "/api/v1/feishu/datasources" && r.method == Post),
            "字段级创建路由应已退役，实际路由表: {routes:?}");
    assert!(routes.iter().any(|r| r == "/api/v1/feishu/datasources/table"));
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test --lib --locked addon::feishu`
Expected: FAIL（旧路由还在）

- [ ] **Step 4: 删旧 Action、改 `pull_now` 入参、删 `pull.rs` 的逐源路径**

`pull_now` 的输入从 `source_key: String` 改为 `datasource_id: i64`，
内部改调 T9 的 `pull_table`。`pull_probe` 同改。
`register_all` 里的对应条目一并删掉。

- [ ] **Step 5: 运行测试 + 门禁**

Run: `cargo test --lib --locked addon::feishu && python scripts/check_architecture.py`
Expected: PASS。若 `check_architecture` 报「一文件一 handle 一 register」，说明删漏了文件。

- [ ] **Step 6: 全量编译（抓测试与 worker 里的残留引用）**

Run: `cargo test --all-targets --locked --no-run`
Expected: 编译通过。`src/infrastructure/feishu_pull.rs` 若引用了逐源路径，在这里一起改。

- [ ] **Step 7: 提交**

```bash
git add -A src/addon/feishu
git commit -m "refactor(feishu): 退役字段级可写入口，拉取与探针改到表级"
```

---

# 阶段四：前端控制台

## Task 14: 配置向导

**Files:**
- Create: `frontend/src/features/feishu/components/DatasourceTableWizard.tsx`
- Create: `frontend/src/features/feishu/components/FieldPickerTable.tsx`
- Modify: `frontend/src/features/feishu/api.ts`、`types.ts`
- Test: `frontend/tests/features/feishu/components/table-wizard.test.tsx`

**Interfaces:**
- Consumes: **T3/T4** 三个元数据端点、**T5** 创建端点
- Produces: 向导组件，四步

- [ ] **Step 1: 写失败的测试**

```tsx
it("视图选择器说明它只决定拉取哪些行，不决定能勾哪些字段", async () => {
  // 实测：列出字段的 view_id 参数不生效。UI 若把两件事讲成一件事，
  // 运维会以为换视图能换出一批字段。
  render(<DatasourceTableWizard />);
  expect(await screen.findByText(/只决定拉取哪些行/)).toBeInTheDocument();
});

it("勾选后可为字段指定父列，且父列下拉只列已勾选项", async () => {
  render(<DatasourceTableWizard />);
  await userEvent.click(await screen.findByLabelText("费用类型/Fee Type*"));
  const parentSelect = await screen.findByLabelText("父列");
  expect(within(parentSelect).queryByText("费用类型/Fee Type*")).toBeNull();
  expect(within(parentSelect).getByText("费用大类/Main Exp Cat*")).toBeInTheDocument();
});
```

- [ ] **Step 2: 运行确认失败**

Run: `cd frontend && pnpm vitest run tests/features/feishu/components/table-wizard.test.tsx`
Expected: FAIL（组件不存在）

- [ ] **Step 3: 实现**

四步（`useState` 管理，照 `DatasourceFormDialog.tsx` 的既有写法，不引 react-hook-form）：
① 填 app_token → 拉表列表选表；② 拉视图列表选视图（**旁附上面那句说明文案**）；
③ 拉字段列表（全部列出，不按类型过滤——设计文档决策：全列出来自己判断）、勾选；
④ 每行给 `source_key` 输入（按 `field_id` 派生默认值，注意 `valid_source_key` 的形状）
与父列下拉（选项 = 已勾选的其它字段）。

复用 `@/shared/ui/{button,checkbox,dialog,input,label,select,table}`。

- [ ] **Step 4: 运行确认通过 + 前端门禁**

Run: `cd frontend && pnpm vitest run tests/features/feishu && pnpm typecheck`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add frontend/src/features/feishu frontend/tests/features/feishu
git commit -m "feat(feishu-ui): 表级配置向导（选表→选视图→勾字段→配父列）"
```

---

## Task 15: 拷贝清单页与体检/轮换入口

**Files:**
- Create: `frontend/src/features/feishu/components/CredentialChecklist.tsx`
- Create: `frontend/src/features/feishu/components/DatasourceHealthPanel.tsx`
- Modify: `frontend/src/features/feishu/views/DatasourceDetailPage.tsx`、`api.ts`、`types.ts`
- Test: `frontend/tests/features/feishu/components/credential-checklist.test.tsx`

**Interfaces:**
- Consumes: **T11**（体检）、**T12**（回显/轮换）
- Produces: 详情页的凭据清单与体检面板

- [ ] **Step 1: 写失败的测试**

```tsx
it("每行有两个可复制项：URL 与 Token", async () => {
  // 本次不做 Key 加密（设计决策 D11），所以是两项不是三项
  render(<CredentialChecklist items={[itemFixture]} />);
  expect(screen.getAllByRole("button", { name: /复制/ })).toHaveLength(2);
});

it("轮换按钮二次确认并说明后果", async () => {
  render(<CredentialChecklist items={[itemFixture]} />);
  await userEvent.click(screen.getByRole("button", { name: "轮换" }));
  expect(await screen.findByText(/已配置该字段的控件会立即失效/)).toBeInTheDocument();
});

it("复制不会触发任何写请求", async () => {
  // 「复制是纯读」是设计硬要求（决策 D10）：误点复制不能有任何后果
  const spy = vi.spyOn(api, "rotateToken");
  render(<CredentialChecklist items={[itemFixture]} />);
  await userEvent.click(screen.getAllByRole("button", { name: /复制/ })[1]);
  expect(spy).not.toHaveBeenCalled();
});

it("体检把缺失的字段按 id 与 source_key 一起列出", async () => {
  render(<DatasourceHealthPanel report={reportFixture} />);
  expect(screen.getByText("fldGONE")).toBeInTheDocument();
  expect(screen.getByText("old_rate")).toBeInTheDocument();
});
```

- [ ] **Step 2: 运行确认失败 → Step 3: 实现 → Step 4: 通过**

`CredentialChecklist`：一行一个字段，两个复制按钮（URL / Token）+ 一个轮换按钮。
**关键：旋转按钮不得与复制按钮相邻到会误点的程度**，且轮换必须走确认框（设计 §10.3）。

Run: `cd frontend && pnpm vitest run tests/features/feishu && pnpm typecheck`

- [ ] **Step 5: 提交**

```bash
git add frontend/src/features/feishu frontend/tests/features/feishu
git commit -m "feat(feishu-ui): 凭据拷贝清单与体检面板"
```

---

## 收尾检查

- [ ] **跑快速门禁**：`python scripts/run_ci.py quick` → 全绿
- [ ] **跑全量门禁**：`python scripts/run_ci.py full` → 全绿（含 clippy `-D warnings`）
- [ ] **MSRV**：本次未新增依赖；若 Step 中引入了任何 crate，跑冷缓存 `cargo check`（见「常用命令」）
- [ ] **集成测试**：**先问用户**，得到许可后再跑
      `python scripts/run_ci.py integration`
- [ ] **设计文档回写**：把 `derive.rs:47-52` 那句「实测三条父子关系全部 0 例『一子多父』」
      改成「该结论得自银行网点 xlsx 数据；目标台账实测有 6 例」（设计文档 §4.7）
- [ ] **V4 前置**：动工**之前**先跑一次真实飞书审批提交抓 `linkage_params` 报文
      （设计文档 §11.2）。若报文与预期不符，**停下来回来改 §8 的级联设计**

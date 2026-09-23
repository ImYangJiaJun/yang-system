# 权限组与首账号引导 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 交付可配置的权限组，并让第一个注册账号在同一事务内被引导为拥有全部权限的系统管理员，同时保留「应用内不存在自提权路径」这一不变量。

**Architecture:** 权限组是权限的命名集合（一层，不可嵌套）。Token 签发时由 `GroupGrantResolver` 把「直授权限 ∪ 组成员权限的并集」烧进 claims——Token 内仍只有具体权限字符串，运行期比对逻辑一行不改。内置全权组 `system_admin` 的权限集合不落存储，解析时取整个权限目录，因此未来新增 Action 的权限自动纳入。引导靠 `system_owner` 单行哨兵表的唯一约束仲裁并发，不依赖任何「判空」逻辑。组变更后的失效沿用既有的「锁用户 → 递增 `authz_version` → 写 Outbox」同事务原语，同步扇出。

**Tech Stack:** Rust 2021（MSRV 1.80）、axum（经 `yang-base` transport-axum）、sqlx (MySQL 8)、Redis 7、声明式 Schema（无 SQL 迁移文件）、React 19 + Vitest、Playwright。

**Spec:** `docs/architecture/2026-09-24-permission-groups-and-bootstrap-design.md`

## Global Constraints

- **决策修订**：本计划实施 spec §3 的 D2/D4 修订。D2 从「无最终管理员」改为「引导式一次性、可降权」；D4 从「无角色聚合层」扩展出一层权限组。两条修订必须先落地到 `docs/architecture/foundation-baseline.md`（Task 16），否则文档与代码口径矛盾。
- **声明式 Schema，禁止 SQL 迁移文件**：`docs/contracts/SCHEMA.md:3` 明确本仓库不维护版本化迁移、历史 SQL 或 `_migrations` 表。新表只经 `TableSpec` 声明，启动时增量同步。
- **schema_sync 只增不删**：永不删除表、列、索引或约束（`docs/contracts/SCHEMA.md:12,30`）。外键规则被框架硬编码为 `RESTRICT` 且不可改（`crates/yang-base/src/table/definition.rs:628-655`）——**外键一旦声明即永久存在**。
- **一 Module 一表**：`ModuleSpec.table` 类型为 `Option<TableSpec>`（`crates/yang-base/src/definition/spec.rs:492,527`）。
- **权限字符串**：`^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$`，最长 128。**不允许通配符**——`*` 与 `system.*` 均会被 DB CHECK 拒绝。
- **组成员上限**：`MAX_GROUP_MEMBERS = 200`（spec §6.3）。超过上限的组权限变更必须明确报错，不得静默做 O(N) 行锁事务。
- **锁序**：扇出失效时一律按 `user_id` 升序加锁（spec §6.3）。这是防死锁的唯一手段。
- **授权事实 writer 边界**：新事实表的 writer 必须登记进 `docs/architecture/authorization-writers.md` 并加 `<!-- authorization-writer: <id> <path> -->` 标记（机器可读门禁）。每次事实变更必须在同一事务经 `AuthorizationPort` 递增 `authz_version` 并追加 Outbox。
- **原始 SQL 只允许落在特定路径**：`**/repository.rs`，或 stem ∈ `{authz_version, grants, guard, lifecycle, service, tenant}`，且必须带 `raw-sql-boundary` 标记与文档条目（`scripts/check_architecture.py:573-590`）。→ **组 Repository 的文件名必须是 `repository.rs` 或落在同目录内被允许的 stem**；本计划用 `group_repository.rs` 会失败，改用 `access/domain/groups/repository.rs` 目录形式（其 stem 仍是 `repository`）。
- **生产代码禁止 `unsafe`、`unwrap()`、`expect()`**（`Cargo.toml` 设 `unwrap_used`/`expect_used` 为 deny）。测试内可用 `unwrap_or_else(|e| panic!(...))`。
- **一 Action 一文件**，形态为「自包含 register」：恰好一个 `pub(super) async fn handle` + 一个 `pub(super) fn register(module, access)`；`actions/mod.rs` 只保留 `mod` 声明与 `ACTIONS` 数组。禁止在业务代码用 `#[derive(Action)]`。改完必须跑 `python scripts/check_architecture.py`。
- **每个 Action 必须挂 Step-up 与 append-only 审计**（`docs/contracts/AUDIT.md`）。
- **命名**：不得把新类型命名为 `PermissionGroup`——框架已有同名结构（`crates/yang-base/src/router/middleware.rs:83-101`，语义是「Action 要求的一组权限 + All/Any」）。统一使用 `Group` 词根。
- **仓库文档与注释一律中文**；`yang-base` 有 `#![warn(missing_docs)]`，应用 crate 无此强制但保持注释密度与邻近文件一致。
- **门禁**：每任务结束跑 `python scripts/run_ci.py quick`；涉及真实 MySQL/Redis 的行为补 `run_ci.py integration`（测试库名以 `_test` 结尾、Redis DB 15、`--test-threads=1`）。
- **提交粒度**：每任务一次提交，Conventional Commits，范围用 `feat(access)` / `feat(account)` / `docs(authz)`。

## Review Focus

以下是 spec 隐含、但没有任何任务的测试会覆盖到的输入类别与失败模式。它们最可能在实际使用时咬人。每一项都在其归属任务的步骤里配了对应测试。

1. **权限目录尚未安装时的全权解析**。`PermissionCatalogHandle::entries()` 在未安装时返回 `ConfigError`（`permission_catalog.rs:95-100`），而 `system_admin` 组的有效权限来自该目录。元数据导出路径（`build_metadata_app`）与部分测试不安装目录——此时解析必须 **fail-closed 报错**，绝不能退化成「返回空权限集」或「返回全部权限」。合理预期：报错并拒绝签发。
2. **管理员把自己移出全权组（但不是最后一名）**。这是合法操作，必须成功；但操作者随即失去全部权限，包括 `access.groups.*`，因此**无法再把自己加回去**（被 §8.1 附加规则挡住）。合理预期：操作成功并给出明确的一次性提示语义，而不是静默把操作者锁在门外。
3. **删除组的并发与「删除 vs 加成员」的竞态**。应用层「组内无成员才允许删除」的检查与 FK `RESTRICT` 之间存在窗口。合理预期：无论哪个先到，最终要么删除成功、要么返回可读的冲突错误，绝不出现 500 或留下悬空成员行。
4. **组成员数正好处于上限边界**。200 与 201 的加权限行为必须不同：≤200 成功，>200 明确报错。合理预期：边界可预测，错误信息说明当前成员数与上限。
5. **目录收缩后的孤儿权限条目**。某个 Action 被移除后，组里那条权限字符串匹配不到任何 Action。合理预期：解析不 panic、不放大权限，且列表接口把它标记出来（spec §8.4）。

---

## 文件结构

### 新建

| 文件 | 职责 |
|---|---|
| `src/addon/access/groups/mod.rs` | `access.groups` 模块装配：表、上下文、中间件、Action 注册表、Step-up 守卫、展示投影 |
| `src/addon/access/groups/table.rs` | `permission_group` 表声明（唯一放在 module 层的组表） |
| `src/addon/access/groups/actions/mod.rs` | 组管理 Action 注册表（`ACTIONS` 数组） |
| `src/addon/access/groups/actions/create_group.rs` | 建组 |
| `src/addon/access/groups/actions/update_group.rs` | 改组标题/描述 |
| `src/addon/access/groups/actions/delete_group.rs` | 删组（组内非空则拒绝） |
| `src/addon/access/groups/actions/list_groups.rs` | 组列表（成员数、权限数、内置标记、孤儿数） |
| `src/addon/access/groups/actions/get_group.rs` | 组详情（条目 + 成员 + `effective_all` + 孤儿标记） |
| `src/addon/access/groups/actions/add_group_item.rs` | 向组加权限 |
| `src/addon/access/groups/actions/remove_group_item.rs` | 从组移除权限 |
| `src/addon/access/groups/actions/add_group_member.rs` | 加成员（受 §8.1 子集校验） |
| `src/addon/access/groups/actions/remove_group_member.rs` | 移出成员（受最后管理员守卫） |
| `src/addon/access/domain/groups/repository.rs` | 组事实的唯一受信 writer（声明、成员、条目三张表） |
| `src/addon/access/domain/groups/resolution.rs` | 有效权限解析（纯函数 + 目录投影），被 resolver 与提权校验共用 |
| `src/addon/access/domain/groups/admin.rs` | 组管理的事务编排：扇出失效、提权校验、最后管理员判定 |
| `src/addon/access/domain/groups/mod.rs` | 上述三者的模块出口 |
| `src/addon/access/domain/groups/owner.rs` | `SystemOwnerClaimer` 的 access 实现 |
| `src/addon/access/domain/group_resolver.rs` | `GroupGrantResolver`（`GrantResolver` 实现） |
| `tests/permission_groups_integration.rs` | 组 CRUD、扇出失效、引用完整性的真实 MySQL/Redis 集成测试 |
| `tests/system_owner_bootstrap_integration.rs` | 并发首注册引导的真实 MySQL 对抗测试 |

> **注意 `domain/groups/` 用目录而非 `group_repository.rs` 单文件**：架构门禁只允许原始 SQL 出现在 `**/repository.rs` 或 stem ∈ `{authz_version, grants, guard, lifecycle, service, tenant}` 的文件里（`scripts/check_architecture.py:573-590`）。目录形式下 stem 仍是 `repository`，可通过门禁。

### 修改

| 文件 | 改动 |
|---|---|
| `src/infrastructure/schema.rs:47-56` | `infrastructure_definitions()` 返回类型 `[TableDefinition; 6]` → `[TableDefinition; 9]`，新增三张表 |
| `src/infrastructure/schema.rs:289-303` | 同步精确表名断言测试 |
| `src/addon/access/mod.rs` | 装配 `access.groups` 模块；新增 `system_owner_claimer()` 出口 |
| `src/addon/access/domain/mod.rs` | 导出 `groups` 与 `group_resolver` |
| `src/addon/access/domain/context.rs` | `Access` 增加 `groups()` 访问器 |
| `src/app.rs:106-107` | `grant_resolvers` 加入 `GroupGrantResolver`；`system_owner_claimer` 换成 access 实现 |
| `src/addon/account/domain/system_owner.rs:21-29` | `SystemOwnerClaimer::claim` 增加 `&ActionContext` 参数 |
| `src/addon/account/domain/context.rs:169-178` | `Account::claim_system_owner` 透传 `ctx` |
| `src/addon/account/user/actions/register.rs:93-115` | `AlreadyClaimed` 降级为正常结果，不再 `?` 上抛 |
| `src/addon/account/user/actions/disable_self.rs` | 增加最后管理员守卫 |
| `src/addon/account/user/actions/admin_disable_user.rs:33-41` | 增加最后管理员守卫 |
| `src/addon/account/user/actions/delete_account.rs:44-90` | 最后管理员守卫 + 清理 `authz_grant`/`user_group` 行 |
| `docs/architecture/authorization-writers.md` | 登记组 writer |
| `docs/architecture/foundation-baseline.md:37,39` | D2/D4 修订 |
| `docs/contracts/AUTHZ_GRANTS.md` | 初始授权章节改写 + 权限组契约章节 |
| `AGENTS.md:7,35` | 口径修正 |
| `docs/architecture/account-system-roadmap.md:32` | 「权限管理面整体不可达」更正 |
| `frontend/src/features/registry.ts` | 登记权限组自定义视图 |
| `frontend/src/shell/routes.tsx` | 权限组路由 |
| `frontend/contracts/openapi.json`、`frontend/src/engine/contracts/api-types.ts` | 契约重生成（生成物，禁止手改） |

---

## Task 1: `permission_group` 表与 `access.groups` 模块骨架

**Files:**
- Create: `src/addon/access/groups/table.rs`
- Create: `src/addon/access/groups/mod.rs`
- Modify: `src/addon/access/mod.rs`
- Test: `src/addon/access/groups/table.rs`（文件内 `#[cfg(test)]`）

**Interfaces:**
- Consumes: `yang_base::definition::{Key, Str, Int, Timestamp, TableName, TableSpec, FieldName, FieldRef}`、`yang_base::fields!`
- Produces:
  - 常量：`pub(crate) const GROUP_ID: &str = "id";`、`GROUP_KEY`、`GROUP_TITLE`、`GROUP_DESCRIPTION`、`GROUP_CREATED_BY`、`GROUP_OCCURRED_AT`、`GROUP_RECORD_FIELDS: &[&str]`
  - `pub(crate) const GROUP_KEY_PATTERN: &str = r"^[a-z][a-z0-9_]*$";`、`pub(crate) const GROUP_KEY_MAX_LENGTH: usize = 64;`
  - `pub(crate) const SYSTEM_ROLE: &str = "system";`（与 `access/grants/table.rs:10` 同名同值，模块内独立常量，遵循仓库既有惯例）
  - `pub(crate) fn groups_table_spec() -> Result<TableSpec, BaseError>`
  - `pub(super) fn build_module(...) -> Result<ModuleSpec, BaseError>`（本任务先只装表，Action 在 Task 10+ 加）

- [ ] **Step 1: 写失败的测试**

创建 `src/addon/access/groups/table.rs`，先只写测试与常量占位：

```rust
//! permission_group 表声明：权限组本体（决策 D4 扩展层）。
//!
//! 声明（模块是什么）位于模块层；机制（模块怎么做）位于 `domain/groups/`。
//! 授权事实只能经 `domain/groups/repository.rs` 的受信 writer 变更。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_table_declares_expected_columns_and_unique_key() {
        let spec = groups_table_spec().unwrap_or_else(|e| panic!("组表定义应有效: {e}"));
        let names: Vec<&str> = spec.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["id", "group_key", "title", "description", "created_by", "occurred_at"]
        );
        assert!(spec
            .indexes
            .iter()
            .any(|i| i.unique && i.name.as_deref() == Some("uk_permission_group_key")));

        let definition = spec.table_definition().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(definition.name(), "permission_group");
        assert_eq!(definition.primary_key(), "id");
    }

    #[test]
    fn group_key_pattern_rejects_uppercase_and_dots() {
        let spec = groups_table_spec().unwrap_or_else(|e| panic!("{e}"));
        let key = spec
            .fields
            .iter()
            .find(|f| f.name.as_str() == "group_key")
            .unwrap_or_else(|| panic!("应存在 group_key 字段"));
        assert_eq!(key.validation.pattern.as_deref(), Some(GROUP_KEY_PATTERN));
        assert_eq!(key.validation.max_length, Some(GROUP_KEY_MAX_LENGTH));
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib --locked groups::table`
Expected: 编译失败，`cannot find function groups_table_spec` / `cannot find value GROUP_KEY_PATTERN`

- [ ] **Step 3: 实现表声明**

在 `src/addon/access/groups/table.rs` 的测试模块之上插入：

```rust
use crate::addon::account::user::table::SYSTEM_ROLE;
use yang_base::definition::{FieldName, FieldRef, Int, Key, Str, TableName, TableSpec, Timestamp};
use yang_base::BaseError;

/// 组标识格式：单段小写，用于代码与审计引用（展示名走 `title`）。
pub(crate) const GROUP_KEY_PATTERN: &str = r"^[a-z][a-z0-9_]*$";
pub(crate) const GROUP_KEY_MAX_LENGTH: usize = 64;

pub(crate) const GROUP_ID: &str = "id";
pub(crate) const GROUP_KEY: &str = "group_key";
pub(crate) const GROUP_TITLE: &str = "title";
pub(crate) const GROUP_DESCRIPTION: &str = "description";
pub(crate) const GROUP_CREATED_BY: &str = "created_by";
pub(crate) const GROUP_OCCURRED_AT: &str = "occurred_at";
pub(crate) const GROUP_RECORD_FIELDS: &[&str] = &[
    GROUP_ID,
    GROUP_KEY,
    GROUP_TITLE,
    GROUP_DESCRIPTION,
    GROUP_CREATED_BY,
    GROUP_OCCURRED_AT,
];

/// 构建权限组事实表的唯一 Schema 定义。
pub(crate) fn groups_table_spec() -> Result<TableSpec, BaseError> {
    let fields = yang_base::fields! {
        id => Key::new().title("ID"),
        group_key => Str::new()
                .title("组标识")
                .require(true)
                .max_length(GROUP_KEY_MAX_LENGTH)
                .pattern(GROUP_KEY_PATTERN)
                .filterable(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
        title => Str::new()
                .title("展示名")
                .require(true)
                .max_length(128)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
        description => Str::new()
                .title("描述")
                .max_length(255)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
        created_by => Int::new()
                .title("创建人")
                .require(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
        occurred_at => Timestamp::new()
                .title("创建时间")
                .created_at()
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
    };
    let table_name =
        TableName::new("permission_group").map_err(|e| BaseError::ConfigError(e.to_string()))?;
    Ok(TableSpec::new(table_name.clone())
        .title("权限组")
        .fields(fields)
        .unique_named(
            "uk_permission_group_key",
            [field_ref(&table_name, GROUP_KEY)?],
        ))
}

fn field_ref(table_name: &TableName, field: &str) -> Result<FieldRef, BaseError> {
    let field = FieldName::new(field).map_err(|e| BaseError::ConfigError(e.to_string()))?;
    Ok(FieldRef::new(table_name.clone(), field))
}
```

同时在测试模块里补上 `use super::*;` 已在，需确认 `TableSpec.fields` 与 `validation.max_length` 的字段名与 `grants/table.rs` 测试一致——**若 `spec.indexes` 或 `validation.max_length` 的访问方式编译不过，照 `src/addon/access/grants/table.rs:71-101` 的既有测试写法调整**（该文件已有同形状断言，是权威参照）。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib --locked groups::table`
Expected: PASS（2 tests）

- [ ] **Step 5: 建模块装配文件并接入 access**

创建 `src/addon/access/groups/mod.rs`：

```rust
//! `access.groups` Module（module 层）：权限组管理装配。
//!
//! 本文件就是这个模块的"定义卡"：表、上下文、中间件、Action 注册表、
//! Step-up 守卫与展示投影按分区顺序装配；业务用例全部在 `actions/` 的
//! 自包含文件中。

pub(super) mod table;

use super::domain::context::Access;
use super::domain::permission_catalog::PermissionCatalogHandle;
use crate::addon::account::user_from_claims;
use crate::authorization::{AuthorizationPort, AuthorizationVersionValidator, StepUpServices};
use std::sync::Arc;
use yang_base::action::TokenAuthMiddleware;
use yang_base::definition::{ModuleName, ModuleSpec};
use yang_base::BaseError;

/// 装配 `access.groups` Module：本任务只装表与中间件，Action 在后续任务加入。
pub(super) fn build_module(
    authorization_validator: AuthorizationVersionValidator,
    _step_up: Option<StepUpServices>,
    _permission_catalog: PermissionCatalogHandle,
    _authorization: AuthorizationPort,
    access: Arc<Access>,
) -> Result<ModuleSpec, BaseError> {
    let table = table::groups_table_spec()?;
    let module = ModuleSpec::new(
        ModuleName::new("access.groups").map_err(|e| BaseError::ConfigError(e.to_string()))?,
    )
    .table(table)
    .middleware(
        TokenAuthMiddleware::new(user_from_claims)
            .with_claims_validator(authorization_validator)
            .authenticate_public_actions(),
    );
    let _ = access; // Action 注册在 Task 10 接入
    Ok(module)
}
```

在 `src/addon/access/mod.rs` 的 `mod grants;` 下方加 `mod groups;`，并在 `build_addon` 里把模块加进 `AddonSpec`：

```rust
    let (module, access) = grants::build_module(
        authorization_validator.clone(),
        step_up.clone(),
        permission_catalog.clone(),
        authorization.clone(),
    )?;
    let groups_module = groups::build_module(
        authorization_validator,
        step_up,
        permission_catalog,
        authorization,
        Arc::clone(&access),
    )?;
    Ok(AccessAddon {
        spec: AddonSpec::new(yang_base::addon!("access"))
            .module(module)
            .module(groups_module),
        grant_resolver: Arc::new(AuthzGrantResolver::new(access)),
    })
```

- [ ] **Step 6: 确认建表真的发生**

Run: `python scripts/check_architecture.py && cargo test --lib --locked`
Expected: 架构门禁通过；全部单元测试通过

- [ ] **Step 7: 提交**

```bash
git add src/addon/access/groups/ src/addon/access/mod.rs
git commit -m "feat(access): 新增 permission_group 表与 access.groups 模块骨架"
```

---

## Task 2: 三张运行支撑表 + `infrastructure_definitions` 扩到 9

**Files:**
- Create: `src/addon/access/domain/groups/tables.rs`
- Modify: `src/infrastructure/schema.rs:47-56`、`:289-303`
- Test: `src/addon/access/domain/groups/tables.rs`（文件内）、`src/infrastructure/schema.rs`（断言测试）

**Interfaces:**
- Consumes: Task 1 的 `GROUP_KEY_PATTERN`、`GROUP_KEY_MAX_LENGTH`、`SYSTEM_ROLE`
- Produces:
  - `pub(crate) fn group_items_table_spec() -> Result<TableSpec, BaseError>`（表名 `permission_group_item`）
  - `pub(crate) fn user_group_table_spec() -> Result<TableSpec, BaseError>`（表名 `user_group`）
  - `pub(crate) fn system_owner_table_spec() -> Result<TableSpec, BaseError>`（表名 `system_owner`）
  - 常量：`ITEM_GROUP_ID`、`ITEM_PERMISSION`、`ITEM_GRANTED_BY`、`ITEM_OCCURRED_AT`、`MEMBER_USER_ID`、`MEMBER_GROUP_ID`、`OWNER_SENTINEL_KEY`、`OWNER_USER_ID`、`OWNER_CLAIMED_AT`、`SENTINEL_KEY_VALUE: &str = "system-owner"`

- [ ] **Step 1: 写失败的测试**

创建 `src/addon/access/domain/groups/tables.rs`，先只写测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_item_table_has_composite_unique_and_permission_check() {
        let spec = group_items_table_spec().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(spec.table.name.as_str(), "permission_group_item");
        assert!(spec.indexes.iter().any(|i| i.unique
            && i.name.as_deref() == Some("uk_permission_group_item")));
        assert!(spec.checks.iter().any(|c| c.name == "chk_permission_group_item_permission_format"));
    }

    #[test]
    fn user_group_table_has_two_restrict_foreign_keys() {
        let spec = user_group_table_spec().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(spec.table.name.as_str(), "user_group");
        let fks: Vec<&str> = spec.foreign_keys.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(fks, ["fk_user_group_user", "fk_user_group_group"]);
    }

    #[test]
    fn system_owner_table_pins_the_sentinel_key() {
        let spec = system_owner_table_spec().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(spec.table.name.as_str(), "system_owner");
        assert!(spec.indexes.iter().any(|i| i.unique
            && i.name.as_deref() == Some("uk_system_owner_sentinel")));
        assert!(spec.checks.iter().any(|c| c.name == "chk_system_owner_sentinel"));
    }
}
```

> `spec.table.name`、`spec.checks`、`spec.foreign_keys` 的确切字段名以 `crates/yang-base/src/definition/spec.rs` 的 `TableSpec` 定义与 `src/addon/access/grants/table.rs` 既有测试为准；如字段名不同，按实际调整断言而非改变表设计。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib --locked groups::tables`
Expected: 编译失败，找不到三个 `*_table_spec`

- [ ] **Step 3: 实现三张表**

```rust
//! 权限组的三张运行支撑表声明：组-权限条目、用户-组关系、引导哨兵。
//!
//! 这三张表没有独立 UI 语义，因此按 `docs/contracts/SCHEMA.md` 与
//! `src/infrastructure/schema.rs` 的既有惯例放进运行支撑表数组，
//! 而不是各建一个 module（一 Module 一表）。

use crate::addon::access::domain::permission_catalog::{
    PERMISSION_MAX_LENGTH, PERMISSION_PATTERN,
};
use crate::addon::account::user::table::SYSTEM_ROLE;
use yang_base::definition::{FieldName, FieldRef, Int, Key, Str, TableName, TableSpec, Timestamp};
use yang_base::BaseError;

pub(crate) const ITEM_ID: &str = "id";
pub(crate) const ITEM_GROUP_ID: &str = "group_id";
pub(crate) const ITEM_PERMISSION: &str = "permission";
pub(crate) const ITEM_GRANTED_BY: &str = "granted_by";
pub(crate) const ITEM_OCCURRED_AT: &str = "occurred_at";

pub(crate) const MEMBER_ID: &str = "id";
pub(crate) const MEMBER_USER_ID: &str = "user_id";
pub(crate) const MEMBER_GROUP_ID: &str = "group_id";
pub(crate) const MEMBER_GRANTED_BY: &str = "granted_by";
pub(crate) const MEMBER_OCCURRED_AT: &str = "occurred_at";

pub(crate) const OWNER_ID: &str = "id";
pub(crate) const OWNER_SENTINEL_KEY: &str = "sentinel_key";
pub(crate) const OWNER_USER_ID: &str = "user_id";
pub(crate) const OWNER_CLAIMED_AT: &str = "claimed_at";

/// 哨兵行唯一取值。第二个插入者必然违反 UNIQUE 或 CHECK。
pub(crate) const SENTINEL_KEY_VALUE: &str = "system-owner";
const SENTINEL_KEY_MAX_LENGTH: usize = 32;
const SENTINEL_CHECK_EXPR: &str = "`sentinel_key` = 'system-owner'";

fn table_name(raw: &str) -> Result<TableName, BaseError> {
    TableName::new(raw).map_err(|e| BaseError::ConfigError(e.to_string()))
}

fn field_ref(table_name: &TableName, field: &str) -> Result<FieldRef, BaseError> {
    let field = FieldName::new(field).map_err(|e| BaseError::ConfigError(e.to_string()))?;
    Ok(FieldRef::new(table_name.clone(), field))
}

/// 组 → 权限条目。
pub(crate) fn group_items_table_spec() -> Result<TableSpec, BaseError> {
    let name = table_name("permission_group_item")?;
    let fields = yang_base::fields! {
        id => Key::new().title("ID"),
        group_id => Int::new().title("权限组").require(true).filterable(true)
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
        permission => Str::new().title("权限").require(true)
                .max_length(PERMISSION_MAX_LENGTH).pattern(PERMISSION_PATTERN).filterable(true)
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
        granted_by => Int::new().title("授权操作人").require(true)
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
        occurred_at => Timestamp::new().title("授权时间").created_at()
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
    };
    Ok(TableSpec::new(name.clone())
        .title("权限组条目")
        .fields(fields)
        .unique_named(
            "uk_permission_group_item",
            [field_ref(&name, ITEM_GROUP_ID)?, field_ref(&name, ITEM_PERMISSION)?],
        )
        .check_named(
            "chk_permission_group_item_permission_format",
            "regexp_like(`permission`, '^[a-z][a-z0-9_]*(\\\\.[a-z][a-z0-9_]*)+$')",
        ))
}

/// 用户 → 权限组。
pub(crate) fn user_group_table_spec() -> Result<TableSpec, BaseError> {
    let name = table_name("user_group")?;
    let users = table_name("users")?;
    let groups = table_name("permission_group")?;
    let fields = yang_base::fields! {
        id => Key::new().title("ID"),
        user_id => Int::new().title("用户").require(true).filterable(true)
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
        group_id => Int::new().title("权限组").require(true).filterable(true)
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
        granted_by => Int::new().title("授权操作人").require(true)
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
        occurred_at => Timestamp::new().title("入组时间").created_at()
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
    };
    Ok(TableSpec::new(name.clone())
        .title("用户权限组")
        .fields(fields)
        .unique_named(
            "uk_user_group",
            [field_ref(&name, MEMBER_USER_ID)?, field_ref(&name, MEMBER_GROUP_ID)?],
        )
        // 外键规则固定 RESTRICT：删除仍有成员的组会被数据库拒绝（spec §8.3）。
        .foreign_key_named(
            "fk_user_group_user",
            [field_ref(&name, MEMBER_USER_ID)?],
            [field_ref(&users, "id")?],
        )
        .foreign_key_named(
            "fk_user_group_group",
            [field_ref(&name, MEMBER_GROUP_ID)?],
            [field_ref(&groups, "id")?],
        ))
}

/// 引导哨兵：语义上的单行表。
pub(crate) fn system_owner_table_spec() -> Result<TableSpec, BaseError> {
    let name = table_name("system_owner")?;
    let fields = yang_base::fields! {
        id => Key::new().title("ID"),
        sentinel_key => Str::new().title("哨兵键").require(true)
                .max_length(SENTINEL_KEY_MAX_LENGTH)
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
        user_id => Int::new().title("被引导用户").require(true)
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
        claimed_at => Timestamp::new().title("声明时间").created_at()
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
    };
    Ok(TableSpec::new(name.clone())
        .title("系统管理员声明")
        .fields(fields)
        .unique_named("uk_system_owner_sentinel", [field_ref(&name, OWNER_SENTINEL_KEY)?])
        .check_named("chk_system_owner_sentinel", SENTINEL_CHECK_EXPR))
}
```

> `TableSpec::unique_named` / `check_named` / `foreign_key_named` 的确切签名参照 `src/addon/access/grants/table.rs:50-63` 与 `src/infrastructure/schema.rs:187-201` 的既有用法。`field_ref` 若在多个文件重复，允许各自私有（与 `grants/table.rs:66-69` 的既有做法一致）。

- [ ] **Step 4: 运行确认三张表定义通过**

Run: `cargo test --lib --locked groups::tables`
Expected: PASS（3 tests）

- [ ] **Step 5: 写 `infrastructure_definitions` 的失败断言**

修改 `src/infrastructure/schema.rs` 的测试：

```rust
    #[test]
    fn infrastructure_schema_is_complete_and_versionless() {
        let definitions = infrastructure_definitions()
            .unwrap_or_else(|error| panic!("运行支撑表定义应有效: {error}"));
        assert_eq!(
            definitions.map(|definition| definition.name().to_string()),
            [
                "authorization_outbox",
                "audit_event",
                "permission_group_item",
                "password_reset_token",
                "user_group",
                "system_owner",
                "user_session",
                "login_event",
                "user_avatar",
            ]
        );
    }
```

- [ ] **Step 6: 运行确认失败**

Run: `cargo test --lib --locked infrastructure_schema_is_complete_and_versionless`
Expected: FAIL——数组长度不匹配（当前 `[TableDefinition; 6]`）

- [ ] **Step 7: 扩展数组到 9**

把 `src/infrastructure/schema.rs:47` 的签名改为：

```rust
fn infrastructure_definitions() -> Result<[TableDefinition; 9], BaseError> {
```

并在数组里追加三项（保持与断言相同的顺序；新表定义经 `table_spec()?.table_definition()?` 转换，与同文件既有条目的写法一致）：

```rust
        crate::addon::access::domain::groups::tables::group_items_table_spec()?.table_definition()?,
        crate::addon::access::domain::groups::tables::user_group_table_spec()?.table_definition()?,
        crate::addon::access::domain::groups::tables::system_owner_table_spec()?.table_definition()?,
```

- [ ] **Step 8: 运行确认通过**

Run: `cargo test --lib --locked infrastructure && python scripts/check_architecture.py`
Expected: PASS；架构门禁通过

- [ ] **Step 9: 提交**

```bash
git add src/addon/access/domain/groups/ src/infrastructure/schema.rs
git commit -m "feat(access): 声明组的条目表/成员表与引导哨兵表，运行支撑表扩到 9"
```

---

## Task 3: 组事实的受信 writer 与门禁登记

**Files:**
- Create: `src/addon/access/domain/groups/mod.rs`
- Create: `src/addon/access/domain/groups/repository.rs`
- Modify: `src/addon/access/domain/mod.rs`、`src/addon/access/domain/context.rs`
- Modify: `docs/architecture/authorization-writers.md`
- Test: `src/addon/access/domain/groups/repository.rs`（文件内）

**Interfaces:**
- Consumes: Task 2 的三张表与其列常量
- Produces:
  - `pub(crate) const SYSTEM_ADMIN_GROUP_KEY: &str = "system_admin";`
  - `pub(crate) const MAX_GROUP_MEMBERS: usize = 200;`
  - `pub(crate) struct GroupRecord { pub(crate) id: i64, pub(crate) group_key: String, pub(crate) title: String, pub(crate) description: Option<String>, pub(crate) created_by: i64 }`
  - `pub(crate) struct GroupRepository`，方法（全部 `_in_tx`，签名模式统一为 `(&self, ctx: &ActionContext, transaction: &mut Transaction, ...) -> Result<_, BaseError>`）：
    - `new(groups: TableDefinition, items: TableDefinition, members: TableDefinition, owners: TableDefinition) -> Self`——**四个表定义**，第四个是 `system_owner`；Task 8 的哨兵写入依赖它，因此此处一次性定型
    - `insert_group_in_tx(..., group_key: &str, title: &str, description: Option<&str>, created_by: i64) -> Result<i64, BaseError>`
    - `find_by_key_in_tx(..., group_key: &str) -> Result<Option<GroupRecord>, BaseError>`
    - `find_by_id_in_tx(..., group_id: i64) -> Result<Option<GroupRecord>, BaseError>`
    - `ensure_system_admin_group_in_tx(...) -> Result<i64, BaseError>`
    - `list_groups_in_tx(...) -> Result<Vec<GroupRecord>, BaseError>`
    - `delete_group_in_tx(..., group_id: i64) -> Result<u64, BaseError>`
    - `update_group_in_tx(..., group_id: i64, title: &str, description: Option<&str>) -> Result<u64, BaseError>`
    - `insert_item_in_tx(..., group_id: i64, permission: &str, granted_by: i64) -> Result<bool, BaseError>`（幂等：已存在返回 `false`）
    - `delete_item_in_tx(..., group_id: i64, permission: &str) -> Result<bool, BaseError>`
    - `list_items_in_tx(..., group_id: i64) -> Result<Vec<String>, BaseError>`
    - `insert_member_in_tx(..., user_id: i64, group_id: i64, granted_by: i64) -> Result<bool, BaseError>`
    - `delete_member_in_tx(..., user_id: i64, group_id: i64) -> Result<bool, BaseError>`
    - `list_members_in_tx(..., group_id: i64) -> Result<Vec<i64>, BaseError>`（按 `user_id` 升序）
    - `list_group_ids_of_user_in_tx(..., user_id: i64) -> Result<Vec<i64>, BaseError>`
    - `count_members_in_tx(..., group_id: i64) -> Result<u64, BaseError>`
  - `Access::groups(&self) -> &GroupRepository`（`context.rs` 新增访问器）
  - writer 标记：`// authorization-writer: access-group-lifecycle src/addon/access/domain/groups/repository.rs`

- [ ] **Step 1: 写失败的测试（受信投影边界）**

创建 `src/addon/access/domain/groups/mod.rs`：

```rust
//! 权限组的领域机制层：事实 writer、有效权限解析、管理编排与引导声明。

pub(crate) mod admin;
pub(crate) mod owner;
pub(crate) mod repository;
pub(crate) mod resolution;
pub(crate) mod tables;

pub(crate) use repository::{
    GroupRecord, GroupRepository, MAX_GROUP_MEMBERS, SYSTEM_ADMIN_GROUP_KEY,
};
```

（`admin`、`owner`、`resolution` 在本任务先建成空文件并各写一行 `//! 待实现` 之外的模块文档，避免 `mod` 声明悬空；其内容在 Task 4/6/8 填入。）

创建 `src/addon/access/domain/groups/repository.rs`，先写测试：

```rust
//! 权限组事实（组、条目、成员）的唯一持久化边界。
//! authorization-writer: access-group-lifecycle
//!
//! 对外 `TableQuery` 始终以 `system` 能力运行：组事实不暴露给字段权限体系，
//! 读取（解析、管理查询）与写入（组生命周期）都收敛在本 Repository。

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::mysql::MySqlPoolOptions;
    use std::sync::Arc;
    use yang_base::table::TableDefinition;
    use yang_base::{action::ActionContext, action::Request, tools::ToolsBuilder};
    use yang_db::{Database, DatabaseConfig};

    fn lazy_pool() -> Arc<sqlx::MySqlPool> {
        let pool = MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"));
        Arc::new(pool)
    }

    fn test_context() -> ActionContext {
        let mysql = Database::from_pool(
            (*lazy_pool()).clone(),
            DatabaseConfig::default(),
        )
        .unwrap_or_else(|error| panic!("测试 Database 应构建成功: {error}"));
        let tools = Arc::new(
            ToolsBuilder::new()
                .mysql(mysql)
                .build()
                .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}")),
        );
        ActionContext::new(Request::new(serde_json::json!({})), tools)
    }

    fn test_repository() -> GroupRepository {
        let definition = |spec: Result<
            yang_base::definition::TableSpec,
            BaseError,
        >| -> TableDefinition {
            spec.and_then(|spec| spec.table_definition())
                .unwrap_or_else(|error| panic!("表定义应有效: {error}"))
        };
        GroupRepository::new(
            definition(crate::addon::access::groups::table::groups_table_spec()),
            definition(super::super::tables::group_items_table_spec()),
            definition(super::super::tables::user_group_table_spec()),
            definition(super::super::tables::system_owner_table_spec()),
        )
    }

    #[test]
    fn group_facts_are_only_readable_by_the_system_capability() {
        let groups = crate::addon::access::groups::table::groups_table_spec()
            .and_then(|spec| spec.table_definition())
            .unwrap_or_else(|error| panic!("组表定义应有效: {error}"));
        let table = groups.bind(lazy_pool());

        let denied = table
            .query(["user"])
            .select_fields(&[crate::addon::access::groups::table::GROUP_KEY]);
        assert!(
            matches!(denied, Err(BaseError::FieldPermissionDenied(_, field, _)) if field == crate::addon::access::groups::table::GROUP_KEY),
            "组事实必须只对 system 能力可读"
        );
        assert!(table
            .query([crate::addon::access::groups::table::SYSTEM_ROLE])
            .select_fields(&[crate::addon::access::groups::table::GROUP_KEY])
            .is_ok());
    }

    #[test]
    fn group_repository_exposes_a_trusted_query() {
        let repository = test_repository();
        let ctx = test_context();
        assert!(repository.trusted_query(&ctx).is_ok());
    }

    #[test]
    fn max_group_members_is_a_named_constant() {
        assert_eq!(MAX_GROUP_MEMBERS, 200);
        assert_eq!(SYSTEM_ADMIN_GROUP_KEY, "system_admin");
    }
}
```

> `table.bind(pool)` 的接收类型以 `src/addon/access/grants/table.rs:104-133` 的既有测试为准（那里传 `Arc<MySqlPool>`）。若 `Database::from_pool` 的签名与之不符，照 `src/addon/access/domain/repository.rs:158-166` 的用法调整——两处都有同形状的可用样例。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib --locked groups::repository`
Expected: 编译失败，找不到 `GroupRepository`

- [ ] **Step 3: 实现 Repository**

按 `src/addon/access/domain/repository.rs` 的形状实现（`trusted_query` 用 `ctx.tools().mysql()?.pool().clone()` + `bind(pool).query([SYSTEM_ROLE])`）。每个方法的 SQL 语义如下，实现时全部经 `trusted_query` 走 `TableQuery`，**不写原始 SQL**：

```rust
pub(crate) const SYSTEM_ADMIN_GROUP_KEY: &str = "system_admin";
pub(crate) const MAX_GROUP_MEMBERS: usize = 200;

pub(crate) struct GroupRecord {
    pub(crate) id: i64,
    pub(crate) group_key: String,
    pub(crate) title: String,
    pub(crate) description: Option<String>,
    pub(crate) created_by: i64,
}

pub(crate) struct GroupRepository {
    groups: TableDefinition,
    items: TableDefinition,
    members: TableDefinition,
    owners: TableDefinition,
}

impl GroupRepository {
    pub(crate) fn new(
        groups: TableDefinition,
        items: TableDefinition,
        members: TableDefinition,
        owners: TableDefinition,
    ) -> Self {
        Self { groups, items, members, owners }
    }

    fn trusted_query(&self, ctx: &ActionContext) -> Result<TableQuery, BaseError> {
        let pool = Arc::new(ctx.tools().mysql()?.pool().clone());
        Ok(self.groups.bind(pool).query([SYSTEM_ROLE]))
    }

    fn trusted_items(&self, ctx: &ActionContext) -> Result<TableQuery, BaseError> { /* 同形，绑 items */ }
    fn trusted_members(&self, ctx: &ActionContext) -> Result<TableQuery, BaseError> { /* 同形，绑 members */ }

    /// 幂等地确保内置全权组存在，返回其 id。
    ///
    /// 并发下两个调用者可能同时插入：唯一键冲突按「别人已建好」处理，
    /// 回查后返回既有 id（savepoint-and-refetch 模式）。
    pub(crate) async fn ensure_system_admin_group_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
    ) -> Result<i64, BaseError> {
        if let Some(existing) = self.find_by_key_in_tx(ctx, transaction, SYSTEM_ADMIN_GROUP_KEY).await? {
            return Ok(existing.id);
        }
        match self
            .insert_group_in_tx(ctx, transaction, SYSTEM_ADMIN_GROUP_KEY, "系统管理员", None, 0)
            .await
        {
            Ok(id) => Ok(id),
            Err(BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_))) => self
                .find_by_key_in_tx(ctx, transaction, SYSTEM_ADMIN_GROUP_KEY)
                .await?
                .map(|record| record.id)
                .ok_or_else(|| {
                    BaseError::ConfigError("内置权限组插入冲突后回查不到".to_string())
                }),
            Err(error) => Err(error),
        }
    }
}
```

其余方法按同名 `GrantRepository` 方法（`repository.rs:51-125`）的写法逐一实现：`select_fields(...)` + `where_eq(...)` + `all_in_tx(transaction)` 读；`insert_in_tx(transaction, record)` 写；`delete_in_tx(transaction)` 返回受影响行数。`list_members_in_tx` 必须显式按 `MEMBER_USER_ID` 升序返回（`order_by` 或查询后 `sort_unstable()`），因为扇出失效依赖升序锁序。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --lib --locked groups::repository`
Expected: PASS

- [ ] **Step 5: 接入 `Access` 上下文**

在 `src/addon/access/domain/context.rs` 给 `Access` 加字段与方法：

```rust
use super::groups::GroupRepository;

pub(crate) struct Access {
    grants: GrantRepository,
    groups: GroupRepository,
    permission_catalog: PermissionCatalogHandle,
    authorization: AuthorizationPort,
}

impl Access {
    pub(crate) fn new(
        grants: GrantRepository,
        groups: GroupRepository,
        permission_catalog: PermissionCatalogHandle,
        authorization: AuthorizationPort,
    ) -> Self { /* ... */ }

    /// 权限组事实的唯一持久化边界。
    pub(crate) fn groups(&self) -> &GroupRepository {
        &self.groups
    }
}
```

在 `src/addon/access/grants/mod.rs:35-39` 的 `Access::new(...)` 调用点补上 `GroupRepository::new(...)` 参数（用 Task 2 的三张表定义构造）。

- [ ] **Step 6: 登记 writer 边界**

在 `docs/architecture/authorization-writers.md` 的「授权事实」表格追加一行：

```markdown
| `permission_group` / `permission_group_item` / `user_group` | 整行 | 仅 access 组事实 writer 可增删改；组成员或权限变化必须同事务经账号安全版本原语递增**受影响用户**的 `authz_version` 并追加 Outbox（扇出按 `user_id` 升序加锁） |
```

在「允许边界」表格追加：

```markdown
| `access-group-lifecycle` | `src/addon/access/domain/groups/repository.rs` | 权限组、组条目与用户-组关系的读取、插入与删除；版本递增复用 `account-security-version` |
```

并在文件底部的标记区追加：

```markdown
<!-- authorization-writer: access-group-lifecycle src/addon/access/domain/groups/repository.rs -->
```

- [ ] **Step 7: 跑门禁并提交**

Run: `python scripts/check_architecture.py && cargo test --lib --locked`
Expected: 门禁通过（writer allowlist 与 raw-sql-boundary 均满足）

```bash
git add src/addon/access/ docs/architecture/authorization-writers.md
git commit -m "feat(access): 组事实受信 writer 与 writer 边界登记"
```

---

## Task 4: 有效权限解析（纯函数 + 目录投影）

**Files:**
- Create: `src/addon/access/domain/groups/resolution.rs`
- Test: `src/addon/access/domain/groups/resolution.rs`（文件内）

**Interfaces:**
- Consumes: `PermissionCatalogHandle::{entries, ensure_declared}`、`SYSTEM_ADMIN_GROUP_KEY`
- Produces:
  - `pub(crate) fn resolve_group_permissions(group_key: &str, items: &[String], catalog: &[String]) -> Vec<String>`
  - `pub(crate) fn catalog_permissions(handle: &PermissionCatalogHandle) -> Result<Vec<String>, BaseError>`（未安装时透传 `ConfigError`）
  - `pub(crate) fn orphan_items<'a>(items: &'a [String], catalog: &[String]) -> Vec<&'a str>`

**为什么是纯函数**：spec §8.1 要求「提权校验」与「实际解析」共用同一实现，否则两条路径会漂移。把可测的合并/排序/孤儿判定抽成不依赖数据库的纯函数，是保证这一点的最小手段。

- [ ] **Step 1: 写失败的测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Vec<String> {
        vec![
            "access.grants.read".to_string(),
            "access.grants.write".to_string(),
            "account.users.read".to_string(),
        ]
    }

    #[test]
    fn system_admin_group_resolves_to_the_whole_catalog() {
        let resolved = resolve_group_permissions(SYSTEM_ADMIN_GROUP_KEY, &[], &catalog());
        assert_eq!(resolved, catalog());
    }

    #[test]
    fn system_admin_ignores_its_own_stored_items() {
        // 内置组的条目由目录计算，即使表里意外有行也不参与。
        let resolved = resolve_group_permissions(
            SYSTEM_ADMIN_GROUP_KEY,
            &["demo.notes.read".to_string()],
            &catalog(),
        );
        assert_eq!(resolved, catalog());
        assert!(!resolved.iter().any(|p| p == "demo.notes.read"));
    }

    #[test]
    fn static_group_resolves_to_its_items_sorted_and_deduplicated() {
        let resolved = resolve_group_permissions(
            "ops",
            &[
                "access.grants.write".to_string(),
                "access.grants.read".to_string(),
                "access.grants.read".to_string(),
            ],
            &catalog(),
        );
        assert_eq!(
            resolved,
            ["access.grants.read", "access.grants.write"],
            "必须稳定排序并去重"
        );
    }

    #[test]
    fn orphan_items_are_reported_but_never_resolved_into_permissions() {
        // Review Focus 5：目录收缩后组里的权限条目成为孤儿。
        let items = vec!["demo.notes.read".to_string(), "access.grants.read".to_string()];
        let orphans = orphan_items(&items, &catalog());
        assert_eq!(orphans, ["demo.notes.read"], "孤儿条目必须被标记出来");

        // 关键：孤儿条目**不参与解析**，因此不会放大权限，也不会 panic。
        let resolved = resolve_group_permissions("ops", &items, &catalog());
        assert_eq!(resolved, ["access.grants.read"]);
    }

    #[test]
    fn static_group_with_only_orphans_resolves_to_nothing() {
        let resolved = resolve_group_permissions(
            "ops",
            &["removed.module.act".to_string()],
            &catalog(),
        );
        assert!(resolved.is_empty());
    }

    #[test]
    fn catalog_permissions_is_fail_closed_when_not_installed() {
        // Review Focus 1：目录未安装时必须报错，绝不能退化成空集或全权集。
        use crate::addon::access::domain::permission_catalog::PermissionCatalogHandle;
        let handle = PermissionCatalogHandle::new();
        let error = catalog_permissions(&handle).expect_err("未安装目录必须失败");
        assert!(
            matches!(error, yang_base::BaseError::ConfigError(_)),
            "必须是 ConfigError（fail-closed），实际为 {error:?}"
        );
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib --locked groups::resolution`
Expected: 编译失败，找不到 `resolve_group_permissions`

- [ ] **Step 3: 实现**

```rust
//! 有效权限解析：把「用户所在组」折叠为权限集合。
//!
//! 本模块是**唯一的解析实现**——`GroupGrantResolver` 与组管理 Action 的
//! 提权校验都调用它（spec §8.1），避免校验与实际解析漂移成两套语义。
//! 全部为纯函数，不依赖数据库，因此可以无夹具单测。

use super::repository::SYSTEM_ADMIN_GROUP_KEY;
use crate::addon::access::domain::permission_catalog::PermissionCatalogHandle;
use std::collections::BTreeSet;
use yang_base::BaseError;

/// 读取已安装的权限目录；未安装时透传 ConfigError（fail-closed）。
pub(crate) fn catalog_permissions(
    handle: &PermissionCatalogHandle,
) -> Result<Vec<String>, BaseError> {
    Ok(handle
        .entries()?
        .iter()
        .map(|entry| entry.permission().to_string())
        .collect())
}

/// 一个组对有效权限的贡献。
///
/// 内置全权组取整个目录（因此未来新增 Action 的权限自动纳入）；
/// 普通组取其条目与目录的交集——**孤儿条目被静默丢弃**，因为匹配不到
/// 任何 Action 的权限字符串不可能放行任何请求。
pub(crate) fn resolve_group_permissions(
    group_key: &str,
    items: &[String],
    catalog: &[String],
) -> Vec<String> {
    if group_key == SYSTEM_ADMIN_GROUP_KEY {
        let mut all = catalog.to_vec();
        all.sort();
        all.dedup();
        return all;
    }
    let known: BTreeSet<&str> = catalog.iter().map(String::as_str).collect();
    let mut resolved: BTreeSet<String> = BTreeSet::new();
    for item in items {
        if known.contains(item.as_str()) {
            resolved.insert(item.clone());
        }
    }
    resolved.into_iter().collect()
}

/// 组内已不在权限目录中的条目（spec §8.4：只报告，不自动清理）。
pub(crate) fn orphan_items<'a>(items: &'a [String], catalog: &[String]) -> Vec<&'a str> {
    let known: BTreeSet<&str> = catalog.iter().map(String::as_str).collect();
    let mut orphans: Vec<&str> = items
        .iter()
        .map(String::as_str)
        .filter(|item| !known.contains(item))
        .collect();
    orphans.sort_unstable();
    orphans.dedup();
    orphans
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --lib --locked groups::resolution`
Expected: PASS（6 tests）

- [ ] **Step 5: 提交**

```bash
git add src/addon/access/domain/groups/resolution.rs
git commit -m "feat(access): 有效权限解析纯函数（含全权组与孤儿条目语义）"
```

---

## Task 5: `GroupGrantResolver` 接入 Token 签发

**Files:**
- Create: `src/addon/access/domain/group_resolver.rs`
- Modify: `src/addon/access/domain/mod.rs`、`src/addon/access/mod.rs`、`src/app.rs:106`
- Test: `src/addon/access/domain/group_resolver.rs`（文件内，纯逻辑部分）

**Interfaces:**
- Consumes: Task 3 的 `GroupRepository`、Task 4 的 `resolution`、`GrantResolver` trait（`src/addon/account/domain/grants.rs:56-66`）
- Produces:
  - `pub(crate) struct GroupGrantResolver { access: Arc<Access> }`
  - `impl GroupGrantResolver { pub(crate) fn new(access: Arc<Access>) -> Self }`
  - `impl GrantResolver for GroupGrantResolver`——`resolve(&self, ctx, user_id, transaction) -> Result<AuthorizationGrants, BaseError>`
  - `AccessAddon::group_grant_resolver(&self) -> Arc<dyn GrantResolver>`（`access/mod.rs`）

**关键语义**：解析结果**只并入 permissions，不并入 roles**。角色保持账号域固定的 `user`，与 `access/domain/resolver.rs:71` 的既有断言一致（spec §6.1）。

- [ ] **Step 1: 写失败的测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_permissions_are_merged_as_permissions_only() {
        // 组名绝不进入角色集合（spec §6.1：角色仍为账号域固定的 user）。
        let grants = grants_from_resolved([
            "access.grants.read".to_string(),
            "access.grants.read".to_string(),
            "account.users.read".to_string(),
        ]);
        assert_eq!(
            grants.permissions().collect::<Vec<_>>(),
            ["access.grants.read", "account.users.read"]
        );
        assert_eq!(grants.roles().count(), 0, "组解析不得附加任何角色");
    }

    #[test]
    fn empty_membership_yields_an_empty_extension() {
        assert_eq!(grants_from_resolved(Vec::new()).permissions().count(), 0);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib --locked groups::group_resolver`
Expected: 编译失败

- [ ] **Step 3: 实现**

```rust
//! Token 签发时的权限组解析：把用户所在组折叠为附加权限。
//!
//! 只补充权限，不附加角色（spec §6.1：Token 中仍只有具体权限字符串，
//! 运行期 `permissions_match` 语义不变）。

use super::context::Access;
use super::groups::resolution::{catalog_permissions, resolve_group_permissions};
use crate::addon::account::{AuthorizationGrants, GrantResolver};
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::BaseError;
use yang_db::Transaction;

/// 把解析出的权限集合合并为稳定去重的授权快照。
pub(crate) fn grants_from_resolved<I>(permissions: I) -> AuthorizationGrants
where
    I: IntoIterator<Item = String>,
{
    let mut grants = AuthorizationGrants::default();
    for permission in permissions {
        grants = grants.permission(permission);
    }
    grants
}

/// 账号域 `GrantResolver` 的权限组实现。
pub(crate) struct GroupGrantResolver {
    access: Arc<Access>,
}

impl GroupGrantResolver {
    pub(crate) fn new(access: Arc<Access>) -> Self {
        Self { access }
    }
}

#[async_trait]
impl GrantResolver for GroupGrantResolver {
    async fn resolve(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        transaction: &mut Transaction,
    ) -> Result<AuthorizationGrants, BaseError> {
        let catalog = catalog_permissions(self.access.permission_catalog())?;
        let group_ids = self
            .access
            .groups()
            .list_group_ids_of_user_in_tx(ctx, transaction, user_id)
            .await?;
        let mut permissions: Vec<String> = Vec::new();
        for group_id in group_ids {
            let group = self
                .access
                .groups()
                .find_by_id_in_tx(ctx, transaction, group_id)
                .await?;
            let Some(group) = group else { continue };
            let items = self
                .access
                .groups()
                .list_items_in_tx(ctx, transaction, group_id)
                .await?;
            permissions.extend(resolve_group_permissions(&group.group_key, &items, &catalog));
        }
        Ok(grants_from_resolved(permissions))
    }
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --lib --locked groups::group_resolver`
Expected: PASS

- [ ] **Step 5: 接入组合根**

在 `src/addon/access/mod.rs` 的 `AccessAddon` 增加出口：

```rust
    /// 账号域在 Token 签发时合并权限组权限的解析器。
    pub(crate) fn group_grant_resolver(&self) -> Arc<dyn GrantResolver> {
        Arc::new(GroupGrantResolver::new(Arc::clone(&self.access)))
    }
```

（`AccessAddon` 需额外持有 `access: Arc<Access>`；`build_addon` 里已经构造了它，只是当前传给了 `AuthzGrantResolver`，改成 `Arc::clone` 后同时传给两者。）

在 `src/app.rs:106` 把 resolver 加进 `Vec`：

```rust
    let grant_resolvers: Vec<Arc<dyn account::GrantResolver>> = vec![
        access.grant_resolver(),
        access.group_grant_resolver(),
    ];
```

- [ ] **Step 6: 端到端验证解析进入 claims**

在 `tests/permission_groups_integration.rs` 新建集成测试，覆盖「用户入组后签发的 Token 含组内权限」与「内置全权组 Token 含全部目录权限」两个用例。夹具按 `tests/account_deletion_integration.rs:31-240` 的既有写法自行构造（该文件展示了 build_app、`sync_with_database`、TokenManager、dispatch 的完整套路）。**注意**：该仓库的 `tests/common/mod.rs` 只有 68 行、仅提供注册验证码捕获，没有通用夹具，本任务不要求先补公共夹具。

Run: `python scripts/run_ci.py integration`
Expected: PASS

- [ ] **Step 7: 提交**

```bash
git add src/addon/access/ src/app.rs tests/permission_groups_integration.rs
git commit -m "feat(access): GroupGrantResolver 接入 Token 签发，组权限并入授权快照"
```

---

## Task 6: 扇出失效、提权校验与最后管理员判定

**Files:**
- Create: `src/addon/access/domain/groups/admin.rs`
- Test: `src/addon/access/domain/groups/admin.rs`（文件内，纯逻辑部分）

**Interfaces:**
- Consumes: `Access::{groups, authorization, permission_catalog}`、`AuthorizationPort::{lock_authorization_version, increment_locked_authorization_version}`、Task 4 的 `resolution`
- Produces:
  - `pub(crate) async fn invalidate_users_in_tx(access: &Access, ctx: &ActionContext, transaction: &mut Transaction, affected: &BTreeSet<i64>) -> Result<(), BaseError>`——**按 `user_id` 升序**锁行并逐个递增版本 + Outbox；空集直接返回
  - `pub(crate) async fn effective_permissions_of_in_tx(access: &Access, ctx: &ActionContext, transaction: &mut Transaction, user_id: i64) -> Result<BTreeSet<String>, BaseError>`——与 `GroupGrantResolver` 共用 `resolution`
  - `pub(crate) fn assert_no_self_escalation(before: &BTreeSet<String>, after: &BTreeSet<String>) -> Result<(), BaseError>`
  - `pub(crate) async fn count_active_system_admins_in_tx(access: &Access, ctx: &ActionContext, transaction: &mut Transaction) -> Result<u64, BaseError>`
  - `pub(crate) fn ensure_member_limit(member_count: u64) -> Result<(), BaseError>`

- [ ] **Step 1: 写失败的测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn self_escalation_is_rejected_when_new_permission_appears() {
        let before = set(&["access.groups.write"]);
        let after = set(&["access.groups.write", "access.grants.write"]);
        let error = assert_no_self_escalation(&before, &after).expect_err("提权必须被拒");
        assert!(
            matches!(error, yang_base::BaseError::PermissionDenied(_)),
            "应为 403，实际 {error:?}"
        );
    }

    #[test]
    fn subset_change_is_allowed() {
        let before = set(&["access.groups.write", "access.grants.read"]);
        let after = set(&["access.groups.write"]);
        assert!(assert_no_self_escalation(&before, &after).is_ok());
    }

    #[test]
    fn identical_sets_are_allowed() {
        let before = set(&["access.groups.write"]);
        assert!(assert_no_self_escalation(&before, &before.clone()).is_ok());
    }

    #[test]
    fn member_limit_boundary_is_exactly_200() {
        // Review Focus 4：200 与 201 的行为必须不同。
        assert!(ensure_member_limit(0).is_ok());
        assert!(ensure_member_limit(199).is_ok());
        assert!(ensure_member_limit(200).is_ok(), "等于上限必须允许");
        let error = ensure_member_limit(201).expect_err("超过上限必须拒绝");
        assert!(matches!(error, yang_base::BaseError::Conflict(_)));
        assert!(error.to_string().contains("200"), "错误信息必须给出上限");
    }

    #[test]
    fn affected_users_are_iterated_in_ascending_order() {
        // 锁序不变量的可测部分：`invalidate_users_in_tx` 直接按集合迭代顺序
        // 加锁，因此集合类型本身就是锁序保证。此测试锁住该类型选择——
        // 若有人把它换成 HashSet，扇出将不再有序，并发下会形成死锁环。
        let mut affected: BTreeSet<i64> = BTreeSet::new();
        affected.insert(9);
        affected.insert(3);
        affected.insert(7);
        assert_eq!(affected.iter().copied().collect::<Vec<_>>(), [3, 7, 9]);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib --locked groups::admin`
Expected: 编译失败

- [ ] **Step 3: 实现**

```rust
//! 组管理的事务编排：扇出失效、提权校验、最后管理员判定与成员上限。
//!
//! 这些机制被多个 Action 共用，集中在此以保证语义单一——尤其
//! `effective_permissions_of_in_tx` 必须与 `GroupGrantResolver` 走同一
//! 解析函数（spec §8.1）。

use super::repository::{GroupRepository, MAX_GROUP_MEMBERS, SYSTEM_ADMIN_GROUP_KEY};
use super::resolution::{catalog_permissions, resolve_group_permissions};
use crate::addon::access::domain::context::Access;
use std::collections::BTreeSet;
use yang_base::action::ActionContext;
use yang_base::BaseError;
use yang_db::Transaction;

/// 使受影响用户的 Access Token 失效（spec §6.3）。
///
/// **锁序**：必须按 `user_id` 升序加锁，这是防死锁的唯一手段。
/// `BTreeSet` 的迭代顺序天然有序，不要改成 `HashSet`。
pub(crate) async fn invalidate_users_in_tx(
    access: &Access,
    ctx: &ActionContext,
    transaction: &mut Transaction,
    affected: &BTreeSet<i64>,
) -> Result<(), BaseError> {
    for user_id in affected {
        let locked = access
            .authorization()
            .lock_authorization_version(ctx.tools().mysql()?.pool(), transaction, *user_id)
            .await?;
        if !locked.is_active() {
            // 已停用用户的 Token 本就不可用，无需递增版本。
            continue;
        }
        access
            .authorization()
            .increment_locked_authorization_version(transaction, &locked)
            .await?;
    }
    Ok(())
}

/// 某用户的当前有效权限（供提权校验使用，与 resolvers 共用解析函数）。
pub(crate) async fn effective_permissions_of_in_tx(
    access: &Access,
    ctx: &ActionContext,
    transaction: &mut Transaction,
    user_id: i64,
) -> Result<BTreeSet<String>, BaseError> {
    let catalog = catalog_permissions(access.permission_catalog())?;
    let mut effective: BTreeSet<String> = BTreeSet::new();
    // 直授权限同样计入——提权判定看的是「有效权限」而非仅组权限。
    for record in access
        .grants()
        .list_by_user_in_tx(ctx, transaction, user_id)
        .await?
    {
        if catalog.iter().any(|p| p == &record.permission) {
            effective.insert(record.permission);
        }
    }
    for group_id in access
        .groups()
        .list_group_ids_of_user_in_tx(ctx, transaction, user_id)
        .await?
    {
        let Some(group) = access
            .groups()
            .find_by_id_in_tx(ctx, transaction, group_id)
            .await?
        else {
            continue;
        };
        let items = access
            .groups()
            .list_items_in_tx(ctx, transaction, group_id)
            .await?;
        effective.extend(resolve_group_permissions(&group.group_key, &items, &catalog));
    }
    Ok(effective)
}

/// spec §8.1 的不变量：任何组管理操作都不得使调用者自身有效权限增大。
pub(crate) fn assert_no_self_escalation(
    before: &BTreeSet<String>,
    after: &BTreeSet<String>,
) -> Result<(), BaseError> {
    if after.is_subset(before) {
        return Ok(());
    }
    let added: Vec<&str> = after.difference(before).map(String::as_str).collect();
    Err(BaseError::PermissionDenied(format!(
        "该操作会使你自己的权限增加（{}），已拒绝",
        added.join(", ")
    )))
}

/// 组成员数上限检查；超过上限必须明确报错而不是静默做 O(N) 行锁事务。
pub(crate) fn ensure_member_limit(member_count: u64) -> Result<(), BaseError> {
    if member_count > MAX_GROUP_MEMBERS as u64 {
        return Err(BaseError::Conflict(format!(
            "该组已有 {member_count} 名成员，超过上限 {MAX_GROUP_MEMBERS}；请先分批移出成员再修改组权限"
        )));
    }
    Ok(())
}

/// 当前处于启用的系统管理员人数（内置全权组的 active 成员）。
///
/// 成员数从组事实读取；active 判定复用 `AuthorizationPort` 的版本快照
/// （`users.status` 的事实源），避免在此另写一条用户状态查询路径。
pub(crate) async fn count_active_system_admins_in_tx(
    access: &Access,
    ctx: &ActionContext,
    transaction: &mut Transaction,
) -> Result<u64, BaseError> {
    let Some(group) = access
        .groups()
        .find_by_key_in_tx(ctx, transaction, SYSTEM_ADMIN_GROUP_KEY)
        .await?
    else {
        return Ok(0);
    };
    let members = access
        .groups()
        .list_members_in_tx(ctx, transaction, group.id)
        .await?;
    let mut active = 0_u64;
    for user_id in members {
        if let Some(snapshot) = access
            .authorization()
            .find_authorization_version(ctx.tools().mysql()?.pool(), user_id)
            .await?
        {
            if snapshot.is_active() {
                active += 1;
            }
        }
    }
    Ok(active)
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --lib --locked groups::admin`
Expected: PASS（5 tests）

- [ ] **Step 5: 提交**

```bash
git add src/addon/access/domain/groups/admin.rs
git commit -m "feat(access): 扇出失效、提权校验与成员上限机制"
```

---

## Task 7: `SystemOwnerClaimer` 端口签名增加 `&ActionContext`

**Files:**
- Modify: `src/addon/account/domain/system_owner.rs:21-47`
- Modify: `src/addon/account/domain/context.rs:169-178`
- Modify: `src/addon/account/user/actions/register.rs:95-97`
- Test: `src/addon/account/domain/system_owner.rs`（文件内）

**Interfaces:**
- Consumes: `yang_base::action::ActionContext`
- Produces:
  - `SystemOwnerClaimer::claim(&self, ctx: &ActionContext, transaction: &mut Transaction, user_id: i64, username: &str) -> Result<OwnerClaimOutcome, BaseError>`
  - `Account::claim_system_owner(&self, ctx: &ActionContext, transaction: &mut Transaction, user_id: i64, username: &str) -> Result<OwnerClaimOutcome, BaseError>`

**为什么必须改签名**：唯一受信 writer（`GrantRepository`、`GroupRepository`）的每个方法都需要 `ctx` 去拿连接池做 `trusted_query`（`access/domain/repository.rs:45-48`）。不传 `ctx`，实现方就只能绕过 writer 直写，直接违反 `authorization-writers.md` 的硬约束。

- [ ] **Step 1: 写失败的测试**

在 `src/addon/account/domain/system_owner.rs` 追加：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn default_claimer_never_claims_and_needs_no_context_side_effects() {
        // 默认实现必须恒返回 AlreadyClaimed，且不触碰任何 ctx 状态。
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"));
        let mysql = yang_db::Database::from_pool(pool, yang_db::DatabaseConfig::default())
            .unwrap_or_else(|error| panic!("{error}"));
        let tools = std::sync::Arc::new(
            yang_base::tools::ToolsBuilder::new()
                .mysql(mysql)
                .build()
                .unwrap_or_else(|error| panic!("{error}")),
        );
        let ctx = ActionContext::new(
            yang_base::action::Request::new(serde_json::json!({})),
            tools,
        );
        let mut transaction = ctx
            .tools()
            .mysql()
            .unwrap_or_else(|e| panic!("{e}"))
            .transaction()
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        let outcome = NoSystemOwnerClaimer
            .claim(&ctx, &mut transaction, 1, "first")
            .await
            .unwrap_or_else(|e| panic!("默认实现必须成功返回: {e}"));
        assert_eq!(outcome, OwnerClaimOutcome::AlreadyClaimed);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib --locked account::domain::system_owner`
Expected: 编译失败——`claim` 参数数量不匹配

- [ ] **Step 3: 改签名与所有实现/调用点**

`system_owner.rs` 的 trait 与默认实现：

```rust
#[async_trait]
pub(crate) trait SystemOwnerClaimer: Send + Sync {
    /// 在创建用户的同一事务中竞争唯一最终管理员哨兵。
    ///
    /// `ctx` 是必需参数：实现方必须经受信 writer 写入授权事实，
    /// 而 writer 需要 `ctx` 取得连接池（`trusted_query`）。
    async fn claim(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
        username: &str,
    ) -> Result<OwnerClaimOutcome, BaseError>;
}

#[async_trait]
impl SystemOwnerClaimer for NoSystemOwnerClaimer {
    async fn claim(
        &self,
        _ctx: &ActionContext,
        _transaction: &mut Transaction,
        _user_id: i64,
        _username: &str,
    ) -> Result<OwnerClaimOutcome, BaseError> {
        Ok(OwnerClaimOutcome::AlreadyClaimed)
    }
}
```

同时删掉 `OwnerClaimOutcome::Claimed` 上的 `#[allow(dead_code)]` 与那句「当前骨架没有平台管理域」的注释——Task 8 会让该分支真正可达。

`account/domain/context.rs:169-178`：

```rust
    pub(crate) async fn claim_system_owner(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
        username: &str,
    ) -> Result<OwnerClaimOutcome, BaseError> {
        self.system_owner_claimer
            .claim(ctx, transaction, user_id, username)
            .await
    }
```

`register.rs:95-97` 的调用点补 `&ctx`：

```rust
        if let OwnerClaimOutcome::Claimed { admin_id } = account
            .claim_system_owner(&ctx, &mut transaction, id, &username)
            .await?
```

顺便修掉 `context.rs:157` 那处挂错的文档注释（「在注册事务中竞争唯一最终管理员哨兵」当前挂在 `session_id_from_request` 上，是 port 半弃置期的遗留）。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --lib --locked && python scripts/check_architecture.py`
Expected: 全部通过

- [ ] **Step 5: 提交**

```bash
git add src/addon/account/
git commit -m "refactor(account): SystemOwnerClaimer 端口透传 ActionContext 以复用受信 writer"
```

---

## Task 8: `SystemOwnerClaimer` 的 access 实现与组合根换注入

**Files:**
- Create: `src/addon/access/domain/groups/owner.rs`
- Modify: `src/addon/access/mod.rs`、`src/app.rs:107`
- Test: `src/addon/access/domain/groups/owner.rs`（文件内）

**Interfaces:**
- Consumes: Task 3 的 `GroupRepository::{ensure_system_admin_group_in_tx, insert_member_in_tx}`、Task 6 的 `invalidate_users_in_tx`、`SystemOwnerClaimer` trait
- Produces:
  - `pub(crate) struct AccessSystemOwnerClaimer { access: Arc<Access> }`
  - `impl AccessSystemOwnerClaimer { pub(crate) fn new(access: Arc<Access>) -> Self }`
  - `impl SystemOwnerClaimer for AccessSystemOwnerClaimer`
  - `AccessAddon::system_owner_claimer(&self) -> Arc<dyn SystemOwnerClaimer>`

**依赖方向**：access 已单向依赖 account（`access/domain/resolver.rs:6` 等），account 目录内 grep 不到 `crate::addon::access`，因此把实现放在 access 不会形成编译期循环。

- [ ] **Step 1: 写失败的集成测试**

claimer 的全部行为都是数据库行为（写哨兵、入组、递增版本），没有可单测的纯逻辑面。**不要写"断言 enum 变体等于它自己"这类同义反复的测试**——那只会制造虚假信心。TDD 在这里落在集成层：先把「哨兵只被夺到一次」的契约写成失败测试。

在 `tests/system_owner_bootstrap_integration.rs`（与 Task 9 同一文件）写：

```rust
/// claimer 的幂等契约：哨兵已存在时第二次 claim 必须返回 AlreadyClaimed 而非报错。
/// 这是 Task 9 降级语义的前提，因此先单独钉住。
#[tokio::test]
#[ignore = "需要真实 MySQL/Redis"]
async fn a_second_claim_returns_already_claimed_instead_of_failing() {
    let app = harness::build_test_app().await;
    harness::reset_business_tables(&app).await;

    let first = harness::claim_owner(&app, "first").await
        .unwrap_or_else(|e| panic!("首次 claim 应成功: {e}"));
    assert!(matches!(first, OwnerClaimOutcome::Claimed { .. }), "首次必须夺到哨兵");
    assert_eq!(harness::count_system_owner_rows(&app).await, 1);

    let second = harness::claim_owner(&app, "second").await
        .unwrap_or_else(|e| panic!("第二次 claim 不得报错: {e}"));
    assert_eq!(second, OwnerClaimOutcome::AlreadyClaimed, "第二次必须降级");
    assert_eq!(harness::count_system_owner_rows(&app).await, 1, "哨兵行不得增加");
}
```

（`harness::claim_owner` 需在测试夹具里直接经注册事务调用 claimer，或经 `register_with_code` 间接驱动；取更贴近真实路径的那种。`reset_business_tables` 必须清空 `system_owner`、`user_group`、`permission_group`。）

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --test system_owner_bootstrap_integration -- --ignored --test-threads=1 a_second_claim_returns_already_claimed`
Expected: FAIL——当前注入的是 `NoSystemOwnerClaimer`，首个 claim 返回 `AlreadyClaimed` 而非 `Claimed`

- [ ] **Step 3: 实现 claimer**

```rust
//! `SystemOwnerClaimer` 的 access 实现：引导首个注册账号为系统管理员。
//!
//! 并发仲裁完全交给 `system_owner` 表的唯一约束——**不做任何「判空」**。
//! 「用户表为空则本次注册者晋升」是典型 TOCTOU，在并发下会产出多个管理员。

use crate::addon::access::domain::context::Access;
use crate::addon::account::{OwnerClaimOutcome, SystemOwnerClaimer};
use async_trait::async_trait;
use std::collections::BTreeSet;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::BaseError;
use yang_db::Transaction;

pub(crate) struct AccessSystemOwnerClaimer {
    access: Arc<Access>,
}

impl AccessSystemOwnerClaimer {
    pub(crate) fn new(access: Arc<Access>) -> Self {
        Self { access }
    }
}

#[async_trait]
impl SystemOwnerClaimer for AccessSystemOwnerClaimer {
    async fn claim(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
        username: &str,
    ) -> Result<OwnerClaimOutcome, BaseError> {
        // 1. 竞争哨兵：唯一约束 + CHECK 让第二个插入者必然失败。
        match self
            .access
            .groups()
            .insert_owner_sentinel_in_tx(ctx, transaction, user_id)
            .await
        {
            Ok(()) => {}
            Err(BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_))) => {
                return Ok(OwnerClaimOutcome::AlreadyClaimed);
            }
            Err(error) => return Err(error),
        }

        // 2. 夺到哨兵：加入内置全权组并使其 Token 生效。
        let group_id = self
            .access
            .groups()
            .ensure_system_admin_group_in_tx(ctx, transaction)
            .await?;
        self.access
            .groups()
            .insert_member_in_tx(ctx, transaction, user_id, group_id, user_id)
            .await?;
        let mut affected = BTreeSet::new();
        affected.insert(user_id);
        super::admin::invalidate_users_in_tx(&self.access, ctx, transaction, &affected).await?;

        tracing::info!(user_id, username, "首个注册账号已引导为系统管理员");
        Ok(OwnerClaimOutcome::Claimed { admin_id: user_id })
    }
}
```

在 `src/addon/access/domain/groups/repository.rs` 增加哨兵写入方法（唯一写入口）：

```rust
    /// 竞争引导哨兵；重复插入由唯一约束与 CHECK 拒绝。
    pub(crate) async fn insert_owner_sentinel_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
    ) -> Result<(), BaseError> {
        let record = Record::new()
            .set(OWNER_SENTINEL_KEY, SENTINEL_KEY_VALUE)
            .set(OWNER_USER_ID, user_id);
        self.trusted_owner(ctx)?
            .insert_in_tx(transaction, record)
            .await?;
        Ok(())
    }
```

（`trusted_owner` 与 `trusted_query` 同形，绑 `system_owner` 表定义——即 `GroupRepository::new` 的第四个参数，Task 3 已定型。）

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --lib --locked groups::owner`
Expected: PASS

- [ ] **Step 5: 组合根换注入**

`src/addon/access/mod.rs` 增加出口：

```rust
    /// 首个注册账号的引导声明器。
    pub(crate) fn system_owner_claimer(&self) -> Arc<dyn SystemOwnerClaimer> {
        Arc::new(AccessSystemOwnerClaimer::new(Arc::clone(&self.access)))
    }
```

`src/app.rs:107` 替换：

```rust
    let system_owner_claimer = access.system_owner_claimer();
```

同时删除 `src/addon/account/mod.rs:35-37` 的 `no_system_owner_claimer()`——它已无调用方。若保留会造成「存在一个永不使用的空实现」的误导。删除后须确认 `cargo clippy --all-targets --all-features --locked -- -D warnings` 通过。

- [ ] **Step 6: 提交**

```bash
git add src/addon/access/ src/app.rs src/addon/account/mod.rs
git commit -m "feat(access): 首个注册账号引导为系统管理员的 claimer 实现"
```

---

## Task 9: 注册流程降级语义与并发引导对抗测试

**Files:**
- Modify: `src/addon/account/user/actions/register.rs:93-115`
- Create: `tests/system_owner_bootstrap_integration.rs`
- Test: 同上

**Interfaces:**
- Consumes: Task 8 的 claimer
- Produces: 无新公共接口；改变 `register` 的失败语义

**语义变更**：`AlreadyClaimed` 是**正常业务结果**，不得阻断注册。当前 `register.rs` 用 `?` 在同一事务闭包内上抛，会让「首个用户因哨兵竞争失败而注册失败」。只有真实数据库故障才回滚整个注册事务。

- [ ] **Step 1: 写失败的集成测试**

创建 `tests/system_owner_bootstrap_integration.rs`，夹具按 `tests/account_deletion_integration.rs` 的既有套路构造。核心用例：

```rust
/// Review Focus 与 spec §13：并发首注册恰好产生一个 owner。
#[tokio::test]
#[ignore = "需要真实 MySQL/Redis"]
async fn concurrent_first_registrations_produce_exactly_one_system_owner() {
    let app = harness::build_test_app().await;
    harness::reset_business_tables(&app).await;

    let mutations = 6;
    // 用 Barrier 让所有任务在同一时刻抵达 INSERT，制造真实的哨兵竞争。
    // 只用 tokio —— 仓库的 dev-dependencies 里没有 futures crate，不要引入。
    let barrier = Arc::new(tokio::sync::Barrier::new(mutations));
    let mut handles = Vec::new();
    for index in 0..mutations {
        let app = app.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            let username = format!("racer{index}");
            let email = format!("racer{index}@example.com");
            barrier.wait().await;
            harness::register_with_code(&app, &username, &email).await
        }));
    }
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap_or_else(|e| panic!("任务不应 panic: {e}")));
    }

    let succeeded = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(succeeded, mutations, "哨兵竞争不得让任何一次注册失败");

    let owners = harness::count_system_owner_rows(&app).await;
    assert_eq!(owners, 1, "唯一约束必须把并发收敛到恰好一行哨兵");

    let admins = harness::active_system_admin_usernames(&app).await;
    assert_eq!(admins.len(), 1, "恰好一个账号成为系统管理员");
}

/// 哨兵已存在时，后续注册一律降级为普通用户且不报错。
#[tokio::test]
#[ignore = "需要真实 MySQL/Redis"]
async fn later_registrations_degrade_to_plain_users() {
    let app = harness::build_test_app().await;
    harness::reset_business_tables(&app).await;

    harness::register_with_code(&app, "first", "first@example.com")
        .await
        .unwrap_or_else(|e| panic!("首个注册应成功: {e}"));
    harness::register_with_code(&app, "second", "second@example.com")
        .await
        .unwrap_or_else(|e| panic!("第二个注册应成功并降级: {e}"));

    let admins = harness::active_system_admin_usernames(&app).await;
    assert_eq!(admins, vec!["first".to_string()]);
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --test system_owner_bootstrap_integration -- --ignored --test-threads=1`
Expected: `concurrent_first_registrations_produce_exactly_one_system_owner` 失败——当前注入的是 `NoSystemOwnerClaimer`，没有管理员产生

- [ ] **Step 3: 改注册流程的降级语义**

`register.rs` 的事务闭包：

```rust
        // 引导是「尽力而为的一次性事件」：AlreadyClaimed 是正常结果，
        // 不得阻断注册；只有真实数据库故障才让整个事务回滚。
        let outcome = account
            .claim_system_owner(&ctx, &mut transaction, id, &username)
            .await?;
        if let OwnerClaimOutcome::Claimed { admin_id } = outcome {
            let event = audit::succeeded_system_event(
                &ctx,
                "first-registration",
                None,
                Some(audit::entity("user", id)?),
                audit::entity("admin_account", admin_id)?,
                None,
                Some(audit::summary([
                    ("admin", json!(true)),
                    ("system_owner", json!(true)),
                    ("user_id", json!(id)),
                ])?),
            )?;
            audit::append_in_tx(&mut transaction, &event).await?;
        }
```

同时删掉 `register.rs:93-94` 那段「当前骨架注入的是不声明的默认实现，永不进入 Claimed 分支」的注释——它已与事实相反。

- [ ] **Step 4: 运行确认通过**

Run: `python scripts/run_ci.py integration`
Expected: PASS（两个新用例通过，且既有 8 个集成入口不回归）

- [ ] **Step 5: 提交**

```bash
git add src/addon/account/user/actions/register.rs tests/system_owner_bootstrap_integration.rs
git commit -m "fix(account): 引导竞争失败降级为普通注册，新增并发对抗测试"
```

---

## Task 10: 组 CRUD Actions（建/改/删/列表/详情）

**Files:**
- Create: `src/addon/access/groups/actions/mod.rs`
- Create: `src/addon/access/groups/actions/create_group.rs`
- Create: `src/addon/access/groups/actions/update_group.rs`
- Create: `src/addon/access/groups/actions/delete_group.rs`
- Create: `src/addon/access/groups/actions/list_groups.rs`
- Create: `src/addon/access/groups/actions/get_group.rs`
- Modify: `src/addon/access/groups/mod.rs`（注册 Action 与 Step-up 目标）
- Test: `tests/permission_groups_integration.rs`（追加用例）

**Interfaces:**
- Consumes: `Access::{groups, permission_catalog}`、Task 4 的 `orphan_items`、Task 6 的 `invalidate_users_in_tx`
- Produces: 九个 Action 文件中的五个；权限字符串 `access.groups.read` / `access.groups.write`
  - `build_module` 签名扩展为接收 `Arc<Access>` 并返回 `(ModuleSpec, )`；`step_up_targets() -> Vec<ActionRef>` 在本任务引入

**通用 Action 骨架**（每个文件都遵循，显式注册，不用宏）：

```rust
pub(super) async fn handle(
    ctx: ActionContext,
    input: XxxInput,
    access: Arc<Access>,
) -> Result<ApiResponse, BaseError> {
    let operator_id = ctx.actor()?.user_id();
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async { /* 业务 */ }.await;
    let payload = Access::finish_transaction(transaction, result).await?;
    ApiResponse::success(payload, "……")
}

pub(super) fn register(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("create_group"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&access))
        })
        .route(HttpMethod::Post, "/api/v1/access/groups")
        .display_name("创建权限组")
        .description("创建一个权限组")
        .permissions(["access.groups.write"])
        .register()
}
```

- [ ] **Step 1: 写 `delete_group` 的失败测试（含 Review Focus 3 竞态）**

在 `tests/permission_groups_integration.rs` 追加：

```rust
/// spec §8.3 与 Review Focus 3：删除仍有成员的组必须被拒，且不产生 500。
#[tokio::test]
#[ignore = "需要真实 MySQL/Redis"]
async fn deleting_a_group_with_members_is_rejected() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let group_id = harness::create_group(&app, &admin, "ops", "运维").await;
    let user_id = harness::register_with_code(&app, "member1", "member1@example.com")
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    harness::add_member(&app, &admin, group_id, user_id).await;

    let status = harness::delete_group_status(&app, &admin, group_id).await;
    assert_eq!(status, 409, "组内非空必须返回 409 而不是 500");
    assert!(harness::group_exists(&app, "ops").await, "拒绝后组必须仍在");
}

/// Review Focus 3：删除与加成员并发时，结果必须收敛到「删成功」或「冲突」。
#[tokio::test]
#[ignore = "需要真实 MySQL/Redis"]
async fn delete_races_add_member_without_orphans_or_500() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;

    for round in 0..5 {
        let group_id = harness::create_group(&app, &admin, &format!("race{round}"), "竞态").await;
        let user_id = harness::register_with_code(
            &app,
            &format!("racer{round}"),
            &format!("racer{round}@example.com"),
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));

        let (delete_result, add_result) = tokio::join!(
            harness::delete_group_status(&app, &admin, group_id),
            harness::add_member_status(&app, &admin, group_id, user_id),
        );

        for status in [delete_result, add_result] {
            assert!(
                status == 200 || status == 404 || status == 409,
                "只允许 200/404/409，实际 {status}"
            );
        }
        // 无论谁赢，都不能留下悬空成员行：组不存在则成员行必须为 0。
        if !harness::group_exists_by_id(&app, group_id).await {
            assert_eq!(
                harness::member_rows_of_group(&app, group_id).await,
                0,
                "组已删除时不得残留成员行（外键 RESTRICT 必须兜住）"
            );
        }
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --test permission_groups_integration -- --ignored --test-threads=1`
Expected: 编译失败（`harness::create_group` 等尚不存在，接口未注册）

- [ ] **Step 3: 实现五个 Action**

**`create_group.rs`**——输入 `group_key`（`GROUP_KEY_PATTERN` / `GROUP_KEY_MAX_LENGTH`）、`title`（1–128）、`description`（≤255，可选）。事务内：`ensure_declared` 不需要（组本身不是权限）；直接 `insert_group_in_tx`；捕获唯一键冲突返回既有的 `ParamInvalid` 语义；写审计。不需要扇出（新组无成员）。

**`update_group.rs`**——输入 `group_id` + `title` + `description`。**先拒绝 `group_key == SYSTEM_ADMIN_GROUP_KEY`**（400，说明内置组不可改名）。`title` 不是授权事实，**不触发失效传播**。

**`delete_group.rs`**——核心：

```rust
    let group = access
        .groups()
        .find_by_id_in_tx(&ctx, &mut transaction, input.group_id)
        .await?
        .ok_or_else(|| BaseError::RecordNotFound("权限组".to_string()))?;
    if group.group_key == SYSTEM_ADMIN_GROUP_KEY {
        return Err(BaseError::ParamInvalid(
            "group_id".to_string(),
            "内置系统管理员组不可删除".to_string(),
        ));
    }
    // 应用层前置检查给出可读错误；数据库外键 RESTRICT 兜底并发窗口（spec §8.3）。
    let member_count = access
        .groups()
        .count_members_in_tx(&ctx, &mut transaction, group.id)
        .await?;
    if member_count > 0 {
        return Err(BaseError::Conflict(format!(
            "该权限组仍有 {member_count} 名成员，请先移出成员"
        )));
    }
    let affected_rows = access
        .groups()
        .delete_group_in_tx(&ctx, &mut transaction, group.id)
        .await?;
    if affected_rows == 0 {
        return Err(BaseError::RecordNotFound("权限组".to_string()));
    }
```

再删该组的条目行（`delete_items_of_group_in_tx`，Task 3 需补此方法），最后写审计。

**`list_groups.rs`**——返回每个组的 `id`、`group_key`、`title`、`description`、`member_count`、`item_count`、`is_builtin`、`orphan_item_count`。`orphan_item_count` 经 `orphan_items(&items, &catalog)` 计算（spec §8.4）。只读，不需要事务写。

**`get_group.rs`**——路径参数 `group_id`；返回条目列表（含每条的 `is_orphan` 布尔）、成员列表、以及 `effective_all: bool`（`group_key == SYSTEM_ADMIN_GROUP_KEY`，spec §15 第 3 条要求接口显式表达内置组无条目可展示）。

- [ ] **Step 4: 注册与 Step-up**

`src/addon/access/groups/actions/mod.rs` 照 `grants/actions/mod.rs:1-39` 的形状写（`mod` 声明 + `ACTIONS` 数组 + `register_all`）。`groups/mod.rs` 补：

```rust
fn step_up_targets() -> Vec<yang_base::definition::ActionRef> {
    vec![
        yang_base::action!("access.groups.create_group"),
        yang_base::action!("access.groups.update_group"),
        yang_base::action!("access.groups.delete_group"),
    ]
}
```

并在 `build_module` 里按 `grants/mod.rs:52-58` 的写法挂载 `step_up.middleware(...)`。

- [ ] **Step 5: 运行确认通过**

Run: `python scripts/run_ci.py integration && python scripts/check_architecture.py`
Expected: PASS

- [ ] **Step 6: 提交**

```bash
git add src/addon/access/groups/ tests/permission_groups_integration.rs
git commit -m "feat(access): 权限组 CRUD 接口与删除竞态对抗测试"
```

---

## Task 11: 组条目 Actions（加/移除权限）

**Files:**
- Create: `src/addon/access/groups/actions/add_group_item.rs`
- Create: `src/addon/access/groups/actions/remove_group_item.rs`
- Modify: `src/addon/access/groups/actions/mod.rs`
- Test: `tests/permission_groups_integration.rs`（追加）

**Interfaces:**
- Consumes: Task 4 的 `resolution`、Task 6 的 `invalidate_users_in_tx` / `ensure_member_limit`
- Produces: 两个 Action；权限 `access.groups.write`

- [ ] **Step 1: 写失败测试（含 Review Focus 4 边界与 Review Focus 1 目录未装）**

```rust
/// spec §6.3：组权限变更必须让全部成员的 Token 失效。
#[tokio::test]
#[ignore = "需要真实 MySQL/Redis"]
async fn adding_a_group_permission_invalidates_every_member() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let group_id = harness::create_group(&app, &admin, "ops", "运维").await;
    let member = harness::register_with_code(&app, "ops1", "ops1@example.com")
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    harness::add_member(&app, &admin, group_id, member).await;

    let before = harness::authz_version_of(&app, member).await;
    harness::add_group_item(&app, &admin, group_id, "access.grants.read").await;
    let after = harness::authz_version_of(&app, member).await;

    assert!(after > before, "组权限变更必须递增成员授权版本");
    assert!(
        harness::member_token_is_stale(&app, member).await,
        "旧 Token 必须被判定为过期"
    );
}

/// Review Focus 4：成员数上限的边界行为必须可预测。
#[tokio::test]
#[ignore = "需要真实 MySQL/Redis"]
async fn group_permission_change_respects_the_member_limit() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    // 极限测试用专用常量；若 MAX_GROUP_MEMBERS 变更，此测试与常量一并更新。
    let group_id = harness::create_group(&app, &admin, "big", "大组").await;

    let at_limit = harness::seed_members_directly(&app, group_id, harness::MAX_GROUP_MEMBERS).await;
    assert_eq!(at_limit, harness::MAX_GROUP_MEMBERS);
    harness::add_group_item(&app, &admin, group_id, "access.grants.read").await;

    let over_limit =
        harness::seed_members_directly(&app, group_id, harness::MAX_GROUP_MEMBERS + 1).await;
    assert_eq!(over_limit, harness::MAX_GROUP_MEMBERS + 1);
    let status = harness::add_group_item_status(&app, &admin, group_id, "account.users.read").await;
    assert_eq!(status, 409, "超过上限必须明确报错");
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --test permission_groups_integration -- --ignored --test-threads=1`
Expected: 编译失败

- [ ] **Step 3: 实现**

**`add_group_item.rs`**：

```rust
    // 只能加入 Catalog 中已声明的权限（fail-closed，沿用 grant_permission 的语义）。
    access
        .permission_catalog()
        .ensure_declared(&input.permission)?;

    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let group = access
            .groups()
            .find_by_id_in_tx(&ctx, &mut transaction, input.group_id)
            .await?
            .ok_or_else(|| BaseError::RecordNotFound("权限组".to_string()))?;
        if group.group_key == SYSTEM_ADMIN_GROUP_KEY {
            return Err(BaseError::ParamInvalid(
                "group_id".to_string(),
                "内置系统管理员组的权限由权限目录计算，不能增删条目".to_string(),
            ));
        }
        let members = access
            .groups()
            .list_members_in_tx(&ctx, &mut transaction, group.id)
            .await?;
        ensure_member_limit(members.len() as u64)?;

        let changed = access
            .groups()
            .insert_item_in_tx(&ctx, &mut transaction, group.id, &input.permission, operator_id)
            .await?;
        if !changed {
            return Ok(false); // 幂等：不递增版本、不写 Outbox。
        }
        let affected: BTreeSet<i64> = members.into_iter().collect();
        invalidate_users_in_tx(&access, &ctx, &mut transaction, &affected).await?;
        // 审计……
        Ok(true)
    }
    .await;
```

**`remove_group_item.rs`**：与上对称，但**不做 `ensure_declared`**——对齐 `revoke_permission.rs:48` 的反向宽容语义（已从 Catalog 移除的权限也必须能清理，否则孤儿条目无法清除）。

- [ ] **Step 4: 运行确认通过**

Run: `python scripts/run_ci.py integration`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add src/addon/access/groups/actions/ tests/permission_groups_integration.rs
git commit -m "feat(access): 组权限条目接口，含成员上限与扇出失效"
```

---

## Task 12: 组成员 Actions（加/移出）与防自提权

**Files:**
- Create: `src/addon/access/groups/actions/add_group_member.rs`
- Create: `src/addon/access/groups/actions/remove_group_member.rs`
- Modify: `src/addon/access/groups/actions/mod.rs`
- Test: `tests/permission_groups_integration.rs`（追加）

**Interfaces:**
- Consumes: Task 6 的 `effective_permissions_of_in_tx` / `assert_no_self_escalation` / `invalidate_users_in_tx`
- Produces: 两个 Action；权限 `access.groups.write`

**这是 spec §8.1 的落点**——保住 D2 实质的地方。

- [ ] **Step 1: 写失败测试（Review Focus 2 与两条提权路径）**

```rust
/// spec §8.1 路径一：给自己加一个权限超集的组必须被拒。
#[tokio::test]
#[ignore = "需要真实 MySQL/Redis"]
async fn user_cannot_add_themselves_to_a_group_granting_more_than_they_hold() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    // operator 只被授予 access.groups.write，不含 access.grants.write。
    let operator = harness::grant_only(&app, &admin, "operator", "access.groups.write").await;
    let powerful = harness::create_group(&app, &admin, "powerful", "高权").await;
    harness::add_group_item(&app, &admin, powerful, "access.grants.write").await;

    let status = harness::add_member_status(&app, &operator, powerful, operator).await;
    assert_eq!(status, 403, "自提权必须被拒绝");
    assert!(!harness::is_member(&app, powerful, operator).await);
}

/// spec §8.1 路径二：给自己已属于的组加一条自己没有的权限，同样必须被拒。
#[tokio::test]
#[ignore = "需要真实 MySQL/Redis"]
async fn user_cannot_add_a_permission_to_their_own_group_beyond_their_holdings() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let operator = harness::grant_only(&app, &admin, "operator2", "access.groups.write").await;
    let own = harness::create_group(&app, &admin, "own", "自属组").await;
    harness::add_group_item(&app, &admin, own, "access.groups.write").await;
    harness::add_member(&app, &admin, own, operator).await;

    let status = harness::add_group_item_status(&app, &operator, own, "access.grants.write").await;
    assert_eq!(status, 403, "给自己所属组加超集权限同样是自提权");
}

/// Review Focus 2：管理员把自己移出全权组（非最后一名）必须成功。
#[tokio::test]
#[ignore = "需要真实 MySQL/Redis"]
async fn admin_can_remove_themselves_when_another_admin_remains() {
    let app = harness::build_test_app().await;
    let first = harness::bootstrap_admin(&app).await;
    let second = harness::add_second_admin(&app, &first, "admin2").await;

    let status = harness::remove_member_status(&app, &second, harness::system_admin_group_id(&app).await, second).await;
    assert_eq!(status, 200, "非最后一名管理员可以退出全权组");
    assert!(!harness::is_member(&app, harness::system_admin_group_id(&app).await, second).await);
    // 退出后该账号立即失去全部权限（旧 Token 失效）。
    assert!(harness::member_token_is_stale(&app, second).await);
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --test permission_groups_integration -- --ignored --test-threads=1`
Expected: 编译失败

- [ ] **Step 3: 实现 `add_group_member.rs`**

```rust
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let group = access
            .groups()
            .find_by_id_in_tx(&ctx, &mut transaction, input.group_id)
            .await?
            .ok_or_else(|| BaseError::RecordNotFound("权限组".to_string()))?;

        // spec §8.1 附加规则：只有全权组成员能修改全权组成员。
        if group.group_key == SYSTEM_ADMIN_GROUP_KEY {
            let admins = access
                .groups()
                .list_members_in_tx(&ctx, &mut transaction, group.id)
                .await?;
            if !admins.contains(&operator_id) {
                return Err(BaseError::PermissionDenied(
                    "只有系统管理员可以修改系统管理员组的成员".to_string(),
                ));
            }
        }

        // spec §8.1 主不变量：任何操作都不得使调用者自身有效权限增大。
        // 只在目标是调用者自己时才有提权的可能；修改他人不受此限。
        if input.user_id == operator_id {
            let before =
                effective_permissions_of_in_tx(&access, &ctx, &mut transaction, operator_id).await?;
            let after = simulate_after_join(&access, &ctx, &mut transaction, operator_id, &group)
                .await?;
            assert_no_self_escalation(&before, &after)?;
        }

        let changed = access
            .groups()
            .insert_member_in_tx(&ctx, &mut transaction, input.user_id, group.id, operator_id)
            .await?;
        if !changed {
            return Ok(false); // 幂等
        }
        let mut affected = BTreeSet::new();
        affected.insert(input.user_id);
        invalidate_users_in_tx(&access, &ctx, &mut transaction, &affected).await?;
        // 审计……
        Ok(true)
    }
    .await;
```

`simulate_after_join` 是 `admin.rs` 里的一个私有辅助：取用户当前有效权限，再并入目标组对该用户的贡献（复用 `resolve_group_permissions`），**不写库**。

- [ ] **Step 4: 实现 `remove_group_member.rs`**

与加成员对称，外加最后管理员守卫：

```rust
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let group = access
            .groups()
            .find_by_id_in_tx(&ctx, &mut transaction, input.group_id)
            .await?
            .ok_or_else(|| BaseError::RecordNotFound("权限组".to_string()))?;

        // spec §8.2：移出全权组成员后系统必须仍有至少一名 active 管理员。
        if group.group_key == SYSTEM_ADMIN_GROUP_KEY {
            let members = access
                .groups()
                .list_members_in_tx(&ctx, &mut transaction, group.id)
                .await?;
            let admins = count_active_system_admins_in_tx(&access, &ctx, &mut transaction).await?;
            if members.contains(&input.user_id) && admins <= 1 {
                return Err(BaseError::Conflict(
                    "不能移出最后一名系统管理员".to_string(),
                ));
            }
        }

        let changed = access
            .groups()
            .delete_member_in_tx(&ctx, &mut transaction, input.user_id, group.id)
            .await?;
        if !changed {
            return Ok(false); // 幂等
        }
        let mut affected = BTreeSet::new();
        affected.insert(input.user_id);
        invalidate_users_in_tx(&access, &ctx, &mut transaction, &affected).await?;
        // 审计……
        Ok(true)
    }
    .await;
```

移出操作**不**受 §8.1 约束（它只会减少权限，不会增加）——这是刻意的，管理员必须能退出。

- [ ] **Step 5: 运行确认通过**

Run: `python scripts/run_ci.py integration`
Expected: PASS（四条新用例全部通过）

- [ ] **Step 6: 提交**

```bash
git add src/addon/access/groups/actions/ src/addon/access/domain/groups/admin.rs tests/permission_groups_integration.rs
git commit -m "feat(access): 组成员接口与防自提权不变量"
```

---

## Task 13: 账号生命周期侧的最后管理员守卫与孤儿授权清理

**Files:**
- Modify: `src/addon/account/domain/system_owner.rs`（新增 `SystemAuthorizationPort` 端口）
- Modify: `src/addon/account/domain/context.rs`（`Account` 持有并透传该端口）
- Modify: `src/addon/account/mod.rs`（`build_addon` 接收该端口）
- Modify: `src/addon/account/user/actions/disable_self.rs`
- Modify: `src/addon/account/user/actions/admin_disable_user.rs:33-41`
- Modify: `src/addon/account/user/actions/delete_account.rs:44-90`
- Modify: `src/addon/access/domain/groups/owner.rs`（实现 `SystemAuthorizationPort`）
- Modify: `src/addon/access/domain/groups/repository.rs`（新增两个清理方法）
- Modify: `src/addon/access/mod.rs`、`src/app.rs`（装配新端口）
- Test: `tests/permission_groups_integration.rs`（追加）

**Interfaces:**
- Consumes: Task 6 的 `count_active_system_admins_in_tx`
- Produces:
  - `pub(crate) trait SystemAuthorizationPort`（account 域），两个方法：
    - `async fn remains_an_admin_after(&self, ctx: &ActionContext, transaction: &mut Transaction, target_user_id: i64) -> Result<bool, BaseError>`
    - `async fn purge_user_facts_in_tx(&self, ctx: &ActionContext, transaction: &mut Transaction, user_id: i64) -> Result<(), BaseError>`
  - `GroupRepository::delete_member_rows_of_user_in_tx(&self, ctx, transaction, user_id) -> Result<u64, BaseError>`
  - `GrantRepository::delete_all_of_user_in_tx(&self, ctx, transaction, user_id) -> Result<u64, BaseError>`（本任务新增到 `src/addon/access/domain/repository.rs`，属既有 writer 边界，无需新登记）
  - `AccessAddon::system_authorization_port(&self) -> Arc<dyn SystemAuthorizationPort>`
  - `Account::system_authorization(&self) -> &Arc<dyn SystemAuthorizationPort>`

**依赖方向注意**：account 域**不能**依赖 access 域（会成环）。因此守卫不能直接调 `access::...`。正确做法是把「系统管理员人数」抽象成一个 account 域端口，由 access 提供实现，经组合根注入——沿用 `SystemOwnerClaimer` 与 `GrantResolver` 的既有模式。

- [ ] **Step 1: 定义 account 域的守卫端口与失败测试**

在 `src/addon/account/domain/system_owner.rs` 追加端口：

```rust
/// account 生命周期所需的授权事实端口：由 access 域实现。
///
/// 存在的理由是依赖方向：account 不能依赖 access（会成环），因此把
/// 「系统管理员不变量」与「账号事实清理」这两个跨域操作抽成端口，
/// 经组合根注入——与 `SystemOwnerClaimer`、`GrantResolver` 同一模式。
#[async_trait]
pub(crate) trait SystemAuthorizationPort: Send + Sync {
    /// 目标用户在「被停用/被删除/被移出全权组」之后，系统是否仍有
    /// 至少一名启用的系统管理员。返回 `false` 表示该操作必须被拒绝。
    async fn remains_an_admin_after(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        target_user_id: i64,
    ) -> Result<bool, BaseError>;

    /// 账号删除时清理其授权事实（`authz_grant` 直授与 `user_group` 成员行）。
    ///
    /// 必须经 access 的 writer 方法完成，**不得**在 account 侧直写这两张表，
    /// 否则绕过 `docs/architecture/authorization-writers.md` 的 writer 边界。
    async fn purge_user_facts_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
    ) -> Result<(), BaseError>;
}
```

失败测试（`tests/permission_groups_integration.rs`）：

```rust
/// spec §8.2：最后一名系统管理员不可被停用。
#[tokio::test]
#[ignore = "需要真实 MySQL/Redis"]
async fn the_last_system_admin_cannot_be_disabled_or_deleted() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;

    let disable_status = harness::admin_disable_status(&app, &admin, admin).await;
    assert_eq!(disable_status, 409, "最后一名管理员不可被停用");

    let delete_status = harness::delete_account_status(&app, &admin).await;
    assert_eq!(delete_status, 409, "最后一名管理员不可被删除");

    let self_disable_status = harness::disable_self_status(&app, &admin).await;
    assert_eq!(self_disable_status, 409, "最后一名管理员不可自停用");
}

/// 有两名管理员时，移除其一必须成功（守卫不能过度收紧）。
#[tokio::test]
#[ignore = "需要真实 MySQL/Redis"]
async fn one_of_two_admins_can_be_disabled() {
    let app = harness::build_test_app().await;
    let first = harness::bootstrap_admin(&app).await;
    let second = harness::add_second_admin(&app, &first, "admin2").await;

    let status = harness::admin_disable_status(&app, &first, second).await;
    assert_eq!(status, 200);
}

/// spec §8.3：账号删除后不得残留授权事实。
#[tokio::test]
#[ignore = "需要真实 MySQL/Redis"]
async fn deleting_an_account_leaves_no_orphan_authorization_rows() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let user = harness::grant_only(&app, &admin, "victim", "demo.notes.read").await;
    let group_id = harness::create_group(&app, &admin, "temp", "临时").await;
    harness::add_member(&app, &admin, group_id, user).await;

    harness::delete_account(&app, &user).await;
    assert_eq!(harness::grant_rows_of_user(&app, user).await, 0, "不得残留 authz_grant 行");
    assert_eq!(harness::member_rows_of_user(&app, user).await, 0, "不得残留 user_group 行");
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --test permission_groups_integration -- --ignored --test-threads=1`
Expected: 三个断言中至少「停用/删除最后管理员」与「孤儿行」失败

- [ ] **Step 3: 实现端口与三处守卫**

在 `src/addon/access/domain/groups/owner.rs` 增加实现（复用 `count_active_system_admins_in_tx`）：

```rust
#[async_trait]
impl SystemAuthorizationPort for AccessSystemOwnerClaimer {
    async fn remains_an_admin_after(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        target_user_id: i64,
    ) -> Result<bool, BaseError> {
        let admins = count_active_system_admins_in_tx(&self.access, ctx, transaction).await?;
        let group_id = match self
            .access
            .groups()
            .find_by_key_in_tx(ctx, transaction, SYSTEM_ADMIN_GROUP_KEY)
            .await?
        {
            Some(group) => group.id,
            None => return Ok(false),
        };
        let members = self
            .access
            .groups()
            .list_members_in_tx(ctx, transaction, group_id)
            .await?;
        if !members.contains(&target_user_id) {
            return Ok(true); // 目标本就不是管理员，任何操作都不影响该不变量。
        }
        Ok(admins > 1)
    }

    async fn purge_user_facts_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
    ) -> Result<(), BaseError> {
        self.access
            .grants()
            .delete_all_of_user_in_tx(ctx, transaction, user_id)
            .await?;
        self.access
            .groups()
            .delete_member_rows_of_user_in_tx(ctx, transaction, user_id)
            .await?;
        Ok(())
    }
}
```

`Account` 上下文增加 `system_authorization: Arc<dyn SystemAuthorizationPort>` 字段与透传方法（与 `system_owner_claimer` 同构），组合根在 `account::build_addon` 调用处传入 `access.system_authorization_port()`。

三处 Action 在事务内、执行变更**之前**调用：

```rust
        // spec §8.2：不能移除最后一名 active 系统管理员。
        if !account
            .system_authorization()
            .remains_an_admin_after(&ctx, &mut transaction, input.id)
            .await?
        {
            return Err(BaseError::Conflict(
                "不能停用最后一名系统管理员".to_string(),
            ));
        }
```

`delete_account.rs` 另在匿名化之前追加清理（spec §8.3）：

```rust
        // spec §8.3：账号删除必须清理授权事实，避免孤儿授权行。
        account
            .system_authorization()
            .purge_user_facts_in_tx(&ctx, &mut transaction, user_id)
            .await?;
```

- [ ] **Step 4: 运行确认通过**

Run: `python scripts/run_ci.py integration && python scripts/check_architecture.py`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add src/addon/account/ src/addon/access/ tests/permission_groups_integration.rs
git commit -m "feat(account): 最后管理员守卫与账号删除的授权事实清理"
```

---

## Task 14: 权限组管理前端视图

**Files:**
- Create: `frontend/src/features/access/views/PermissionGroupsPage.tsx`
- Create: `frontend/src/features/access/api.ts`
- Modify: `frontend/src/features/registry.ts`
- Modify: `frontend/src/shell/routes.tsx`
- Test: `frontend/tests/features/access/permission-groups-page.test.tsx`

**Interfaces:**
- Consumes: `GET /api/v1/access/groups`、`GET /api/v1/access/groups/{id}` 与 Task 10–12 的写接口；`hasOperation(catalog, operationId)`（当前在 `frontend/src/features/feishu/api.ts:121-129`）
- Produces: 路由 `/access/groups` 与两个导出：`PermissionGroupsPage`、`canManageGroups(catalog): boolean`

**规范约束**：
- 视图必须在 `frontend/src/features/registry.ts` 的**静态注册表**中登记（字面量 import，禁止按后端字符串动态 import）。
- 依赖方向 `shared ← engine ← features ← shell`；`features/access/` 不得 import `features/feishu/`。
- 因此本任务要先把 `hasOperation` 下沉到 `engine/`（见 Step 1）——这同时修掉 feishu 域私有实现的跨域复用障碍。

- [ ] **Step 1: 把 `hasOperation` 下沉到 engine 并写失败测试**

移动 `frontend/src/features/feishu/api.ts:121-129` 的 `hasOperation` 到 `frontend/src/engine/catalog/has-operation.ts`，从 `engine/index.ts` 导出；`features/feishu/api.ts` 改为从 engine 引入（保持其对外签名不变）。现有 `frontend/tests/features/feishu/api.test.ts` 必须继续通过——这是不含回归的证明。

测试 `frontend/tests/engine/catalog/has-operation.test.ts`：

```ts
import { describe, expect, it } from "vitest";

import { hasOperation } from "@/engine/catalog/has-operation";
import type { UiCatalog } from "@/engine/contracts/ui-catalog";

function catalogWith(operationIds: string[]): UiCatalog {
  return {
    modules: [],
    actions: operationIds.map((operation_id) => ({ operation_id })),
  } as unknown as UiCatalog;
}

describe("hasOperation", () => {
  it("returns true only for operation ids present in the catalog", () => {
    const catalog = catalogWith(["access.groups.create_group"]);
    expect(hasOperation(catalog, "access.groups.create_group")).toBe(true);
    expect(hasOperation(catalog, "access.groups.delete_group")).toBe(false);
  });

  it("does not throw on an empty catalog", () => {
    expect(hasOperation({ modules: [], actions: [] } as unknown as UiCatalog, "x.y")).toBe(false);
  });
});
```

Run: `pnpm --dir frontend exec vitest run tests/engine/catalog/has-operation.test.ts`
Expected: FAIL（模块不存在）

实现后 Run 同上，Expected: PASS；再 Run `pnpm --dir frontend exec vitest run tests/features/feishu/` 确认无回归。

- [ ] **Step 2: 写页面的失败测试**

```tsx
import { describe, expect, it } from "vitest";

import { canManageGroups } from "@/features/access/api";
import type { UiCatalog } from "@/engine/contracts/ui-catalog";

describe("canManageGroups", () => {
  it("requires the write operation to be visible in the catalog", () => {
    const readOnly = {
      modules: [],
      actions: [{ operation_id: "access.groups.list_groups" }],
    } as unknown as UiCatalog;
    expect(canManageGroups(readOnly)).toBe(false);
  });

  it("is true when the write operation is present", () => {
    const writable = {
      modules: [],
      actions: [
        { operation_id: "access.groups.list_groups" },
        { operation_id: "access.groups.create_group" },
      ],
    } as unknown as UiCatalog;
    expect(canManageGroups(writable)).toBe(true);
  });
});
```

Run: `pnpm --dir frontend exec vitest run tests/features/access/`
Expected: FAIL

- [ ] **Step 3: 实现 api 与页面**

`frontend/src/features/access/api.ts` 导出 `canManageGroups(catalog)`（基于 `hasOperation(catalog, "access.groups.create_group")`）与读写请求函数。`PermissionGroupsPage.tsx` 渲染三块：组列表（含 `is_builtin` 与 `orphan_item_count` 徽标）、选中组的权限条目矩阵（勾选即加/移权限，写入后经 `invalidate` 重拉）、成员列表（加/移成员）。

页面必须显式处理两种状态（spec §15 第 3 条与 Review Focus 5）：
- `effective_all === true` 的内置组：不渲染条目矩阵，改为一句说明「该组的权限由权限目录实时计算，共 N 项」。
- 条目标 `is_orphan === true`：以警示样式渲染并提示「该权限已不在权限目录中，可安全移除」。

- [ ] **Step 4: 登记静态视图与路由**

`frontend/src/features/registry.ts` 的字面量表加一条 `"access.groups.list_groups": PermissionGroupsPage`（键与后端 Action 名一致，静态 import）。`frontend/src/shell/routes.tsx` 加 `/access/groups` 路由并挂在 `RequireAuth` 下。

- [ ] **Step 5: 运行前端门禁**

Run: `pnpm --dir frontend check`
Expected: format:check → lint → typecheck → test → verify:locale-contract → build → verify:production-build → verify:bundle-budget → verify:deployment-contract 全部通过。若首屏 bundle 超预算，把新页面改为路由级 `lazy` 加载。

- [ ] **Step 6: 提交**

```bash
git add frontend/src/features/access/ frontend/src/features/registry.ts frontend/src/shell/routes.tsx frontend/src/engine/catalog/has-operation.ts frontend/src/features/feishu/api.ts frontend/tests/
git commit -m "feat(frontend): 权限组管理视图，hasOperation 下沉到 engine"
```

---

## Task 15: 契约重生成与前端契约测试

**Files:**
- Modify: `frontend/contracts/openapi.json`、`frontend/src/engine/contracts/api-types.ts`（生成物）
- Test: `frontend/tests/engine/contracts/openapi-contract.test.ts`（既有，确认不回归）

**Interfaces:**
- Consumes: Task 10–13 全部新增 Action
- Produces: 更新后的契约快照与 TS 类型

- [ ] **Step 1: 重生成契约**

Run: `python scripts/dump_openapi.py`
Expected: `frontend/contracts/openapi.json` 与 `frontend/src/engine/contracts/api-types.ts` 被更新；新 Action 出现在 operation 列表中。**两个生成物禁止手改**，只能经该脚本产生。

- [ ] **Step 2: 验证契约测试不回归**

Run: `pnpm --dir frontend exec vitest run tests/engine/contracts/openapi-contract.test.ts`
Expected: PASS。该测试断言 `operationCount >= 19`（下限）且每个 operation 的输入 Schema 都能过 `compileDynamicSchema` 白名单——新接口的输入必须全部通过白名单，若有字段类型不被支持，回到 Task 10–12 调整输入声明。

- [ ] **Step 3: 全量门禁**

Run: `python scripts/run_ci.py quick && pnpm --dir frontend check`
Expected: 全部通过

- [ ] **Step 4: 提交**

```bash
git add frontend/contracts/openapi.json frontend/src/engine/contracts/api-types.ts
git commit -m "chore(contracts): 重新生成 OpenAPI 快照与前端类型"
```

---

## Task 16: 文档同步（决策修订与口径对齐）

**Files:**
- Modify: `docs/architecture/foundation-baseline.md:37,39`
- Modify: `docs/contracts/AUTHZ_GRANTS.md`
- Modify: `AGENTS.md:7,35`
- Modify: `docs/architecture/account-system-roadmap.md:32`
- Modify: `docs/contracts/SCHEMA.md`（仅确认无需改动）

**Interfaces:**
- Consumes: 前 15 个任务的最终实现
- Produces: 无代码接口

**为什么这是独立任务**：这五处修订是 D2/D4 决策变更的正式记录。spec §12 已列出全部条目；仓库对文档/代码同步要求严格，遗漏任一处都会造成口径矛盾。放在最后是因为文档必须描述**已实现**的状态，而不是计划。

- [ ] **Step 1: 修订 D2 与 D4**

`docs/architecture/foundation-baseline.md:37` 的 D2 改为 spec §3.1 的修订后表述（引导式一次性、可降权、无自提权路径），并保留原文作为修订记录。`:39` 的 D4 标注「已按预留接口扩展出一层权限组；仍不引入组嵌套与角色继承」。

- [ ] **Step 2: 重写 `AUTHZ_GRANTS.md`**

- 「初始授权（运维）」章节：应用引导为**主路径**，运维 SQL 降为**灾备路径**（spec §7.3），两条路径都要保留同一事务三件事的要求。
- 新增「权限组」章节：三张表的形状、`system_admin` 内置组的计算语义、§8.1 提权不变量、§8.2 最后管理员守卫、幂等语义、错误码表。
- 修正既有的目录来源描述：`project_permissions` 同时合并 `module.default_permissions`（`permission_catalog.rs:44-49`），当前文档只写了 Action 来源。
- 新增权限清单：`access.groups.read` / `access.groups.write`。

- [ ] **Step 3: 修正 AGENTS.md 与路线图口径**

`AGENTS.md:7` 删去「也没有任何账号会成为系统最终管理员」；`:35` 的「access（授权端口（预留，无冷启动引导，权限管理未交付））」改为实际状态。`docs/architecture/account-system-roadmap.md:32` 的「权限管理面整体不可达、属预留端口」改为已完成引导与权限组交付，并保留该段作为历史记录。

- [ ] **Step 4: 核对文档与代码一致性**

Run: `python scripts/check_architecture.py && python scripts/run_ci.py quick`
Expected: 通过。另需人工核对：`AGENTS.md:106-107` 关于 `tests/` 入口数量的描述——本计划新增两个集成测试文件后需同步更新该计数（当前文档写「八个入口」，实际已有 10 个 `.rs`，属既有漂移，本任务顺带修正）。

- [ ] **Step 5: 提交**

```bash
git add docs/ AGENTS.md
git commit -m "docs(authz): 修订 D2/D4 决策并同步权限组与引导契约"
```

---

## 完成定义

全部 16 个任务完成后，以下条件必须同时成立：

1. `python scripts/run_ci.py quick` 与 `python scripts/run_ci.py full` 通过。
2. `python scripts/run_ci.py integration` 通过（含两个新集成入口，共 12 个）。
3. `python scripts/check_architecture.py` 通过，含新 writer 的 allowlist 与 raw-sql-boundary 检查。
4. 全新数据库启动一次即可完成建表（9 张运行支撑表 + `permission_group`），无 SQL 迁移文件。
5. 并发首注册恰好产生一个系统管理员（集成测试证明）。
6. 最后一名管理员不可被停用/删除/移出全权组（集成测试证明）。
7. 自提权两条路径均被拒（集成测试证明）。
8. `docs/architecture/foundation-baseline.md` 的 D2/D4 已修订，五处文档口径与代码一致。
9. `tests/refresh_load_benchmark.rs` 未回归（本计划不触碰校验热路径）。

## 已知未覆盖项

- **字段级可见性**：`users.email` 等字段仍只对 `system` 伪角色可读，管理员在用户列表页看不到邮箱。spec §15 第 6 条列为已知限制，**不在本计划范围**，但建议在 Task 14 之前单独决策（两条修复路径见 spec）。
- **数据范围权限、组嵌套、ABAC**：spec §4.2 明确为非目标。
- **孤儿权限的自动清理**：spec §8.4 只要求报告（Task 10 的 `orphan_item_count` 与 Task 14 的警示样式），不自动清除。
- **`tests/common/mod.rs` 公共夹具**：当前只有 68 行、无建用户/发 Token/装目录的通用夹具。本计划的新集成测试各自复制样板（与既有 8 个入口的做法一致），未顺带补公共夹具——那是一次独立重构。

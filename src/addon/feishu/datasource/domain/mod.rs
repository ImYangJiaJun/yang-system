//! `feishu.datasource` 的机制层。
//!
//! 这里只放**表声明**：`field_table.rs` 是字段绑定表（`feishu_datasource_field`）
//! 的 Schema 唯一事实来源。它的 module 构造器在 `datasource/mod.rs` 里
//! （`build_field_module`），与主表的 `build_module` 并列。
//!
//! # 为什么绑定表的 Schema 不在 `datasource/table.rs` 旁边
//!
//! 架构门禁（`scripts/check_architecture.py`）规定：module 目录根下只允许
//! `mod.rs` / `table.rs` / `actions/` / `domain/`，**每个模块名只能有一张主表**。
//! 绑定表是本 addon 的第二张表，它的声明必须进 `domain/`。

pub(crate) mod field_table;

//! `feishu.approval` 的机制层：表声明与（后续的）派发机制。
//!
//! 表声明住在这里而不是 module 层，与 `datasource/domain/field_table.rs` 同一条纪律：
//! module 层只保留主表（`table.rs`）与 Action 注册表，其余表归 `domain/`。

pub(crate) mod field_map_table;
pub(crate) mod request_log_table;
pub(crate) mod task_table;

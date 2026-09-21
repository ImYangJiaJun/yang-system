//! 飞书集成 addon 的共享机制。
//!
//! 架构门禁要求机制代码一律住在这里：module 目录只允许
//! `mod.rs` / `table.rs` / `actions` / `domain` 四种条目。

pub(crate) mod crypto;
pub(crate) mod protocol;

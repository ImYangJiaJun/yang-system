//! `feishu.portal` module 的机制代码。
//!
//! 架构门禁（`check_module_layout`）只允许接口目录承载 `mod.rs` / `table.rs` /
//! `actions/` / `domain/`——多出来的表声明与共享校验都进这里。

pub(crate) mod bindings;
pub(crate) mod log_table;

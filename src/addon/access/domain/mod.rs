//! 权限 Addon 的领域机制层。
//!
//! 业务用例流程内联在各 Action 文件；这里只承载共享机制：模块上下文
//! （context）、授权存储持久化边界（repository）、Token 授权快照解析器
//! （resolver）与权限目录投影（permission_catalog）。

pub(crate) mod context;
pub(crate) mod permission_catalog;
pub(crate) mod repository;
pub(crate) mod resolver;

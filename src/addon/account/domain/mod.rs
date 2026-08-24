//! 账号 Addon 的领域机制层。
//!
//! 业务用例流程内联在各 Action 文件；这里只承载共享机制：模块上下文
//! （context）、持久化边界（repository）、授权声明（claims）、安全版本原语
//! （authz_version）、密码重置凭证（password_reset）、领域不变量（policy）、
//! 状态机（status）、授权快照扩展点（grants）、
//! 最终管理员声明端口（system_owner）与邮件投递（email_delivery）。

pub(crate) mod authz_version;
pub(crate) mod claims;
pub(crate) mod context;
pub mod email_delivery;
pub(crate) mod grants;
pub(crate) mod password_reset;
pub(crate) mod policy;
pub(crate) mod repository;
pub(crate) mod status;
pub(crate) mod system_owner;

//! 高权限业务审计事实。
//!
//! 审计事件是独立于 tracing 的数据库事实源；本模块只提供不可变事件契约与
//! Schema 校验。事务内写入在独立改进点 P-05 接入各高风险业务路径。

mod event;
mod schema;

pub use event::{
    AuditActor, AuditEntity, AuditEvent, AuditEventContext, AuditResult, AuditSummary,
};

pub(crate) use schema::validate_schema;

pub const AUDIT_EVENT_TABLE: &str = "audit_event";
pub const AUDIT_EVENT_SCHEMA_VERSION: u16 = 1;
pub const AUDIT_ONLINE_RETENTION_DAYS: u16 = 365;

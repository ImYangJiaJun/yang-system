//! 高权限业务审计事实。
//!
//! 审计事件是独立于 tracing 的数据库事实源；本模块提供不可变事件契约、
//! Schema 校验，以及只能复用业务事务的追加原语。

mod event;
mod repository;
mod schema;

pub use event::{
    AuditActor, AuditEntity, AuditEvent, AuditEventContext, AuditResult, AuditSummary,
};

pub(crate) use repository::{
    append_in_tx, append_independent, entity, succeeded_event, succeeded_system_event, summary,
};
pub(crate) use schema::validate_schema;

pub const AUDIT_EVENT_TABLE: &str = "audit_event";
pub const AUDIT_EVENT_SCHEMA_VERSION: u16 = 1;
pub const AUDIT_ONLINE_RETENTION_DAYS: u16 = 365;

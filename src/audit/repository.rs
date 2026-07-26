//! 审计事件的事务内追加边界。
//!
//! 本模块刻意不提供独立事务或连接池写入口，确保调用方只能把审计事实与业务
//! 变更放进同一个 MySQL 事务。

use super::{AuditActor, AuditEntity, AuditEvent, AuditEventContext, AuditResult, AuditSummary};
use serde_json::Value;
use yang_base::action::ActionContext;
use yang_base::BaseError;
use yang_db::Transaction;

pub(crate) fn entity(kind: &'static str, id: impl ToString) -> Result<AuditEntity, BaseError> {
    AuditEntity::new(kind, id.to_string()).map_err(invalid_event)
}

pub(crate) fn summary<const N: usize>(
    fields: [(&'static str, Value); N],
) -> Result<AuditSummary, BaseError> {
    AuditSummary::try_from_fields(fields).map_err(invalid_event)
}

/// 从 Registry 注入的可信派发目标创建成功事件。
pub(crate) fn succeeded_event(
    ctx: &ActionContext,
    tenant_id: Option<i64>,
    subject: Option<AuditEntity>,
    target: AuditEntity,
    before_summary: Option<AuditSummary>,
    after_summary: Option<AuditSummary>,
) -> Result<AuditEvent, BaseError> {
    let actor = AuditActor::user(ctx.actor()?.user_id()).map_err(invalid_event)?;
    let context =
        AuditEventContext::new(actor, tenant_id, ctx.request_id()).map_err(invalid_event)?;
    let (module, action) = ctx.dispatch_target().ok_or_else(|| {
        BaseError::ConfigError("高权限写入缺少可信 Action 派发目标，拒绝生成审计事件".to_string())
    })?;
    AuditEvent::new(
        context,
        format!("{module}.{action}"),
        subject,
        target,
        AuditResult::Succeeded,
        before_summary,
        after_summary,
    )
    .map_err(invalid_event)
}

/// 在调用方持有的业务事务中追加一条审计事实。
pub(crate) async fn append_in_tx(
    transaction: &mut Transaction,
    event: &AuditEvent,
) -> Result<(), BaseError> {
    let before_summary = event
        .before_summary()
        .map(|summary| serde_json::to_string(&summary.as_json()))
        .transpose()
        .map_err(|error| {
            BaseError::ConfigError(format!("序列化审计 before_summary 失败: {error}"))
        })?;
    let after_summary = event
        .after_summary()
        .map(|summary| serde_json::to_string(&summary.as_json()))
        .transpose()
        .map_err(|error| {
            BaseError::ConfigError(format!("序列化审计 after_summary 失败: {error}"))
        })?;
    let (subject_type, subject_id) = event
        .subject()
        .map(|subject| (Some(subject.kind()), Some(subject.id())))
        .unwrap_or((None, None));
    let executor = transaction.executor().ok_or_else(|| {
        BaseError::from(yang_db::DbError::TransactionError(
            "审计 writer 事务已结束".to_string(),
        ))
    })?;
    let result = sqlx::query(
        "INSERT INTO audit_event \
         (event_id, schema_version, occurred_at, actor_type, actor_id, tenant_id, action, \
          subject_type, subject_id, target_type, target_id, before_summary, after_summary, \
          request_id, result) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(event.event_id())
    .bind(event.schema_version())
    .bind(event.occurred_at())
    .bind(event.context().actor().kind())
    .bind(event.context().actor().id())
    .bind(event.context().tenant_id())
    .bind(event.action())
    .bind(subject_type)
    .bind(subject_id)
    .bind(event.target().kind())
    .bind(event.target().id())
    .bind(before_summary)
    .bind(after_summary)
    .bind(event.context().request_id().to_string())
    .bind(event.result().as_str())
    .execute(executor)
    .await
    .map_err(yang_db::DbError::from)?;
    if result.rows_affected() != 1 {
        return Err(BaseError::Unknown(
            "审计事件追加未精确影响一行，拒绝提交业务事务".to_string(),
        ));
    }
    Ok(())
}

fn invalid_event(error: anyhow::Error) -> BaseError {
    BaseError::ConfigError(format!("构建高权限审计事件失败: {error}"))
}

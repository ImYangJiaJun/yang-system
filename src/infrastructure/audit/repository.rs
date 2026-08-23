//! 审计事件的 append-only 追加边界。
//!
//! 业务成功必须使用调用方事务，与状态变更原子提交；业务尚未开始或已经失败的
//! 拒绝/失败结果可使用独立连接追加。两条路径都不提供 UPDATE/DELETE。

use super::{AuditActor, AuditEntity, AuditEvent, AuditEventContext, AuditResult, AuditSummary};
use serde_json::{json, Value};
use sqlx::MySqlPool;
use yang_base::action::ActionContext;
use yang_base::BaseError;
use yang_db::{table, QueryBuilder, Transaction};

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
    succeeded_event_with_actor(
        ctx,
        actor,
        tenant_id,
        subject,
        target,
        before_summary,
        after_summary,
    )
}

/// 为没有登录用户、但持有一次性凭证的成功写入创建系统 actor 审计事件。
pub(crate) fn succeeded_system_event(
    ctx: &ActionContext,
    actor_id: impl Into<String>,
    tenant_id: Option<i64>,
    subject: Option<AuditEntity>,
    target: AuditEntity,
    before_summary: Option<AuditSummary>,
    after_summary: Option<AuditSummary>,
) -> Result<AuditEvent, BaseError> {
    let actor = AuditActor::system(actor_id).map_err(invalid_event)?;
    succeeded_event_with_actor(
        ctx,
        actor,
        tenant_id,
        subject,
        target,
        before_summary,
        after_summary,
    )
}

fn succeeded_event_with_actor(
    ctx: &ActionContext,
    actor: AuditActor,
    tenant_id: Option<i64>,
    subject: Option<AuditEntity>,
    target: AuditEntity,
    before_summary: Option<AuditSummary>,
    after_summary: Option<AuditSummary>,
) -> Result<AuditEvent, BaseError> {
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

/// 组装审计事件的全部持久化列；NULL 值以 JSON null 表达，绑定为 SQL NULL。
fn insert_data(event: &AuditEvent) -> Result<Value, BaseError> {
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
    Ok(json!({
        "event_id": event.event_id(),
        "schema_version": event.schema_version(),
        "occurred_at": event.occurred_at(),
        "actor_type": event.context().actor().kind(),
        "actor_id": event.context().actor().id(),
        "tenant_id": event.context().tenant_id(),
        "action": event.action(),
        "subject_type": subject_type,
        "subject_id": subject_id,
        "target_type": event.target().kind(),
        "target_id": event.target().id(),
        "before_summary": before_summary,
        "after_summary": after_summary,
        "request_id": event.context().request_id().to_string(),
        "result": event.result().as_str(),
    }))
}

/// 在调用方持有的业务事务中追加一条审计事实。
pub(crate) async fn append_in_tx(
    transaction: &mut Transaction,
    event: &AuditEvent,
) -> Result<(), BaseError> {
    // 单行 INSERT 成功即恰好影响一行；失败以错误返回，不再单独核对 rows_affected。
    transaction
        .table(table!("audit_event"))
        .insert(&insert_data(event)?)
        .await?;
    Ok(())
}

/// 使用连接池自动提交一条与业务事务解耦的审计事实。
///
/// 仅用于已被拒绝或已失败、因而没有可提交业务事务的安全事件。调用方不得用它
/// 替代成功业务写入的 `append_in_tx`；Step-up proof 接受事件是业务执行前的独立
/// 安全决策，必须先成功落库才能继续执行受保护 Action。
pub(crate) async fn append_independent(
    pool: &MySqlPool,
    event: &AuditEvent,
) -> Result<(), BaseError> {
    QueryBuilder::from_pool(pool, table!("audit_event"))
        .insert(&insert_data(event)?)
        .await?;
    Ok(())
}

fn invalid_event(error: anyhow::Error) -> BaseError {
    BaseError::ConfigError(format!("构建高权限审计事件失败: {error}"))
}

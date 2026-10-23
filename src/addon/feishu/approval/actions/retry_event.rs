//! 人工重试失败终态事件（前端控制台）。
//!
//! worker 对连续失败达 [`MAX_EVENT_RETRY_COUNT`] 的事件置终态并丢弃，避免毒事件
//! 死循环。但「丢弃」不能是永久黑洞：本 Action 把终态行重置回待处理队列
//! （`processed_at=NULL`、`retry_count=0`、`last_error=NULL`），下一轮 worker
//! 即可重新认领。
//!
//! # 只允许重试失败终态
//!
//! 成功处理的事件同样 `processed_at` 非空，但 `retry_count < 上限`。重放它会
//! 重复回写多维表格，因此必须 400 拒绝。判定统一走
//! [`is_terminal_event`]，与查询侧的 `terminal_only` 同源。
//!
//! # 并发安全
//!
//! 读事件与更新之间可能被并发重试/worker 推进插入。更新条件同时带
//! `id + processed_at IS NOT NULL + retry_count >= 上限`，命中 0 行即视为状态已
//! 变化并返回冲突错误——绝不能用无条件 UPDATE 覆盖成功事件。

use std::sync::Arc;

use serde_json::json;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, Key, ModuleSpec};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::approval::domain::event_table::{
    is_terminal_event, MAX_EVENT_RETRY_COUNT,
};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::infrastructure::audit;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) RetryEventInput {
        #[param(source = path)]
        id: Key::new()
            .title("事件 ID")
            .require(true),
    }
}

/// 注册人工重试端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("retry_event"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&context))
        })
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/events/{id}/retry",
        )
        .display_name("重试失败终态事件")
        .description("把连续失败达到上限的审批事件重置回待处理队列，下一轮 worker 重新认领")
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: RetryEventInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    if input.id <= 0 {
        return Err(BaseError::ParamInvalid(
            "id".to_string(),
            "事件 ID 必须是正整数".to_string(),
        ));
    }

    let row = context
        .approval_events()
        .query()
        .select_fields(&["id", "processed_at", "retry_count"])?
        .where_eq("id", json!(input.id))?
        .optional()
        .await?
        .ok_or_else(|| BaseError::RecordNotFound("审批事件不存在".to_string()))?;
    let processed_at: Option<i64> = row.optional("processed_at")?;
    let previous_retry_count: i64 = row.optional("retry_count")?.unwrap_or(0);

    if processed_at.is_none() {
        return Err(BaseError::ParamInvalid(
            "id".to_string(),
            "该事件仍在待处理队列中，无需重试".to_string(),
        ));
    }
    if !is_terminal_event(processed_at, previous_retry_count) {
        // 成功事件（含幂等跳过）不能重放：会重复回写多维表格。
        return Err(BaseError::ParamInvalid(
            "id".to_string(),
            "该事件已成功处理，重放会重复回写，拒绝重试".to_string(),
        ));
    }

    // 状态变更与审计同一事务：审计写失败必须整体回滚。
    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<serde_json::Value, BaseError> = async {
        let mut record = Record::new();
        record.insert("processed_at", serde_json::Value::Null);
        record.insert("retry_count", json!(0));
        record.insert("last_error", serde_json::Value::Null);
        let affected = context
            .approval_events()
            .query()
            .where_eq("id", json!(input.id))?
            .where_not_null("processed_at")?
            .where_gte("retry_count", json!(MAX_EVENT_RETRY_COUNT))?
            .update_in_tx(&mut transaction, record)
            .await?;
        if affected == 0 {
            // 读到终态与更新之间状态被改掉了（并发重试已回队 / 成功事件被改写）。
            // 框架没有 Conflict 变体，沿用 ParamInvalid 表示资源状态冲突（同
            // access 的组删除/成员守卫取舍）。
            return Err(BaseError::ParamInvalid(
                "id".to_string(),
                "事件状态已变化，请刷新后重试".to_string(),
            ));
        }

        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_event", input.id)?,
            Some(audit::summary([
                ("retry_count", json!(previous_retry_count)),
                ("processed_at", json!(processed_at)),
            ])?),
            Some(audit::summary([
                ("retry_count", json!(0)),
                ("processed_at", serde_json::Value::Null),
                ("requeued", json!(true)),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;

        Ok(json!({
            "event_id": input.id,
            "requeued": true,
            "previous_retry_count": previous_retry_count,
        }))
    }
    .await;
    let value = FeishuContext::finish_transaction(transaction, result).await?;

    Ok(ApiResponse::success_value(value, "事件已重新入队"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_requires_path_event_id() {
        let params = <RetryEventInput as ParamInput>::params();
        let id = params
            .as_slice()
            .iter()
            .find(|param| param.name.as_str() == "id")
            .unwrap_or_else(|| panic!("应声明 id 参数"));
        assert_eq!(id.source, yang_base::definition::ParamSource::Path);
        assert!(id.required);
    }

    #[tokio::test]
    async fn declares_write_permission_and_route() {
        let module = super::super::tests::registered_module(false);
        let action = module
            .actions()
            .iter()
            .find(|spec| spec.route.path == "/api/v1/feishu/approval/events/{id}/retry")
            .unwrap_or_else(|| panic!("人工重试路由必须注册"));
        assert_eq!(
            action.permissions,
            vec!["feishu.approval.write".to_string()]
        );
    }

    #[test]
    fn successful_event_is_not_retryable() {
        // 与 list_events 的 terminal_only 同一条判据：processed_at 非空但重试
        // 未达上限的成功事件必须被拒（否则重放会重复回写）。
        assert!(!is_terminal_event(
            Some(1_700_000_000),
            MAX_EVENT_RETRY_COUNT - 1
        ));
        assert!(is_terminal_event(
            Some(1_700_000_000),
            MAX_EVENT_RETRY_COUNT
        ));
    }
}

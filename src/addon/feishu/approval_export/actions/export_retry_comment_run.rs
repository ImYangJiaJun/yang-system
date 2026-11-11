//! 人工重试一条审批评论运行记录。
//!
//! 只允许 `failed` → `retry`：评论创建成功但状态列写失败的行保持 `commented`，
//! 由 worker 自动补写；`commenting` 可能在飞行中；`done` 已完成。把其它状态
//! 一律拒绝，避免「点一下重试」掩盖真实问题。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;
use yang_db::{field, table, CompareOp, QueryBuilder};

use crate::addon::feishu::domain::approval_provision::now_unix_secs;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::infrastructure::audit;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct RetryCommentRunInput {
    pub(super) run_id: i64,
}

impl ParamInput for RetryCommentRunInput {
    fn params() -> Params {
        Params::new()
    }
}

impl RetryCommentRunInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.run_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "run_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        Ok(())
    }
}

/// 注册人工重试端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("export_retry_comment_run"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/export/comment/runs/retry",
        )
        .display_name("重试审批评论回写")
        .description("把 failed 的评论运行记录重新排入队列")
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: RetryCommentRunInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<serde_json::Value, BaseError> = async {
        let row = crate::db::select_for_update::<Record>(
            &mut transaction,
            QueryBuilder::from_pool(
                context.approval_comment_runs().pool(),
                table!("feishu_approval_comment_run"),
            )
            .fields(&[field!("id"), field!("rule_id"), field!("state")])
            .where_and(field!("id"), CompareOp::Eq, input.run_id)
            .map_err(BaseError::from)?,
        )
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| BaseError::RecordNotFound("评论运行记录不存在".to_string()))?;

        let previous: String = row.require("state")?;
        if previous != "failed" {
            return Err(BaseError::ParamInvalid(
                "run_id".to_string(),
                format!(
                    "当前状态「{previous}」不可重试：只支持 failed；commented 会自动补状态，\
                     done 已完成，commenting 可能正在处理"
                ),
            ));
        }

        let mut update = Record::new();
        update.insert("state", serde_json::json!("retry"));
        update.insert("attempts", serde_json::json!(0));
        update.insert("next_attempt_at", serde_json::json!(now_unix_secs()));
        update.insert("last_error", serde_json::Value::Null);
        context
            .approval_comment_runs()
            .query()
            .where_eq("id", serde_json::json!(input.run_id))?
            .update_in_tx(&mut transaction, update)
            .await?;

        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_approval_comment_run", input.run_id)?,
            Some(audit::summary([("state", serde_json::json!(&previous))])?),
            Some(audit::summary([
                ("run_id", serde_json::json!(input.run_id)),
                ("state", serde_json::json!("retry")),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;

        Ok(serde_json::json!({
            "run_id": input.run_id,
            "previous_state": previous,
            "state": "retry",
        }))
    }
    .await;
    let value = FeishuContext::finish_transaction(transaction, result).await?;
    ApiResponse::success(value, "已重新排队")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_positive_ids_are_accepted() {
        assert!(RetryCommentRunInput { run_id: 0 }.validate().is_err());
        assert!(RetryCommentRunInput { run_id: 3 }.validate().is_ok());
    }
}

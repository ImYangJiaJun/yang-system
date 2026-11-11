//! 取消批量审批 job：只停止尚未开始的任务项。

use std::sync::Arc;

use serde_json::{json, Value};

use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::table::Record;
use yang_base::BaseError;

use super::super::super::domain::batch_job_item_table::{
    ITEM_FINISHED_AT, ITEM_JOB_ID, ITEM_STATUS,
};
use super::super::super::domain::batch_job_table::{
    JOB_FINISHED_AT, JOB_ID, JOB_STATUS, JOB_USER_ID,
};
use crate::addon::feishu::domain::context::FeishuContext;

yang_base::params! {
    pub(super) CancelJobInput {
        #[param(source = path)]
        id: Str::new().require(true),
    }
}

pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("feishu_batch_cancel_job"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/batch-approval/jobs/{id}/cancel",
        )
        .display_name("取消批量审批任务")
        .description("取消尚未开始的批量审批任务项")
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: CancelJobInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let user_id = crate::addon::feishu::sso::require_current_user(&ctx)?;
    let job_id: i64 = input
        .id
        .parse()
        .map_err(|_| BaseError::ParamInvalid("id".to_string(), "任务 ID 非法".to_string()))?;
    let rows = context
        .batch_jobs()
        .query()
        .select_fields(&[JOB_STATUS])?
        .where_eq(JOB_ID, Value::from(job_id))?
        .where_eq(JOB_USER_ID, Value::from(user_id))?
        .page(1, 1)?
        .all()
        .await?;
    let Some(job) = rows.first() else {
        return Ok(ApiResponse::fail(40401, "批量审批任务不存在"));
    };
    let status: String = job.require(JOB_STATUS)?;
    if !matches!(status.as_str(), "pending" | "running") {
        return ApiResponse::success(
            serde_json::json!({ "cancelled": false, "status": status }),
            "任务已结束，无需取消",
        );
    }
    let now = crate::addon::feishu::domain::user_token::now_unix_secs();
    context
        .batch_jobs()
        .query()
        .where_eq(JOB_ID, Value::from(job_id))?
        .where_eq(JOB_USER_ID, Value::from(user_id))?
        .update(
            Record::new()
                .set(JOB_STATUS, "cancelled")
                .set(JOB_FINISHED_AT, now),
        )
        .await?;
    let event = crate::infrastructure::audit::succeeded_event(
        &ctx,
        None,
        Some(crate::infrastructure::audit::entity("user", user_id)?),
        crate::infrastructure::audit::entity("feishu_batch_job", job_id)?,
        Some(crate::infrastructure::audit::summary([(
            "status",
            json!(status),
        )])?),
        Some(crate::infrastructure::audit::summary([(
            "status",
            json!("cancelled"),
        )])?),
    )?;
    crate::infrastructure::audit::append_independent(ctx.tools().mysql()?.pool(), &event).await?;
    context
        .batch_job_items()
        .query()
        .where_eq(ITEM_JOB_ID, Value::from(job_id))?
        .where_eq("status", Value::String("pending".to_string()))?
        .update(
            Record::new()
                .set(ITEM_STATUS, "skipped")
                .set(ITEM_FINISHED_AT, now),
        )
        .await?;
    ApiResponse::success(
        serde_json::json!({ "cancelled": true, "status": "cancelled" }),
        "已取消",
    )
}

/// 解绑/删除账号前取消该用户所有尚未结束的 job，避免凭证被删后 worker 继续失败。
pub(crate) async fn cancel_user_jobs(
    context: &FeishuContext,
    user_id: i64,
) -> Result<u64, BaseError> {
    let rows = context
        .batch_jobs()
        .query()
        .select_fields(&[JOB_ID])?
        .where_eq(JOB_USER_ID, Value::from(user_id))?
        .where_in(
            "status",
            vec![
                Value::String("pending".to_string()),
                Value::String("running".to_string()),
            ],
        )?
        .page(1, 100)?
        .all()
        .await?;
    let ids: Vec<Value> = rows
        .iter()
        .filter_map(|row| row.optional::<i64>(JOB_ID).ok().flatten())
        .map(Value::from)
        .collect();
    if ids.is_empty() {
        return Ok(0);
    }
    let now = crate::addon::feishu::domain::user_token::now_unix_secs();
    let affected = context
        .batch_jobs()
        .query()
        .where_in(JOB_ID, ids.clone())?
        .where_eq(JOB_USER_ID, Value::from(user_id))?
        .where_in(
            "status",
            vec![
                Value::String("pending".to_string()),
                Value::String("running".to_string()),
            ],
        )?
        .update(
            Record::new()
                .set(JOB_STATUS, "cancelled")
                .set(JOB_FINISHED_AT, now),
        )
        .await?;
    context
        .batch_job_items()
        .query()
        .where_in(ITEM_JOB_ID, ids)?
        .where_eq("status", Value::String("pending".to_string()))?
        .update(
            Record::new()
                .set(ITEM_STATUS, "skipped")
                .set(ITEM_FINISHED_AT, now),
        )
        .await?;
    Ok(affected)
}

//! 查询批量审批 job 与逐条结果。

use std::sync::Arc;

use serde_json::Value;

use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::table::Record;
use yang_base::BaseError;

use super::super::super::domain::batch_job_item_table::{
    ITEM_AFTER_STATUS, ITEM_ATTEMPTS, ITEM_BEFORE_STATUS, ITEM_DEFINITION_CODE,
    ITEM_DEFINITION_NAME, ITEM_FINISHED_AT, ITEM_ID, ITEM_INITIATOR_NAME, ITEM_INSTANCE_CODE,
    ITEM_JOB_ID, ITEM_MESSAGE, ITEM_RESULT_CODE, ITEM_STARTED_AT, ITEM_STATUS, ITEM_TASK_ID,
    ITEM_TITLE,
};
use super::super::super::domain::batch_job_table::{
    JOB_ACTION, JOB_ADD_SIGN_OPEN_IDS, JOB_ADD_SIGN_TYPE, JOB_APPROVAL_METHOD, JOB_COMMENT,
    JOB_CREATED_AT, JOB_FAILED, JOB_FINISHED_AT, JOB_ID, JOB_ROLLBACK_MODE, JOB_STATUS,
    JOB_SUCCEEDED, JOB_TOTAL, JOB_TRANSFER_OPEN_ID, JOB_UNKNOWN, JOB_USER_ID,
};
use crate::addon::feishu::domain::context::FeishuContext;

yang_base::params! {
    pub(super) GetJobInput {
        #[param(source = path)]
        id: Str::new().require(true),
    }
}

pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("feishu_batch_get_job"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Get, "/api/v1/feishu/batch-approval/jobs/{id}")
        .display_name("查询批量审批任务")
        .description("返回批量审批 job 的进度与逐条结果")
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: GetJobInput,
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
        .select_fields(&[
            JOB_ID,
            JOB_ACTION,
            JOB_STATUS,
            JOB_TOTAL,
            JOB_SUCCEEDED,
            JOB_FAILED,
            JOB_UNKNOWN,
            JOB_COMMENT,
            JOB_TRANSFER_OPEN_ID,
            JOB_ADD_SIGN_OPEN_IDS,
            JOB_ADD_SIGN_TYPE,
            JOB_APPROVAL_METHOD,
            JOB_ROLLBACK_MODE,
            JOB_CREATED_AT,
            JOB_FINISHED_AT,
        ])?
        .where_eq(JOB_ID, Value::from(job_id))?
        .where_eq(JOB_USER_ID, Value::from(user_id))?
        .page(1, 1)?
        .all()
        .await?;
    let Some(job) = rows.first() else {
        return Ok(ApiResponse::fail(40401, "批量审批任务不存在"));
    };
    let add_sign: Value = job
        .optional::<String>(JOB_ADD_SIGN_OPEN_IDS)?
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null);
    let job_json = serde_json::json!({
        "id": job.require::<i64>(JOB_ID)?,
        "action": job.require::<String>(JOB_ACTION)?,
        "status": job.require::<String>(JOB_STATUS)?,
        "total": job.require::<i64>(JOB_TOTAL)?,
        "succeeded": job.require::<i64>(JOB_SUCCEEDED)?,
        "failed": job.require::<i64>(JOB_FAILED)?,
        "unknown": job.require::<i64>(JOB_UNKNOWN)?,
        "comment": job.optional::<String>(JOB_COMMENT)?,
        "transfer_open_id": job.optional::<String>(JOB_TRANSFER_OPEN_ID)?,
        "add_sign_open_ids": add_sign,
        "add_sign_type": job.optional::<i64>(JOB_ADD_SIGN_TYPE)?,
        "approval_method": job.optional::<i64>(JOB_APPROVAL_METHOD)?,
        "rollback_mode": job.optional::<String>(JOB_ROLLBACK_MODE)?,
        "created_at": job.optional::<i64>(JOB_CREATED_AT)?,
        "finished_at": job.optional::<i64>(JOB_FINISHED_AT)?,
    });
    let items = context
        .batch_job_items()
        .query()
        .select_fields(&[
            ITEM_ID,
            ITEM_TASK_ID,
            ITEM_INSTANCE_CODE,
            ITEM_DEFINITION_CODE,
            ITEM_DEFINITION_NAME,
            ITEM_TITLE,
            ITEM_INITIATOR_NAME,
            ITEM_STATUS,
            ITEM_ATTEMPTS,
            ITEM_RESULT_CODE,
            ITEM_MESSAGE,
            ITEM_BEFORE_STATUS,
            ITEM_AFTER_STATUS,
            ITEM_STARTED_AT,
            ITEM_FINISHED_AT,
        ])?
        .where_eq(ITEM_JOB_ID, Value::from(job_id))?
        .page(1, 100)?
        .all()
        .await?;
    let items: Vec<Value> = items.iter().map(item_json).collect::<Result<_, _>>()?;
    // 前端契约是「job 字段平铺 + items 数组」；保持与 `create`/`list` 的扁平形状一致。
    let mut payload = job_json;
    if let Value::Object(ref mut map) = payload {
        map.insert("items".to_string(), Value::Array(items));
    }
    ApiResponse::success(payload, "ok")
}

fn item_json(record: &Record) -> Result<Value, BaseError> {
    Ok(serde_json::json!({
        "id": record.require::<i64>(ITEM_ID)?,
        "task_id": record.require::<String>(ITEM_TASK_ID)?,
        "instance_code": record.require::<String>(ITEM_INSTANCE_CODE)?,
        "definition_code": record.optional::<String>(ITEM_DEFINITION_CODE)?,
        "definition_name": record.optional::<String>(ITEM_DEFINITION_NAME)?,
        "title": record.optional::<String>(ITEM_TITLE)?,
        "initiator_name": record.optional::<String>(ITEM_INITIATOR_NAME)?,
        "status": record.require::<String>(ITEM_STATUS)?,
        "attempts": record.require::<i64>(ITEM_ATTEMPTS)?,
        "result_code": record.optional::<i64>(ITEM_RESULT_CODE)?,
        "message": record.optional::<String>(ITEM_MESSAGE)?,
        "before_status": record.optional::<String>(ITEM_BEFORE_STATUS)?,
        "after_status": record.optional::<String>(ITEM_AFTER_STATUS)?,
        "started_at": record.optional::<i64>(ITEM_STARTED_AT)?,
        "finished_at": record.optional::<i64>(ITEM_FINISHED_AT)?,
    }))
}

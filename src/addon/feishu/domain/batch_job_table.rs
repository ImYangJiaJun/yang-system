//! `feishu_batch_job`：一次批量审批请求（一个用户、一个动作、一组任务）。

use yang_base::definition::{FieldName, FieldRef, Int, Key, Str, TableName, TableSpec, Timestamp};
use yang_base::BaseError;

pub(crate) const JOB_ID: &str = "id";
pub(crate) const JOB_USER_ID: &str = "user_id";
pub(crate) const JOB_IDENTITY_ID: &str = "identity_id";
pub(crate) const JOB_ACTION: &str = "action";
pub(crate) const JOB_STATUS: &str = "status";
pub(crate) const JOB_IDEMPOTENCY_KEY: &str = "idempotency_key";
pub(crate) const JOB_TOTAL: &str = "total";
pub(crate) const JOB_SUCCEEDED: &str = "succeeded";
pub(crate) const JOB_FAILED: &str = "failed";
pub(crate) const JOB_UNKNOWN: &str = "unknown";
pub(crate) const JOB_COMMENT: &str = "comment";
pub(crate) const JOB_TRANSFER_OPEN_ID: &str = "transfer_open_id";
pub(crate) const JOB_ADD_SIGN_OPEN_IDS: &str = "add_sign_open_ids";
pub(crate) const JOB_ADD_SIGN_TYPE: &str = "add_sign_type";
pub(crate) const JOB_APPROVAL_METHOD: &str = "approval_method";
pub(crate) const JOB_ROLLBACK_MODE: &str = "rollback_mode";
pub(crate) const JOB_FINISHED_AT: &str = "finished_at";
pub(crate) const JOB_CREATED_AT: &str = "created_at";

pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    let fields = yang_base::fields! {
        id => Key::new().title("ID").filterable(true),
        user_id => Int::new().title("发起人用户 ID").require(true).filterable(true),
        identity_id => Int::new().title("飞书身份 ID").require(true).filterable(true),
        action => Str::new().title("动作").require(true).max_length(16).filterable(true),
        status => Str::new()
            .title("状态")
            .require(true)
            .max_length(16)
            .default("pending")
            .filterable(true),
        idempotency_key => Str::new()
            .title("幂等键")
            .require(true)
            .max_length(64)
            .filterable(true),
        total => Int::new().title("总任务数").require(true).default(0_i64),
        succeeded => Int::new().title("成功数").require(true).default(0_i64),
        failed => Int::new().title("失败数").require(true).default(0_i64),
        unknown => Int::new().title("未知数").require(true).default(0_i64),
        comment => Str::new().title("审批意见").max_length(500),
        transfer_open_id => Str::new().title("转交目标 Open ID").max_length(64),
        add_sign_open_ids => Str::new().title("加签目标 Open ID JSON").max_length(2048),
        add_sign_type => Int::new().title("加签位置"),
        approval_method => Int::new().title("加签审批方式"),
        rollback_mode => Str::new().title("退回方式").max_length(16),
        finished_at => Timestamp::new().title("完成时间"),
        created_at => Timestamp::new().title("创建时间").created_at(),
        updated_at => Timestamp::new().title("更新时间").updated_at(),
    };
    let table_name = TableName::new("feishu_batch_job")
        .map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(TableSpec::new(table_name.clone())
        .title("飞书批量审批任务")
        .fields(fields)
        .unique_named(
            "uk_feishu_batch_job_idempotency",
            [
                field_ref(&table_name, JOB_USER_ID)?,
                field_ref(&table_name, JOB_IDEMPOTENCY_KEY)?,
            ],
        )
        .check_named(
            "chk_feishu_batch_job_action",
            "`action` IN ('pass', 'refuse', 'forward', 'rollback', 'add_sign')",
        )
        .check_named(
            "chk_feishu_batch_job_status",
            "`status` IN ('pending', 'running', 'succeeded', 'partial', 'failed', 'cancelled')",
        ))
}

fn field_ref(table_name: &TableName, field: &str) -> Result<FieldRef, BaseError> {
    let field = FieldName::new(field).map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(FieldRef::new(table_name.clone(), field))
}

//! `feishu_batch_job_item`：批量 job 的逐条任务与结果快照。

use yang_base::definition::{FieldName, FieldRef, Int, Key, Str, TableName, TableSpec, Timestamp};
use yang_base::BaseError;

pub(crate) const ITEM_ID: &str = "id";
pub(crate) const ITEM_JOB_ID: &str = "job_id";
pub(crate) const ITEM_TASK_ID: &str = "task_id";
pub(crate) const ITEM_INSTANCE_CODE: &str = "instance_code";
pub(crate) const ITEM_DEFINITION_CODE: &str = "definition_code";
pub(crate) const ITEM_DEFINITION_NAME: &str = "definition_name";
pub(crate) const ITEM_TITLE: &str = "title";
pub(crate) const ITEM_INITIATOR_NAME: &str = "initiator_name";
pub(crate) const ITEM_STATUS: &str = "status";
pub(crate) const ITEM_ATTEMPTS: &str = "attempts";
pub(crate) const ITEM_RESULT_CODE: &str = "result_code";
pub(crate) const ITEM_MESSAGE: &str = "message";
pub(crate) const ITEM_BEFORE_STATUS: &str = "before_status";
pub(crate) const ITEM_AFTER_STATUS: &str = "after_status";
pub(crate) const ITEM_STARTED_AT: &str = "started_at";
pub(crate) const ITEM_FINISHED_AT: &str = "finished_at";

pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    let fields = yang_base::fields! {
        id => Key::new().title("ID").filterable(true),
        job_id => Int::new().title("任务 ID").require(true).filterable(true),
        task_id => Str::new().title("飞书 Task ID").require(true).max_length(64).filterable(true),
        instance_code => Str::new().title("实例 Code").require(true).max_length(128).filterable(true),
        definition_code => Str::new().title("审批定义 Code").max_length(128),
        definition_name => Str::new().title("审批定义名称").max_length(255),
        title => Str::new().title("标题").max_length(512),
        initiator_name => Str::new().title("发起人").max_length(128),
        status => Str::new()
            .title("状态")
            .require(true)
            .max_length(16)
            .default("pending")
            .filterable(true),
        attempts => Int::new().title("尝试次数").require(true).default(0_i64),
        result_code => Int::new().title("飞书业务码"),
        message => Str::new().title("结果说明").max_length(1024),
        before_status => Str::new().title("写入前状态").max_length(16),
        after_status => Str::new().title("写入后状态").max_length(16),
        started_at => Timestamp::new().title("开始时间"),
        finished_at => Timestamp::new().title("完成时间"),
        created_at => Timestamp::new().title("创建时间").created_at(),
        updated_at => Timestamp::new().title("更新时间").updated_at(),
    };
    let table_name = TableName::new("feishu_batch_job_item")
        .map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(TableSpec::new(table_name.clone())
        .title("飞书批量审批任务项")
        .fields(fields)
        .unique_named(
            "uk_feishu_batch_job_item_task",
            [
                field_ref(&table_name, ITEM_JOB_ID)?,
                field_ref(&table_name, ITEM_TASK_ID)?,
            ],
        )
        .check_named(
            "chk_feishu_batch_job_item_status",
            "`status` IN ('pending', 'running', 'succeeded', 'failed', 'unknown', 'skipped')",
        ))
}

fn field_ref(table_name: &TableName, field: &str) -> Result<FieldRef, BaseError> {
    let field = FieldName::new(field).map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(FieldRef::new(table_name.clone(), field))
}

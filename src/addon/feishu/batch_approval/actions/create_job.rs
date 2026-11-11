//! 创建批量审批 job：服务端重新校验待办后落 job + items，并异步处理。

use std::collections::BTreeSet;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;

use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;

use super::super::api;
use super::access_token_or_error;
use crate::addon::feishu::domain::batch_job_item_table::{
    ITEM_ATTEMPTS, ITEM_DEFINITION_CODE, ITEM_DEFINITION_NAME, ITEM_INITIATOR_NAME,
    ITEM_INSTANCE_CODE, ITEM_JOB_ID, ITEM_STATUS, ITEM_TASK_ID, ITEM_TITLE,
};
use crate::addon::feishu::domain::batch_job_table::{
    JOB_ACTION, JOB_ADD_SIGN_OPEN_IDS, JOB_ADD_SIGN_TYPE, JOB_APPROVAL_METHOD, JOB_COMMENT,
    JOB_FAILED, JOB_ID, JOB_IDEMPOTENCY_KEY, JOB_IDENTITY_ID, JOB_ROLLBACK_MODE, JOB_STATUS,
    JOB_SUCCEEDED, JOB_TOTAL, JOB_TRANSFER_OPEN_ID, JOB_UNKNOWN, JOB_USER_ID,
};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::infrastructure::audit;

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct JobItemInput {
    task_id: String,
    instance_code: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateJobInput {
    action: String,
    items: Vec<JobItemInput>,
    #[serde(default)]
    comment: Option<String>,
    #[serde(default)]
    transfer_open_id: Option<String>,
    #[serde(default)]
    add_sign_open_ids: Vec<String>,
    #[serde(default)]
    add_sign_type: Option<i64>,
    #[serde(default)]
    approval_method: Option<i64>,
    #[serde(default)]
    rollback_mode: Option<String>,
    idempotency_key: String,
}

impl ParamInput for CreateJobInput {
    fn params() -> Params {
        Params::new()
    }
}

pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    // auth: authenticated-only 自服务操作——登录用户只能访问自己的飞书身份与批量审批数据
    module
        .action_fn(
            yang_base::action_name!("feishu_batch_create_job"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/batch-approval/jobs")
        .display_name("创建批量审批任务")
        .description("校验待办并创建异步批量审批 job")
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: CreateJobInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let user_id = crate::addon::feishu::sso::require_current_user(&ctx)?;
    let Some(settings) = context.settings() else {
        return Ok(ApiResponse::fail(50301, "飞书集成未配置"));
    };
    validate_input(&input, settings.batch_max_tasks as usize)?;
    // 幂等键优先于凭证：授权过期/解绑后的重试仍应拿回已创建的 job，而不是 40901。
    if let Some(existing) =
        find_existing_job(&ctx, &context, user_id, &input.idempotency_key).await?
    {
        return ApiResponse::success(serde_json::json!({ "job_id": existing }), "已存在");
    }
    let Some(credential) =
        crate::addon::feishu::sso::credentials::load_credential(&ctx, &context, user_id).await?
    else {
        return Ok(ApiResponse::fail(40901, "尚未绑定飞书身份"));
    };
    let add_sign_targets = validate_targets(&input, &credential.open_id)?;
    let token = match access_token_or_error(&ctx, &context, user_id).await {
        Ok(token) => token,
        Err(response) => return Ok(response),
    };
    let target_ids: Vec<String> = match input.action.as_str() {
        "forward" => vec![input
            .transfer_open_id
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .to_string()],
        "add_sign" => add_sign_targets.clone(),
        _ => Vec::new(),
    };
    validate_target_users(&ctx, settings, &token, &target_ids).await?;
    let list = api::list_pending_tasks(&ctx, settings, &token)
        .await
        .map_err(|error| error.to_base())?;
    let mut fresh = std::collections::HashMap::new();
    for task in &list.tasks {
        fresh.insert(
            (task.task_id.clone(), task.instance_code.clone()),
            task.clone(),
        );
    }
    let mut seen = BTreeSet::new();
    let mut selected = Vec::new();
    for item in &input.items {
        if !seen.insert(item.task_id.clone()) {
            continue;
        }
        let Some(task) = fresh.get(&(item.task_id.clone(), item.instance_code.clone())) else {
            return Err(BaseError::ParamInvalid(
                "items".to_string(),
                "部分任务已不在待办，请刷新后重试".to_string(),
            ));
        };
        if task.support_api_operate == Some(false)
            || task.instance_external_id.is_some()
            || task.task_external_id.is_some()
        {
            return Err(BaseError::ParamInvalid(
                "items".to_string(),
                "部分任务不支持 API 操作（外部审批/禁止 API 操作），请取消勾选".to_string(),
            ));
        }
        if let Some(status) = task.status.as_deref() {
            if status != "PENDING" {
                return Err(BaseError::ParamInvalid(
                    "items".to_string(),
                    "部分任务已不是待审批状态，请刷新后重试".to_string(),
                ));
            }
        }
        selected.push(task.clone());
    }
    let add_sign_json = if add_sign_targets.is_empty() {
        Value::Null
    } else {
        Value::String(
            serde_json::to_string(&add_sign_targets)
                .map_err(|error| BaseError::JsonSerializeFailed(error.to_string()))?,
        )
    };
    let mut transaction = ctx.begin_transaction().await?;
    let result = async {
        let job = Record::new()
            .set(JOB_USER_ID, user_id)
            .set(JOB_IDENTITY_ID, credential.identity_id)
            .set(JOB_ACTION, input.action.as_str())
            .set(JOB_STATUS, "pending")
            .set(JOB_IDEMPOTENCY_KEY, input.idempotency_key.as_str())
            .set(JOB_TOTAL, selected.len() as i64)
            .set(JOB_SUCCEEDED, 0_i64)
            .set(JOB_FAILED, 0_i64)
            .set(JOB_UNKNOWN, 0_i64)
            .set(JOB_COMMENT, option_value(input.comment.as_deref()))
            .set(
                JOB_TRANSFER_OPEN_ID,
                option_value(input.transfer_open_id.as_deref()),
            )
            .set(JOB_ADD_SIGN_OPEN_IDS, add_sign_json)
            .set(
                JOB_ADD_SIGN_TYPE,
                input.add_sign_type.map(Value::from).unwrap_or(Value::Null),
            )
            .set(
                JOB_APPROVAL_METHOD,
                input
                    .approval_method
                    .map(Value::from)
                    .unwrap_or(Value::Null),
            )
            .set(
                JOB_ROLLBACK_MODE,
                option_value(input.rollback_mode.as_deref()),
            );
        let (_, raw_id) = context
            .batch_jobs()
            .query()
            .insert_returning_id_in_tx(&mut transaction, job)
            .await?;
        let job_id = i64::try_from(raw_id)
            .map_err(|_| BaseError::Unknown("批量任务主键超出 i64".to_string()))?;
        for task in &selected {
            let item = Record::new()
                .set(ITEM_JOB_ID, job_id)
                .set(ITEM_TASK_ID, task.task_id.as_str())
                .set(ITEM_INSTANCE_CODE, task.instance_code.as_str())
                .set(
                    ITEM_DEFINITION_CODE,
                    option_value(task.definition_code.as_deref()),
                )
                .set(
                    ITEM_DEFINITION_NAME,
                    option_value(task.definition_name.as_deref()),
                )
                .set(ITEM_TITLE, option_value(task.title.as_deref()))
                .set(
                    ITEM_INITIATOR_NAME,
                    option_value(task.initiator_name.as_deref()),
                )
                .set(ITEM_STATUS, "pending")
                .set(ITEM_ATTEMPTS, 0_i64);
            context
                .batch_job_items()
                .query()
                .insert_in_tx(&mut transaction, item)
                .await?;
        }
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", user_id)?),
            audit::entity("feishu_batch_job", job_id)?,
            None,
            Some(audit::summary([
                ("action", Value::String(input.action.clone())),
                ("total", Value::from(selected.len() as i64)),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(job_id)
    }
    .await;
    let job_id = match result {
        Ok(job_id) => AccountFinish::commit(transaction, job_id).await?,
        Err(error) if crate::db::is_constraint_violation(&error) => {
            let _ = transaction.rollback().await;
            if let Some(existing) =
                find_existing_job(&ctx, &context, user_id, &input.idempotency_key).await?
            {
                return ApiResponse::success(serde_json::json!({ "job_id": existing }), "已存在");
            }
            return Err(error);
        }
        Err(error) => {
            let _ = transaction.rollback().await;
            return Err(error);
        }
    };
    if settings.batch_worker_enabled {
        super::super::worker::spawn(
            ctx.clone(),
            Arc::clone(&context),
            settings.clone(),
            user_id,
            job_id,
        );
    }
    ApiResponse::success(serde_json::json!({ "job_id": job_id }), "已创建")
}

fn validate_input(input: &CreateJobInput, max_tasks: usize) -> Result<(), BaseError> {
    if !matches!(
        input.action.as_str(),
        "pass" | "refuse" | "forward" | "rollback" | "add_sign"
    ) {
        return Err(BaseError::ParamInvalid(
            "action".to_string(),
            "不支持的审批动作".to_string(),
        ));
    }
    if input.items.is_empty() || input.items.len() > max_tasks {
        return Err(BaseError::ParamInvalid(
            "items".to_string(),
            format!("任务数必须在 1..={max_tasks} 之间"),
        ));
    }
    if input.idempotency_key.trim().is_empty() || input.idempotency_key.len() > 64 {
        return Err(BaseError::ParamInvalid(
            "idempotency_key".to_string(),
            "幂等键不能为空且不超过 64 字符".to_string(),
        ));
    }
    if input
        .comment
        .as_deref()
        .is_some_and(|value| value.chars().count() > 500)
    {
        return Err(BaseError::ParamInvalid(
            "comment".to_string(),
            "审批意见不能超过 500 字符".to_string(),
        ));
    }
    match input.action.as_str() {
        "refuse" => {
            if input
                .comment
                .as_deref()
                .map_or(true, |value| value.trim().is_empty())
            {
                return Err(BaseError::ParamInvalid(
                    "comment".to_string(),
                    "拒绝必须填写审批意见".to_string(),
                ));
            }
        }
        "forward" => {
            if input
                .transfer_open_id
                .as_deref()
                .map_or(true, |value| value.trim().is_empty())
            {
                return Err(BaseError::ParamInvalid(
                    "transfer_open_id".to_string(),
                    "转交必须选择目标用户".to_string(),
                ));
            }
        }
        "add_sign" => {
            let unique: BTreeSet<_> = input
                .add_sign_open_ids
                .iter()
                .filter(|value| !value.trim().is_empty())
                .collect();
            if unique.is_empty() || unique.len() > 10 {
                return Err(BaseError::ParamInvalid(
                    "add_sign_open_ids".to_string(),
                    "加签目标必须为 1..=10 人".to_string(),
                ));
            }
            if !matches!(input.add_sign_type, Some(1..=3)) {
                return Err(BaseError::ParamInvalid(
                    "add_sign_type".to_string(),
                    "加签位置必须是 1/2/3".to_string(),
                ));
            }
            if !matches!(input.approval_method, Some(1..=3)) {
                return Err(BaseError::ParamInvalid(
                    "approval_method".to_string(),
                    "加签审批方式必须是 1/2/3".to_string(),
                ));
            }
        }
        "rollback"
            if !matches!(
                input.rollback_mode.as_deref(),
                Some("initiator" | "previous")
            ) =>
        {
            return Err(BaseError::ParamInvalid(
                "rollback_mode".to_string(),
                "退回方式必须是 initiator 或 previous".to_string(),
            ));
        }
        _ => {}
    }
    Ok(())
}

/// 目标用户校验：不能把任务转交/加签给自己，加签目标必须去重且非空。
fn validate_targets(input: &CreateJobInput, open_id: &str) -> Result<Vec<String>, BaseError> {
    if input.action == "forward" {
        let target = input
            .transfer_open_id
            .as_deref()
            .map(str::trim)
            .unwrap_or_default();
        if target == open_id {
            return Err(BaseError::ParamInvalid(
                "transfer_open_id".to_string(),
                "不能转交给自己".to_string(),
            ));
        }
        return Ok(Vec::new());
    }
    if input.action != "add_sign" {
        return Ok(Vec::new());
    }
    let mut seen = BTreeSet::new();
    let mut targets = Vec::new();
    for raw in &input.add_sign_open_ids {
        let target = raw.trim();
        if target.is_empty() {
            return Err(BaseError::ParamInvalid(
                "add_sign_open_ids".to_string(),
                "加签目标不能为空".to_string(),
            ));
        }
        if target == open_id {
            return Err(BaseError::ParamInvalid(
                "add_sign_open_ids".to_string(),
                "不能给自己加签".to_string(),
            ));
        }
        if !seen.insert(target.to_string()) {
            return Err(BaseError::ParamInvalid(
                "add_sign_open_ids".to_string(),
                "加签目标不能重复".to_string(),
            ));
        }
        targets.push(target.to_string());
    }
    Ok(targets)
}

/// 用通讯录详情确认目标仍在原租户且在职；`search/v1/user` 不返回这些状态。
async fn validate_target_users(
    ctx: &ActionContext,
    settings: &crate::config::FeishuSettings,
    token: &str,
    target_ids: &[String],
) -> Result<(), BaseError> {
    for open_id in target_ids.iter().filter(|id| !id.is_empty()) {
        let detail = api::user_detail(ctx, settings, token, open_id)
            .await
            .map_err(|error| error.to_base())?;
        let user = detail.pointer("/data/user").ok_or_else(|| {
            BaseError::ParamInvalid("target".to_string(), "目标用户不存在".to_string())
        })?;
        if user
            .pointer("/status/is_resigned")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || user
                .pointer("/status/is_activated")
                .and_then(Value::as_bool)
                == Some(false)
        {
            return Err(BaseError::ParamInvalid(
                "target".to_string(),
                "目标用户已离职或未激活".to_string(),
            ));
        }
        if !settings.login_allowed_tenant_keys.is_empty() {
            let tenant_key = user
                .get("tenant_key")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !settings
                .login_allowed_tenant_keys
                .iter()
                .any(|allowed| allowed == tenant_key)
            {
                return Err(BaseError::ParamInvalid(
                    "target".to_string(),
                    "目标用户不属于允许的飞书租户".to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn option_value(value: Option<&str>) -> Value {
    value
        .map(|value| Value::String(value.to_string()))
        .unwrap_or(Value::Null)
}

async fn find_existing_job(
    _ctx: &ActionContext,
    context: &FeishuContext,
    user_id: i64,
    idempotency_key: &str,
) -> Result<Option<i64>, BaseError> {
    let rows = context
        .batch_jobs()
        .query()
        .select_fields(&[JOB_ID])?
        .where_eq(JOB_USER_ID, Value::from(user_id))?
        .where_eq(
            JOB_IDEMPOTENCY_KEY,
            Value::String(idempotency_key.to_string()),
        )?
        .page(1, 1)?
        .all()
        .await?;
    rows.first()
        .map(|row| row.require::<i64>(JOB_ID))
        .transpose()
}

/// 小工具：事务提交/回滚，避免在 create_job 里重复。
struct AccountFinish;

impl AccountFinish {
    async fn commit(transaction: crate::db::Transaction, value: i64) -> Result<i64, BaseError> {
        transaction.commit().await.map_err(BaseError::from)?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(action: &str) -> CreateJobInput {
        CreateJobInput {
            action: action.to_string(),
            items: vec![JobItemInput {
                task_id: "t1".to_string(),
                instance_code: "i1".to_string(),
            }],
            comment: None,
            transfer_open_id: None,
            add_sign_open_ids: Vec::new(),
            add_sign_type: None,
            approval_method: None,
            rollback_mode: None,
            idempotency_key: "k1".to_string(),
        }
    }

    #[test]
    fn rejects_empty_items_and_unknown_action() {
        let mut empty = input("pass");
        empty.items.clear();
        assert!(validate_input(&empty, 50).is_err());
        let mut unknown = input("approve");
        unknown.action = "approve".to_string();
        assert!(validate_input(&unknown, 50).is_err());
    }

    #[test]
    fn refuse_requires_comment() {
        let value = input("refuse");
        assert!(validate_input(&value, 50).is_err());
        let mut with_comment = value;
        with_comment.comment = Some("金额与合同不符".to_string());
        assert!(validate_input(&with_comment, 50).is_ok());
    }

    #[test]
    fn forward_requires_target() {
        let value = input("forward");
        assert!(validate_input(&value, 50).is_err());
        let mut with_target = value;
        with_target.transfer_open_id = Some("ou_target".to_string());
        assert!(validate_input(&with_target, 50).is_ok());
    }

    #[test]
    fn add_sign_requires_one_to_ten_targets_and_type() {
        let mut value = input("add_sign");
        assert!(validate_input(&value, 50).is_err());
        value.add_sign_open_ids = vec!["ou_1".to_string()];
        value.add_sign_type = Some(1);
        value.approval_method = Some(1);
        assert!(validate_input(&value, 50).is_ok());
        value.add_sign_open_ids = (0..11).map(|i| format!("ou_{i}")).collect();
        assert!(validate_input(&value, 50).is_err());
    }

    #[test]
    fn targets_cannot_be_self_or_duplicated() {
        let mut forward = input("forward");
        forward.transfer_open_id = Some("ou_me".to_string());
        assert!(validate_targets(&forward, "ou_me").is_err());

        let mut add_sign = input("add_sign");
        add_sign.add_sign_type = Some(1);
        add_sign.approval_method = Some(1);
        add_sign.add_sign_open_ids = vec!["ou_1".to_string(), "ou_1".to_string()];
        assert!(validate_targets(&add_sign, "ou_me").is_err());

        add_sign.add_sign_open_ids = vec!["ou_me".to_string()];
        assert!(validate_targets(&add_sign, "ou_me").is_err());

        add_sign.add_sign_open_ids = vec![" ou_1 ".to_string()];
        assert_eq!(
            validate_targets(&add_sign, "ou_me").unwrap_or_default(),
            vec!["ou_1".to_string()]
        );
    }

    #[test]
    fn rollback_mode_is_limited_to_initiator_or_previous() {
        let mut value = input("rollback");
        assert!(validate_input(&value, 50).is_err());
        value.rollback_mode = Some("initiator".to_string());
        assert!(validate_input(&value, 50).is_ok());
        value.rollback_mode = Some("previous".to_string());
        assert!(validate_input(&value, 50).is_ok());
        value.rollback_mode = Some("start".to_string());
        assert!(validate_input(&value, 50).is_err());
    }
}

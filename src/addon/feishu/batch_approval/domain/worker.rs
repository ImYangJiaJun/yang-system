//! 批量审批 job 的进程内执行器。
//!
//! 写请求不自动重试未知错误，网络/5xx 标 `unknown` 后停止该 job，绝不盲目重放；
//! 只有明确限流（429 / 230020）才在同一 item 上退避重试。`run_due_jobs` 由
//! 基础设施周期 worker 调用，负责接管进程重启后遗留的 pending/running job，
//! 并把超过 5 分钟仍 running 的 item 标成 unknown 后核对详情。

use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use yang_base::action::ActionContext;
use yang_base::table::Record;
use yang_base::BaseError;

use crate::config::FeishuSettings;
use crate::infrastructure::audit;

use super::api;
use crate::addon::feishu::domain::batch_job_item_table::{
    ITEM_ATTEMPTS, ITEM_FINISHED_AT, ITEM_ID, ITEM_INSTANCE_CODE, ITEM_JOB_ID, ITEM_MESSAGE,
    ITEM_RESULT_CODE, ITEM_STARTED_AT, ITEM_STATUS, ITEM_TASK_ID,
};
use crate::addon::feishu::domain::batch_job_table::{
    JOB_ACTION, JOB_ADD_SIGN_OPEN_IDS, JOB_ADD_SIGN_TYPE, JOB_APPROVAL_METHOD, JOB_COMMENT,
    JOB_FAILED, JOB_FINISHED_AT, JOB_ID, JOB_ROLLBACK_MODE, JOB_STATUS, JOB_SUCCEEDED, JOB_TOTAL,
    JOB_TRANSFER_OPEN_ID, JOB_UNKNOWN, JOB_USER_ID,
};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::user_token::now_unix_secs;
use crate::addon::feishu::sso::credentials;

const REAUTH_CODES: &[i32] = &[99991672, 99991679, 20026, 20037, 20064, 20073];
const RATE_LIMIT_CODES: &[i32] = &[429, 230020, 99991400, 1254290];
const STALE_RUNNING_SECONDS: i64 = 300;
const MAX_ITEM_ATTEMPTS: i64 = 3;

/// 本进程内正在执行的 job。周期 worker 每轮都会看到 running job，若不拦截，
/// 同一 job 会被反复 spawn 出并行执行器；item 的 CAS 只能防重复写，不能防无谓
/// 的并发扫描与重复取 token。
static ACTIVE_JOBS: LazyLock<Mutex<HashSet<i64>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

fn claim_local_job(job_id: i64) -> bool {
    ACTIVE_JOBS
        .lock()
        .map(|mut jobs| jobs.insert(job_id))
        .unwrap_or(false)
}

struct LocalJobGuard(i64);

impl Drop for LocalJobGuard {
    fn drop(&mut self) {
        if let Ok(mut jobs) = ACTIVE_JOBS.lock() {
            jobs.remove(&self.0);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemOutcome {
    Continue,
    Stop,
}

pub(crate) fn spawn(
    ctx: ActionContext,
    context: Arc<FeishuContext>,
    settings: FeishuSettings,
    user_id: i64,
    job_id: i64,
) {
    if !claim_local_job(job_id) {
        return;
    }
    tokio::spawn(async move {
        let _guard = LocalJobGuard(job_id);
        if let Err(error) = process(ctx, context, settings, user_id, job_id).await {
            tracing::error!(error = %error, job_id, user_id, "批量审批 job 执行失败");
        }
    });
}

/// 周期 worker 的入口：先恢复进程中断遗留的 item，再为待执行 job 拉起执行器。
pub(crate) async fn run_due_jobs(
    ctx: &ActionContext,
    context: Arc<FeishuContext>,
    settings: Arc<FeishuSettings>,
    limit: usize,
) -> Result<usize, BaseError> {
    recover_stale_items(ctx, &context, settings.as_ref()).await?;
    let pending_rows = context
        .batch_jobs()
        .query()
        .select_fields(&[JOB_ID, JOB_USER_ID, JOB_STATUS])?
        .where_eq("status", Value::String("pending".to_string()))?
        .page(1, limit)?
        .all()
        .await?;
    let running_rows = context
        .batch_jobs()
        .query()
        .select_fields(&[JOB_ID, JOB_USER_ID, JOB_STATUS])?
        .where_eq("status", Value::String("running".to_string()))?
        .page(1, limit)?
        .all()
        .await?;
    let mut started = 0;
    for row in pending_rows.into_iter().chain(running_rows) {
        let job_id: i64 = row.require(JOB_ID)?;
        let user_id: i64 = row.require(JOB_USER_ID)?;
        if !claim_local_job(job_id) {
            continue;
        }
        let ctx = ctx.clone();
        let context = Arc::clone(&context);
        let settings = Arc::clone(&settings);
        tokio::spawn(async move {
            let _guard = LocalJobGuard(job_id);
            if let Err(error) = process(ctx, context, (*settings).clone(), user_id, job_id).await {
                tracing::error!(error = %error, job_id, user_id, "批量审批 job 恢复执行失败");
            }
        });
        started += 1;
    }
    Ok(started)
}

/// 进程重启/多实例竞争后，running 超过 5 分钟的 item 不能盲目重放。
async fn recover_stale_items(
    ctx: &ActionContext,
    context: &FeishuContext,
    settings: &FeishuSettings,
) -> Result<(), BaseError> {
    let cutoff = now_unix_secs() - STALE_RUNNING_SECONDS;
    let rows = context
        .batch_job_items()
        .query()
        .select_fields(&[
            ITEM_ID,
            ITEM_TASK_ID,
            ITEM_INSTANCE_CODE,
            ITEM_JOB_ID,
            ITEM_ATTEMPTS,
            ITEM_STARTED_AT,
        ])?
        .where_eq("status", Value::String("running".to_string()))?
        .page(1, 100)?
        .all()
        .await?;
    for row in rows {
        let started_at: Option<i64> = row.optional(ITEM_STARTED_AT)?;
        if started_at.is_some_and(|value| value > cutoff) {
            continue;
        }
        let item_id: i64 = row.require(ITEM_ID)?;
        let job_id: i64 = row.require(ITEM_JOB_ID)?;
        let task_id: String = row.require(ITEM_TASK_ID)?;
        let instance_code: String = row.require(ITEM_INSTANCE_CODE)?;
        let Some(job) = load_job_any(ctx, context, job_id).await? else {
            continue;
        };
        let user_id: i64 = job.require(JOB_USER_ID)?;
        let mut message = "执行进程中断，结果未知；请刷新待办确认，系统不会自动重发".to_string();
        let mut status = "unknown";
        let token = credentials::access_token_for_user(ctx, context, settings, user_id).await;
        if let Ok(token) = token {
            if let Ok(detail) = api::instance_detail(ctx, settings, &token, &instance_code).await {
                if task_status(&detail, &task_id) != Some("PENDING") {
                    status = "skipped";
                    message = "任务已被处理，跳过".to_string();
                }
            }
        }
        finish_item(ctx, context, item_id, status, Some(0), Some(&message)).await?;
        refresh_counts(ctx, context, job_id).await?;
    }
    Ok(())
}

async fn user_still_active(
    ctx: &ActionContext,
    context: &FeishuContext,
    user_id: i64,
) -> Result<bool, BaseError> {
    match context.external_login() {
        Some(port) => port.user_is_active(ctx, user_id).await,
        None => Ok(true),
    }
}

async fn process(
    ctx: ActionContext,
    context: Arc<FeishuContext>,
    settings: FeishuSettings,
    user_id: i64,
    job_id: i64,
) -> Result<(), BaseError> {
    if !mark_running(&ctx, &context, user_id, job_id).await? {
        let Some(job) = load_job(&ctx, &context, user_id, job_id).await? else {
            return Ok(());
        };
        if job.require::<String>(JOB_STATUS)? != "running" {
            return Ok(());
        }
    }
    if !user_still_active(&ctx, &context, user_id).await? {
        mark_pending_skipped(&ctx, &context, job_id).await?;
        update_job_status(
            &ctx,
            &context,
            user_id,
            job_id,
            "failed",
            Some(now_unix_secs()),
        )
        .await?;
        return Ok(());
    }
    // 写请求不重放：开跑前用一次新的待办列表确认任务仍在待办中。列表被截断时
    // 不能据此判定“不在待办”，否则会误跳过第 2001 条之后的任务。
    let token = match credentials::access_token_for_user(&ctx, &context, &settings, user_id).await {
        Ok(token) => token,
        Err(error) => {
            mark_pending_skipped(&ctx, &context, job_id).await?;
            update_job_status(
                &ctx,
                &context,
                user_id,
                job_id,
                "failed",
                Some(now_unix_secs()),
            )
            .await?;
            if matches!(error, credentials::CredentialError::ReauthRequired { .. }) {
                let _ = credentials::mark_reauth_required(&ctx, &context, user_id).await;
            }
            return Ok(());
        }
    };
    let fresh = match api::list_pending_tasks(&ctx, &settings, &token).await {
        Ok(list) if !list.truncated => Some(
            list.tasks
                .into_iter()
                .map(|task| (task.task_id, task.instance_code))
                .collect::<HashSet<_>>(),
        ),
        Ok(_) => None,
        Err(error) => {
            if REAUTH_CODES.contains(&error.code) {
                let _ = credentials::mark_reauth_required(&ctx, &context, user_id).await;
                mark_pending_skipped(&ctx, &context, job_id).await?;
                update_job_status(
                    &ctx,
                    &context,
                    user_id,
                    job_id,
                    "failed",
                    Some(now_unix_secs()),
                )
                .await?;
            } else {
                tracing::warn!(
                    code = error.code,
                    message = %error.message,
                    job_id,
                    user_id,
                    "批量审批开跑前拉待办失败，保留 job 等待重试"
                );
            }
            return Ok(());
        }
    };
    loop {
        let Some(job) = load_job(&ctx, &context, user_id, job_id).await? else {
            return Ok(());
        };
        let status: String = job.require(JOB_STATUS)?;
        if status == "cancelled" {
            mark_pending_skipped(&ctx, &context, job_id).await?;
            refresh_counts(&ctx, &context, job_id).await?;
            return Ok(());
        }
        if status != "running" {
            return Ok(());
        }
        let pending = load_pending_items(&ctx, &context, job_id).await?;
        if pending.is_empty() {
            finalize_job(&ctx, &context, user_id, job_id).await?;
            return Ok(());
        }
        for item in pending {
            let Some(job) = load_job(&ctx, &context, user_id, job_id).await? else {
                return Ok(());
            };
            if job.require::<String>(JOB_STATUS)? == "cancelled" {
                mark_pending_skipped(&ctx, &context, job_id).await?;
                refresh_counts(&ctx, &context, job_id).await?;
                return Ok(());
            }
            let outcome =
                process_item(&ctx, &context, &settings, &job, &item, fresh.as_ref()).await?;
            refresh_counts(&ctx, &context, job_id).await?;
            if outcome == ItemOutcome::Stop {
                return Ok(());
            }
        }
    }
}

async fn process_item(
    ctx: &ActionContext,
    context: &FeishuContext,
    settings: &FeishuSettings,
    job: &Record,
    item: &Record,
    fresh: Option<&HashSet<(String, String)>>,
) -> Result<ItemOutcome, BaseError> {
    let item_id: i64 = item.require(ITEM_ID)?;
    let job_id: i64 = item.require(ITEM_JOB_ID)?;
    let user_id: i64 = job.require(JOB_USER_ID)?;
    let task_id: String = item.require(ITEM_TASK_ID)?;
    let instance_code: String = item.require(ITEM_INSTANCE_CODE)?;
    let action: String = job.require(JOB_ACTION)?;
    if fresh.is_some_and(|tasks| !tasks.contains(&(task_id.clone(), instance_code.clone()))) {
        if claim_item(ctx, context, item_id).await?.is_some() {
            finish_item(
                ctx,
                context,
                item_id,
                "skipped",
                None,
                Some("任务已不在待办中，跳过"),
            )
            .await?;
        }
        return Ok(ItemOutcome::Continue);
    }
    // 先领全局配额再 claim：排队等令牌时不能把 item 提前标成 running，
    // 否则超过 5 分钟的排队会被恢复逻辑误判成 unknown。
    super::rate_limit::acquire(&action).await;
    let Some(latest) = load_job(ctx, context, user_id, job_id).await? else {
        return Ok(ItemOutcome::Continue);
    };
    if latest.require::<String>(JOB_STATUS)? != "running" {
        return Ok(ItemOutcome::Continue);
    }
    let Some(mut attempt) = claim_item(ctx, context, item_id).await? else {
        return Ok(ItemOutcome::Continue);
    };
    let token = match credentials::access_token_for_user(ctx, context, settings, user_id).await {
        Ok(token) => token,
        Err(error) => {
            finish_item(
                ctx,
                context,
                item_id,
                "failed",
                None,
                Some(&error.user_message()),
            )
            .await?;
            mark_pending_skipped(ctx, context, job_id).await?;
            update_job_status(
                ctx,
                context,
                user_id,
                job_id,
                "failed",
                Some(now_unix_secs()),
            )
            .await?;
            if matches!(error, credentials::CredentialError::ReauthRequired { .. }) {
                let _ = credentials::mark_reauth_required(ctx, context, user_id).await;
            }
            return Ok(ItemOutcome::Stop);
        }
    };
    let body = match build_body(
        ctx,
        settings,
        &token,
        job,
        &action,
        &task_id,
        &instance_code,
    )
    .await
    {
        Ok(body) => body,
        Err(message) => {
            finish_item(ctx, context, item_id, "failed", None, Some(&message)).await?;
            return Ok(ItemOutcome::Continue);
        }
    };
    loop {
        match api::task_action(ctx, settings, &token, &action, body.clone()).await {
            Ok(_) => {
                finish_item(ctx, context, item_id, "succeeded", Some(0), None).await?;
                return Ok(ItemOutcome::Continue);
            }
            Err(error) if RATE_LIMIT_CODES.contains(&error.code) && attempt < MAX_ITEM_ATTEMPTS => {
                attempt += 1;
                update_item_attempts(ctx, context, item_id, attempt).await?;
                tokio::time::sleep(Duration::from_secs(1_u64 << attempt)).await;
                // 重试也是一次真实出站，必须重新领取全局令牌。
                super::rate_limit::acquire(&action).await;
            }
            Err(error) => {
                return handle_item_error(
                    ctx,
                    context,
                    settings,
                    user_id,
                    job_id,
                    item_id,
                    &task_id,
                    &instance_code,
                    &token,
                    attempt,
                    error,
                )
                .await;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_item_error(
    ctx: &ActionContext,
    context: &FeishuContext,
    settings: &FeishuSettings,
    user_id: i64,
    job_id: i64,
    item_id: i64,
    task_id: &str,
    instance_code: &str,
    token: &str,
    attempt: i64,
    error: api::ApiError,
) -> Result<ItemOutcome, BaseError> {
    let now = now_unix_secs();
    if error.code == 0 {
        // 网络/5xx：写入结果未知，绝不重发，也不继续处理同 job 的后续任务。
        finish_item(
            ctx,
            context,
            item_id,
            "unknown",
            Some(0),
            Some("网络或飞书服务异常，结果未知；请刷新待办确认"),
        )
        .await?;
        mark_pending_skipped(ctx, context, job_id).await?;
        update_job_status(ctx, context, user_id, job_id, "partial", Some(now)).await?;
        return Ok(ItemOutcome::Stop);
    }
    let message = format!("飞书返回 {}：{}", error.code, error.message);
    if error.code == 1395001 {
        // 任务状态冲突：先看详情，已不是 PENDING 就是并发处理掉了。
        let status = api::instance_detail(ctx, settings, token, instance_code)
            .await
            .ok()
            .and_then(|detail| task_status(&detail, task_id).map(str::to_string));
        if status.as_deref() != Some("PENDING") {
            finish_item(
                ctx,
                context,
                item_id,
                "skipped",
                Some(error.code),
                Some("任务已不在待审批状态，跳过"),
            )
            .await?;
            return Ok(ItemOutcome::Continue);
        }
        finish_item(
            ctx,
            context,
            item_id,
            "failed",
            Some(error.code),
            Some(&message),
        )
        .await?;
        return Ok(ItemOutcome::Continue);
    }
    if matches!(error.code, 1390018 | 1390001) {
        finish_item(
            ctx,
            context,
            item_id,
            "failed",
            Some(error.code),
            Some("该审批需要手写签名或条件表单，请单独处理"),
        )
        .await?;
        return Ok(ItemOutcome::Continue);
    }
    if RATE_LIMIT_CODES.contains(&error.code) {
        finish_item(
            ctx,
            context,
            item_id,
            "failed",
            Some(error.code),
            Some(&format!("{message}（已重试 {attempt} 次，请稍后再试）")),
        )
        .await?;
        return Ok(ItemOutcome::Continue);
    }
    finish_item(
        ctx,
        context,
        item_id,
        "failed",
        Some(error.code),
        Some(&message),
    )
    .await?;
    if REAUTH_CODES.contains(&error.code) {
        let _ = credentials::mark_reauth_required(ctx, context, user_id).await;
        mark_pending_skipped(ctx, context, job_id).await?;
        update_job_status(ctx, context, user_id, job_id, "failed", Some(now)).await?;
        return Ok(ItemOutcome::Stop);
    }
    Ok(ItemOutcome::Continue)
}

async fn build_body(
    ctx: &ActionContext,
    settings: &FeishuSettings,
    token: &str,
    job: &Record,
    action: &str,
    task_id: &str,
    instance_code: &str,
) -> Result<Value, String> {
    let mut body = json!({
        "instance_code": instance_code,
        "task_id": task_id,
    });
    if let Some(comment) = job
        .optional::<String>(JOB_COMMENT)
        .map_err(|error| error.to_string())?
    {
        if !comment.trim().is_empty() {
            body["comment"] = Value::String(comment);
        }
    }
    match action {
        "forward" => {
            let target = job
                .optional::<String>(JOB_TRANSFER_OPEN_ID)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "缺少转交目标".to_string())?;
            body["transfer_user_id"] = Value::String(target);
        }
        "add_sign" => {
            let targets = job
                .optional::<String>(JOB_ADD_SIGN_OPEN_IDS)
                .map_err(|error| error.to_string())?
                .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
                .ok_or_else(|| "缺少加签目标".to_string())?;
            body["add_sign_user_ids"] = json!(targets);
            if let Some(value) = job
                .optional::<i64>(JOB_ADD_SIGN_TYPE)
                .map_err(|error| error.to_string())?
            {
                body["add_sign_type"] = Value::from(value);
            }
            if let Some(value) = job
                .optional::<i64>(JOB_APPROVAL_METHOD)
                .map_err(|error| error.to_string())?
            {
                body["approval_method"] = Value::from(value);
            }
        }
        "rollback" => {
            let mode = job
                .optional::<String>(JOB_ROLLBACK_MODE)
                .map_err(|error| error.to_string())?
                .unwrap_or_else(|| "initiator".to_string());
            let node_ids = if mode == "initiator" {
                vec!["START".to_string()]
            } else {
                let detail = api::instance_detail(ctx, settings, token, instance_code)
                    .await
                    .map_err(|error| format!("取实例详情失败：{}", error.message))?;
                previous_node_id(&detail)
                    .ok_or_else(|| "无法确定上一节点，请单独处理该任务".to_string())?
            };
            body["node_ids"] = json!(node_ids);
        }
        _ => {}
    }
    Ok(body)
}

fn previous_node_id(detail: &Value) -> Option<Vec<String>> {
    let records = detail
        .get("timeline")
        .and_then(Value::as_array)
        .or_else(|| detail.get("operation_records").and_then(Value::as_array))?;
    for record in records.iter().rev() {
        let kind = record
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if kind != "PASS" {
            continue;
        }
        if let Some(node) = record
            .get("node_id")
            .and_then(Value::as_str)
            .or_else(|| record.get("node_key").and_then(Value::as_str))
        {
            if !node.is_empty() {
                return Some(vec![node.to_string()]);
            }
        }
    }
    None
}

fn task_status<'a>(detail: &'a Value, task_id: &str) -> Option<&'a str> {
    detail
        .get("task_list")
        .and_then(Value::as_array)
        .or_else(|| detail.get("tasks").and_then(Value::as_array))?
        .iter()
        .find(|task| {
            task.get("id")
                .or_else(|| task.get("task_id"))
                .and_then(value_text)
                .as_deref()
                == Some(task_id)
        })?
        .get("status")?
        .as_str()
}

fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => None,
    }
}

async fn load_job(
    ctx: &ActionContext,
    context: &FeishuContext,
    user_id: i64,
    job_id: i64,
) -> Result<Option<Record>, BaseError> {
    let rows = context
        .batch_jobs()
        .query()
        .select_fields(&[
            JOB_ID,
            JOB_USER_ID,
            JOB_ACTION,
            JOB_STATUS,
            JOB_COMMENT,
            JOB_TRANSFER_OPEN_ID,
            JOB_ADD_SIGN_OPEN_IDS,
            JOB_ADD_SIGN_TYPE,
            JOB_APPROVAL_METHOD,
            JOB_ROLLBACK_MODE,
            JOB_TOTAL,
            JOB_SUCCEEDED,
            JOB_FAILED,
            JOB_UNKNOWN,
        ])?
        .where_eq(JOB_ID, Value::from(job_id))?
        .where_eq(JOB_USER_ID, Value::from(user_id))?
        .page(1, 1)?
        .all()
        .await?;
    let _ = ctx;
    Ok(rows.into_iter().next())
}

async fn load_job_any(
    ctx: &ActionContext,
    context: &FeishuContext,
    job_id: i64,
) -> Result<Option<Record>, BaseError> {
    let _ = ctx;
    let rows = context
        .batch_jobs()
        .query()
        .select_fields(&[JOB_ID, JOB_USER_ID, JOB_STATUS])?
        .where_eq(JOB_ID, Value::from(job_id))?
        .page(1, 1)?
        .all()
        .await?;
    Ok(rows.into_iter().next())
}

async fn load_pending_items(
    ctx: &ActionContext,
    context: &FeishuContext,
    job_id: i64,
) -> Result<Vec<Record>, BaseError> {
    let _ = ctx;
    context
        .batch_job_items()
        .query()
        .select_fields(&[
            ITEM_ID,
            ITEM_TASK_ID,
            ITEM_INSTANCE_CODE,
            ITEM_STATUS,
            ITEM_ATTEMPTS,
            ITEM_JOB_ID,
        ])?
        .where_eq(ITEM_JOB_ID, Value::from(job_id))?
        .where_eq("status", Value::String("pending".to_string()))?
        .page(1, 100)?
        .all()
        .await
}

/// CAS 抢占一个 item；抢不到说明另一个执行器已在处理，不能重复写飞书。
async fn claim_item(
    ctx: &ActionContext,
    context: &FeishuContext,
    item_id: i64,
) -> Result<Option<i64>, BaseError> {
    let _ = ctx;
    let rows = context
        .batch_job_items()
        .query()
        .select_fields(&[ITEM_ATTEMPTS])?
        .where_eq(ITEM_ID, Value::from(item_id))?
        .page(1, 1)?
        .all()
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let attempt: i64 = row.require::<i64>(ITEM_ATTEMPTS)? + 1;
    let affected = context
        .batch_job_items()
        .query()
        .where_eq(ITEM_ID, Value::from(item_id))?
        .where_eq("status", Value::String("pending".to_string()))?
        .update(
            Record::new()
                .set(ITEM_STATUS, "running")
                .set(ITEM_ATTEMPTS, attempt)
                .set(ITEM_STARTED_AT, now_unix_secs()),
        )
        .await?;
    Ok((affected == 1).then_some(attempt))
}

async fn update_item_attempts(
    ctx: &ActionContext,
    context: &FeishuContext,
    item_id: i64,
    attempt: i64,
) -> Result<(), BaseError> {
    let _ = ctx;
    context
        .batch_job_items()
        .query()
        .where_eq(ITEM_ID, Value::from(item_id))?
        .update(Record::new().set(ITEM_ATTEMPTS, attempt))
        .await?;
    Ok(())
}

async fn finish_item(
    ctx: &ActionContext,
    context: &FeishuContext,
    item_id: i64,
    status: &str,
    code: Option<i32>,
    message: Option<&str>,
) -> Result<(), BaseError> {
    let _ = ctx;
    let now = now_unix_secs();
    context
        .batch_job_items()
        .query()
        .where_eq(ITEM_ID, Value::from(item_id))?
        .where_in(
            "status",
            vec![
                Value::String("pending".to_string()),
                Value::String("running".to_string()),
            ],
        )?
        .update(
            Record::new()
                .set(ITEM_STATUS, status)
                .set(
                    ITEM_RESULT_CODE,
                    code.map(Value::from).unwrap_or(Value::Null),
                )
                .set(
                    ITEM_MESSAGE,
                    message
                        .map(|value| Value::String(value.to_string()))
                        .unwrap_or(Value::Null),
                )
                .set(ITEM_FINISHED_AT, now),
        )
        .await?;
    Ok(())
}

async fn mark_pending_skipped(
    ctx: &ActionContext,
    context: &FeishuContext,
    job_id: i64,
) -> Result<(), BaseError> {
    let _ = ctx;
    context
        .batch_job_items()
        .query()
        .where_eq(ITEM_JOB_ID, Value::from(job_id))?
        .where_eq("status", Value::String("pending".to_string()))?
        .update(
            Record::new()
                .set(ITEM_STATUS, "skipped")
                .set(ITEM_FINISHED_AT, now_unix_secs()),
        )
        .await?;
    Ok(())
}

async fn refresh_counts(
    ctx: &ActionContext,
    context: &FeishuContext,
    job_id: i64,
) -> Result<(), BaseError> {
    let rows = context
        .batch_job_items()
        .query()
        .select_fields(&[ITEM_STATUS])?
        .where_eq(ITEM_JOB_ID, Value::from(job_id))?
        .page(1, 100)?
        .all()
        .await?;
    let mut succeeded = 0_i64;
    let mut failed = 0_i64;
    let mut unknown = 0_i64;
    let mut pending = 0_i64;
    for row in &rows {
        match row.require::<String>(ITEM_STATUS)?.as_str() {
            "succeeded" => succeeded += 1,
            "failed" => failed += 1,
            "unknown" => unknown += 1,
            "pending" | "running" => pending += 1,
            _ => {}
        }
    }
    let _ = ctx;
    context
        .batch_jobs()
        .query()
        .where_eq(JOB_ID, Value::from(job_id))?
        .update(
            Record::new()
                .set(JOB_SUCCEEDED, succeeded)
                .set(JOB_FAILED, failed)
                .set(JOB_UNKNOWN, unknown),
        )
        .await?;
    if pending == 0 {
        let status = if failed == 0 && unknown == 0 {
            "succeeded"
        } else if succeeded == 0 {
            "failed"
        } else {
            "partial"
        };
        update_job_status_raw(ctx, context, job_id, status, Some(now_unix_secs())).await?;
    }
    Ok(())
}

async fn finalize_job(
    ctx: &ActionContext,
    context: &FeishuContext,
    user_id: i64,
    job_id: i64,
) -> Result<(), BaseError> {
    refresh_counts(ctx, context, job_id).await?;
    let _ = user_id;
    Ok(())
}

async fn mark_running(
    ctx: &ActionContext,
    context: &FeishuContext,
    user_id: i64,
    job_id: i64,
) -> Result<bool, BaseError> {
    let _ = ctx;
    let affected = context
        .batch_jobs()
        .query()
        .where_eq(JOB_ID, Value::from(job_id))?
        .where_eq(JOB_USER_ID, Value::from(user_id))?
        .where_eq("status", Value::String("pending".to_string()))?
        .update(Record::new().set(JOB_STATUS, "running"))
        .await?;
    Ok(affected == 1)
}

async fn update_job_status(
    ctx: &ActionContext,
    context: &FeishuContext,
    user_id: i64,
    job_id: i64,
    status: &str,
    finished_at: Option<i64>,
) -> Result<(), BaseError> {
    let _ = ctx;
    let mut record = Record::new().set(JOB_STATUS, status);
    if let Some(at) = finished_at {
        record = record.set(JOB_FINISHED_AT, at);
    }
    let affected = context
        .batch_jobs()
        .query()
        .where_eq(JOB_ID, Value::from(job_id))?
        .where_eq(JOB_USER_ID, Value::from(user_id))?
        .where_eq("status", Value::String("running".to_string()))?
        .update(record)
        .await?;
    if affected == 1 {
        audit_finished(ctx, context, job_id, status).await?;
    }
    Ok(())
}

async fn update_job_status_raw(
    ctx: &ActionContext,
    context: &FeishuContext,
    job_id: i64,
    status: &str,
    finished_at: Option<i64>,
) -> Result<(), BaseError> {
    let mut record = Record::new().set(JOB_STATUS, status);
    if let Some(at) = finished_at {
        record = record.set(JOB_FINISHED_AT, at);
    }
    let affected = context
        .batch_jobs()
        .query()
        .where_eq(JOB_ID, Value::from(job_id))?
        .where_eq("status", Value::String("running".to_string()))?
        .update(record)
        .await?;
    if affected == 1 {
        audit_finished(ctx, context, job_id, status).await?;
    }
    Ok(())
}

async fn audit_finished(
    ctx: &ActionContext,
    context: &FeishuContext,
    job_id: i64,
    status: &str,
) -> Result<(), BaseError> {
    let Some(job) = load_job_any(ctx, context, job_id).await? else {
        return Ok(());
    };
    let user_id: i64 = job.require(JOB_USER_ID)?;
    let event = audit::succeeded_system_event_without_ctx(
        "feishu_batch_worker",
        "feishu.batch.job_finished",
        Some(audit::entity("user", user_id)?),
        audit::entity("feishu_batch_job", job_id)?,
        audit::summary([("status", json!(status))])?,
    )?;
    audit::append_independent(ctx.tools().mysql()?.pool(), &event).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_status_reads_the_matching_task() {
        let detail = json!({
            "tasks": [
                {"task_id": "t1", "status": "PENDING"},
                {"task_id": "t2", "status": "APPROVED"},
            ],
        });
        assert_eq!(task_status(&detail, "t1"), Some("PENDING"));
        assert_eq!(task_status(&detail, "t2"), Some("APPROVED"));
        assert_eq!(task_status(&detail, "missing"), None);
    }
}

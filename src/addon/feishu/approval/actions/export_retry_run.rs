//! 人工重试一条回写运行记录。
//!
//! # 只重试「可恢复」状态
//!
//! 行级 `pending` / `retry` / `review` 与规则级同名状态可以重开；`writing` 可能
//! 正在 worker 手里，`written` 已经成功，`terminal` / `skipped` 是配置或数据错误
//! 的终态——重试不会变好，必须先改配置或修数据。把这些状态一律拒绝，避免管理员
//! 用「点一下重试」掩盖真实问题。
//!
//! # 行级行必须同时唤醒规则级行
//!
//! worker 只认领规则级行（`row_key=__rule__`）。单独把一条行级行改成 `retry`
//! 不会让 worker 看它一眼，所以这里同时把同 (规则, 实例) 的规则行拉到当前时间；
//! 规则行不存在时补建一条，保证操作不是空转。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;
use yang_db::{field, table, CompareOp, QueryBuilder};

use crate::addon::feishu::domain::approval_export::RULE_ROW_KEY;
use crate::addon::feishu::domain::approval_provision::now_unix_secs;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::infrastructure::audit;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct RetryExportRunInput {
    /// 运行记录主键；规则级行与行级行都可以。
    pub(super) run_id: i64,
}

impl ParamInput for RetryExportRunInput {
    fn params() -> Params {
        Params::new()
    }
}

impl RetryExportRunInput {
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
            yang_base::action_name!("export_retry_run"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/export/runs/retry",
        )
        .display_name("重试审批回写运行")
        .description("把 pending / retry / review 的运行记录重新排入队列，并唤醒对应规则行")
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: RetryExportRunInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<serde_json::Value, BaseError> = async {
        let row = crate::db::select_for_update::<Record>(
            &mut transaction,
            QueryBuilder::from_pool(
                context.approval_export_runs().pool(),
                table!("feishu_approval_export_run"),
            )
            .fields(&[
                field!("id"),
                field!("rule_id"),
                field!("approval_code"),
                field!("instance_code"),
                field!("row_key"),
                field!("write_status"),
            ])
            .where_and(field!("id"), CompareOp::Eq, input.run_id)
            .map_err(BaseError::from)?,
        )
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| BaseError::RecordNotFound("回写运行记录不存在".to_string()))?;

        let previous_status: String = row.require("write_status")?;
        if !matches!(previous_status.as_str(), "pending" | "retry" | "review") {
            return Err(BaseError::ParamInvalid(
                "run_id".to_string(),
                format!(
                    "当前状态「{previous_status}」不可重试：只支持 pending / retry / review；\
                     writing 可能正在处理，written 已成功，terminal/skipped 需先修配置"
                ),
            ));
        }
        let rule_id: i64 = row.require("rule_id")?;
        let approval_code: String = row.require("approval_code")?;
        let instance_code: String = row.require("instance_code")?;
        let row_key: String = row.require("row_key")?;

        // 停用 / 删除的规则不会被 worker 加载；重试这些行只会「成功排队、永不执行」。
        ensure_rule_enabled(&mut transaction, &context, rule_id).await?;

        let mut update = Record::new();
        update.insert("write_status", serde_json::json!("retry"));
        update.insert("attempts", serde_json::json!(0));
        update.insert("next_attempt_at", serde_json::json!(now_unix_secs()));
        update.insert("lease_until", serde_json::Value::Null);
        update.insert("worker_id", serde_json::Value::Null);
        update.insert("last_error", serde_json::Value::Null);
        context
            .approval_export_runs()
            .query()
            .where_eq("id", serde_json::json!(input.run_id))?
            .update_in_tx(&mut transaction, update)
            .await?;

        let woke_rule = if row_key == RULE_ROW_KEY {
            false
        } else {
            wake_rule_row(
                &mut transaction,
                &context,
                rule_id,
                &approval_code,
                &instance_code,
            )
            .await?
        };

        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_approval_export_run", input.run_id)?,
            Some(audit::summary([(
                "write_status",
                serde_json::json!(&previous_status),
            )])?),
            Some(audit::summary([
                ("run_id", serde_json::json!(input.run_id)),
                ("write_status", serde_json::json!("retry")),
                ("woke_rule", serde_json::json!(woke_rule)),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;

        Ok(serde_json::json!({
            "run_id": input.run_id,
            "previous_status": previous_status,
            "write_status": "retry",
            "woke_rule": woke_rule,
        }))
    }
    .await;
    let value = FeishuContext::finish_transaction(transaction, result).await?;

    ApiResponse::success(value, "已重新排队")
}

/// 重试/重建前确认规则仍处于启用状态：worker 只加载启用规则，
/// 对停用/删除规则的行做「重新排队」是空转。
pub(super) async fn ensure_rule_enabled(
    transaction: &mut crate::db::Transaction,
    context: &FeishuContext,
    rule_id: i64,
) -> Result<(), BaseError> {
    let rows = context
        .approval_export_rules()
        .query()
        .select_fields(&["enabled"])?
        .where_eq("id", serde_json::json!(rule_id))?
        .all_in_tx(transaction)
        .await?;
    let Some(row) = rows.first() else {
        return Err(BaseError::ParamInvalid(
            "run_id".to_string(),
            "所属回写规则已删除；运行记录仅作追溯，不能再重试".to_string(),
        ));
    };
    if !row.optional::<bool>("enabled")?.unwrap_or(false) {
        return Err(BaseError::ParamInvalid(
            "run_id".to_string(),
            "所属回写规则已停用；请先启用规则再重试".to_string(),
        ));
    }
    Ok(())
}

/// 把同 (规则, 实例) 的规则级行拉到当前时间；不存在则补建。
async fn wake_rule_row(
    transaction: &mut crate::db::Transaction,
    context: &FeishuContext,
    rule_id: i64,
    approval_code: &str,
    instance_code: &str,
) -> Result<bool, BaseError> {
    let existing = context
        .approval_export_runs()
        .query()
        .select_fields(&["id"])?
        .where_eq("rule_id", serde_json::json!(rule_id))?
        .where_eq("instance_code", serde_json::json!(instance_code))?
        .where_eq("trigger_node_id", serde_json::json!(RULE_ROW_KEY))?
        .where_eq("row_key", serde_json::json!(RULE_ROW_KEY))?
        .where_eq("generation", serde_json::json!(0))?
        .all_in_tx(transaction)
        .await?;

    if let Some(rule_row) = existing.first() {
        let id: i64 = rule_row.require("id")?;
        let mut update = Record::new();
        update.insert("write_status", serde_json::json!("retry"));
        update.insert("attempts", serde_json::json!(0));
        update.insert("next_attempt_at", serde_json::json!(now_unix_secs()));
        update.insert("lease_until", serde_json::Value::Null);
        update.insert("worker_id", serde_json::Value::Null);
        update.insert("last_error", serde_json::Value::Null);
        context
            .approval_export_runs()
            .query()
            .where_eq("id", serde_json::json!(id))?
            .update_in_tx(transaction, update)
            .await?;
        return Ok(true);
    }

    let mut record = Record::new();
    record.insert("rule_id", serde_json::json!(rule_id));
    record.insert("approval_code", serde_json::json!(approval_code));
    record.insert("instance_code", serde_json::json!(instance_code));
    record.insert("trigger_node_id", serde_json::json!(RULE_ROW_KEY));
    record.insert("row_key", serde_json::json!(RULE_ROW_KEY));
    record.insert("generation", serde_json::json!(0));
    record.insert("write_status", serde_json::json!("retry"));
    record.insert("mark_status", serde_json::json!("none"));
    record.insert("attempts", serde_json::json!(0));
    record.insert("next_attempt_at", serde_json::json!(now_unix_secs()));
    context
        .approval_export_runs()
        .query()
        .insert_in_tx(transaction, record)
        .await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_positive_ids_are_accepted() {
        assert!(RetryExportRunInput { run_id: 0 }.validate().is_err());
        assert!(RetryExportRunInput { run_id: 3 }.validate().is_ok());
    }
}

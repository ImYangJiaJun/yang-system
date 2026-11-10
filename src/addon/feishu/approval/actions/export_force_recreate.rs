//! 强制重建一条已写入的目标记录。
//!
//! # 为什么必须换代次与新 Token
//!
//! `create_record` 的 `client_token` 是服务端幂等键：同一 token 重放会返回同一个
//! `record_id`。目标行被手工删除后，重放旧 token 仍然只返回旧 id（记录已不存在），
//! 「写成功」是假的。唯一可靠的恢复方式是 `generation + 1` 并用新代次派生新 token
//! （`client_token` 的输入包含代次，见幂等模块）。
//!
//! 本端点只把台账推进到下一代并重新排队，真正的 `create_record` 仍由 worker 执行
//!（worker 按代次最大的一行取 token，见 `find_row_run`）。这样写入路径只有
//! 一条，不会出现「Action 里写一次、worker 又写一次」的双写。
//!
//! # 只处理行级行
//!
//! 规则级行（`__rule__`）没有目标记录；对它做重建没有语义。必须给出行级 run id。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;
use yang_db::{field, table, CompareOp, QueryBuilder};

use crate::addon::feishu::domain::approval_export::{client_token, RULE_ROW_KEY};
use crate::addon::feishu::domain::approval_provision::now_unix_secs;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::infrastructure::audit;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ForceRecreateInput {
    /// 行级运行记录主键（`row_key=row:N`，且已有 `target_record_id`）。
    pub(super) run_id: i64,
}

impl ParamInput for ForceRecreateInput {
    fn params() -> Params {
        Params::new()
    }
}

impl ForceRecreateInput {
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

/// 注册强制重建端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("export_force_recreate"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/export/runs/force-recreate",
        )
        .display_name("强制重建回写目标记录")
        .description("目标记录被手工删除后，代次 +1 并换新幂等 Token 重新创建")
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: ForceRecreateInput,
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
                field!("generation"),
                field!("write_status"),
                field!("target_record_id"),
            ])
            .where_and(field!("id"), CompareOp::Eq, input.run_id)
            .map_err(BaseError::from)?,
        )
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| BaseError::RecordNotFound("回写运行记录不存在".to_string()))?;

        let row_key: String = row.require("row_key")?;
        if row_key == RULE_ROW_KEY {
            return Err(BaseError::ParamInvalid(
                "run_id".to_string(),
                "规则级运行行没有目标记录；请选择一条行级（row:N）记录".to_string(),
            ));
        }
        let write_status: String = row.require("write_status")?;
        if write_status == "writing" {
            return Err(BaseError::ParamInvalid(
                "run_id".to_string(),
                "该行正在写入（writing），请等待本轮结束后再强制重建".to_string(),
            ));
        }
        let previous_target: Option<String> = row.optional("target_record_id")?;
        if previous_target
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
        {
            return Err(BaseError::ParamInvalid(
                "run_id".to_string(),
                "该行没有已写入的目标记录；请使用「重试」而不是「强制重建」".to_string(),
            ));
        }

        let rule_id: i64 = row.require("rule_id")?;
        let approval_code: String = row.require("approval_code")?;
        let instance_code: String = row.require("instance_code")?;
        super::export_retry_run::ensure_rule_enabled(&mut transaction, &context, rule_id).await?;
        let previous_generation: i64 = row.optional("generation")?.unwrap_or(0);
        let generation = previous_generation + 1;
        let token = client_token(rule_id, &instance_code, &row_key, generation);

        let mut update = Record::new();
        update.insert("generation", serde_json::json!(generation));
        update.insert("client_token", serde_json::json!(&token));
        update.insert("target_record_id", serde_json::Value::Null);
        update.insert("write_status", serde_json::json!("retry"));
        update.insert("mark_status", serde_json::json!("none"));
        update.insert("applied_label", serde_json::Value::Null);
        update.insert("attempts", serde_json::json!(0));
        update.insert("next_attempt_at", serde_json::json!(now_unix_secs()));
        update.insert("lease_until", serde_json::Value::Null);
        update.insert("worker_id", serde_json::Value::Null);
        update.insert("last_error", serde_json::Value::Null);
        update.insert("written_at", serde_json::Value::Null);
        update.insert("marked_at", serde_json::Value::Null);
        let updated = context
            .approval_export_runs()
            .query()
            .where_eq("id", serde_json::json!(input.run_id))?
            .update_in_tx(&mut transaction, update)
            .await
            .map_err(|error| {
                if crate::db::is_unique_violation(&error) {
                    BaseError::ParamInvalid(
                        "run_id".to_string(),
                        "下一代运行记录已存在，请刷新后重试".to_string(),
                    )
                } else {
                    error
                }
            })?;
        if updated == 0 {
            return Err(BaseError::RecordNotFound("回写运行记录不存在".to_string()));
        }

        wake_rule_row(
            &mut transaction,
            &context,
            rule_id,
            &approval_code,
            &instance_code,
        )
        .await?;

        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_approval_export_run", input.run_id)?,
            Some(audit::summary([
                ("generation", serde_json::json!(previous_generation)),
                ("write_status", serde_json::json!(&write_status)),
            ])?),
            Some(audit::summary([
                ("run_id", serde_json::json!(input.run_id)),
                ("generation", serde_json::json!(generation)),
                ("write_status", serde_json::json!("retry")),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;

        tracing::info!(
            run_id = input.run_id,
            rule_id,
            generation,
            "回写目标记录已安排强制重建"
        );

        Ok(serde_json::json!({
            "run_id": input.run_id,
            "previous_generation": previous_generation,
            "generation": generation,
            "write_status": "retry",
            "woke_rule": true,
        }))
    }
    .await;
    let value = FeishuContext::finish_transaction(transaction, result).await?;

    ApiResponse::success(value, "已安排强制重建")
}

/// 把同 (规则, 实例) 的规则级行拉到当前时间；不存在则补建（与重试端点同一语义）。
async fn wake_rule_row(
    transaction: &mut crate::db::Transaction,
    context: &FeishuContext,
    rule_id: i64,
    approval_code: &str,
    instance_code: &str,
) -> Result<(), BaseError> {
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
        return Ok(());
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_positive_ids_are_accepted() {
        assert!(ForceRecreateInput { run_id: 0 }.validate().is_err());
        assert!(ForceRecreateInput { run_id: 7 }.validate().is_ok());
    }

    /// 代次必须真的换 token：这是「目标行被删除后重建」能成立的全部依据。
    #[test]
    fn a_new_generation_derives_a_different_token() {
        let before = client_token(3, "INST-1", "row:0", 0);
        let after = client_token(3, "INST-1", "row:0", 1);
        assert_ne!(before, after);
    }
}

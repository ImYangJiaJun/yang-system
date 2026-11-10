//! 删除回写规则（前端控制台）。
//!
//! 只删规则行与字段映射行，**保留运行记录**：已写入的目标记录不会因为删规则而
//! 消失，台账是这些记录的来源证据；worker 对规则已删除的运行行会置 `skipped`
//!（见回写 worker 的 `rules_by_id` 分支），不需要级联清理。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::infrastructure::audit;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteExportRuleInput {
    pub(super) rule_id: i64,
}

impl ParamInput for DeleteExportRuleInput {
    fn params() -> Params {
        Params::new()
    }
}

impl DeleteExportRuleInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.rule_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "rule_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        Ok(())
    }
}

/// 注册删除规则端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("export_delete_rule"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/export/rules/delete",
        )
        .display_name("删除审批回写规则")
        .description("删除回写规则及其字段映射；运行记录保留，便于追溯已写入的目标记录")
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: DeleteExportRuleInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<u64, BaseError> = async {
        let deleted_maps = context
            .approval_export_field_maps()
            .query()
            .where_eq("rule_id", serde_json::json!(input.rule_id))?
            .delete_in_tx(&mut transaction)
            .await?;
        let deleted_rules = context
            .approval_export_rules()
            .query()
            .where_eq("id", serde_json::json!(input.rule_id))?
            .delete_in_tx(&mut transaction)
            .await?;
        if deleted_rules == 0 {
            return Err(BaseError::RecordNotFound("审批回写规则不存在".to_string()));
        }
        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_approval_export_rule", input.rule_id)?,
            None,
            Some(audit::summary([
                ("rule_id", serde_json::json!(input.rule_id)),
                ("deleted_maps", serde_json::json!(deleted_maps)),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(deleted_maps)
    }
    .await;
    let deleted_maps = FeishuContext::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        serde_json::json!({
            "rule_id": input.rule_id,
            "deleted_maps": deleted_maps,
            "runs_kept": true,
        }),
        "规则已删除，运行记录已保留",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_id_must_be_positive() {
        assert!(DeleteExportRuleInput { rule_id: 0 }.validate().is_err());
        assert!(DeleteExportRuleInput { rule_id: -1 }.validate().is_err());
        assert!(DeleteExportRuleInput { rule_id: 1 }.validate().is_ok());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(
            serde_json::from_value::<DeleteExportRuleInput>(serde_json::json!({
                "rule_id": 1, "cascade_runs": true
            }))
            .is_err()
        );
    }
}

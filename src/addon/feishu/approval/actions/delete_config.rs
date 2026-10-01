//! 删除审批派发配置（前端控制台）。
//!
//! # 同事务清理三件事
//!
//! 配置行 + 它的字段映射行 + **`state=pending` 的任务行**；backfilled / terminal
//! 任务行保留作流水（已完结的历史审批单要留在库里可查）。
//!
//! # 不加未完结守卫
//!
//! worker 对配置缺失本就是优雅的 terminal 降级（设计 §2.2-1/2）：认领侧读不到
//! 启用配置即把任务标 terminal，不重试、不坏数据。删除不需要等任务跑完。
//! 在途行此时必然仍为 `pending`（状态只在处理结束时写回），同事务清掉即可。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::infrastructure::audit;

/// 删除配置的输入契约。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteConfigInput {
    /// 目标配置的 `id`。
    pub(super) config_id: i64,
}

impl ParamInput for DeleteConfigInput {
    fn params() -> Params {
        Params::new()
    }
}

impl DeleteConfigInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.config_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "config_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        Ok(())
    }
}

/// 注册删除配置端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("delete_config"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/approval/configs/delete")
        .display_name("删除审批派发配置")
        .description("删除配置、它的字段映射与待处理任务；已完结任务保留作流水")
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: DeleteConfigInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    // 删配置、删映射、删 pending 任务、写审计：同一事务，缺一不可。
    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<serde_json::Value, BaseError> = async {
        let deleted_maps = context
            .approval_field_maps()
            .query()
            .where_eq("config_id", serde_json::json!(input.config_id))?
            .delete_in_tx(&mut transaction)
            .await?;
        let deleted_pending_tasks = context
            .approval_tasks()
            .query()
            .where_eq("config_id", serde_json::json!(input.config_id))?
            .where_eq("state", serde_json::json!("pending"))?
            .delete_in_tx(&mut transaction)
            .await?;
        let deleted_configs = context
            .approval_configs()
            .query()
            .where_eq("id", serde_json::json!(input.config_id))?
            .delete_in_tx(&mut transaction)
            .await?;
        if deleted_configs == 0 {
            return Err(BaseError::RecordNotFound("审批派发配置不存在".to_string()));
        }

        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_approval_config", input.config_id.to_string())?,
            None,
            Some(audit::summary([
                ("config_id", serde_json::json!(input.config_id)),
                ("deleted_field_maps", serde_json::json!(deleted_maps)),
                (
                    "deleted_pending_tasks",
                    serde_json::json!(deleted_pending_tasks),
                ),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;

        Ok(serde_json::json!({
            "config_id": input.config_id,
            "deleted_field_maps": deleted_maps,
            "deleted_pending_tasks": deleted_pending_tasks,
        }))
    }
    .await;
    let value = FeishuContext::finish_transaction(transaction, result).await?;

    ApiResponse::success(value, "配置已删除")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_config_id_is_rejected() {
        let input = DeleteConfigInput { config_id: 0 };
        assert!(input.validate().is_err());
    }

    #[test]
    fn positive_config_id_is_accepted() {
        let input = DeleteConfigInput { config_id: 42 };
        assert!(input.validate().is_ok());
    }
}

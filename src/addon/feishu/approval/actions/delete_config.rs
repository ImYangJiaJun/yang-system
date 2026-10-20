//! 删除审批派发配置（前端控制台）。
//!
//! # 同事务清理三件事
//!
//! 配置行 + 它的字段映射行 + **该配置的全部任务行**。任务表以全局唯一的
//! `record_id` 作并发去重键；若删除配置时保留已完结任务，重建配置后对同一记录
//! 重新派发会撞上旧的 `record_id` 唯一键，播种永久卡住（feishu-approval#31）。
//! 任务流水属于已删除的配置，因此这里一并清掉；配置的请求流水仍保留在
//! `feishu_approval_request_log`。
//!
//! # 不加未完结守卫
//!
//! worker 对配置缺失本就是优雅的终态降级（设计 §2.2-1/2）：认领侧读不到启用配置即
//! 把任务标 `unnotified`（那时配置行已不在，渲染不出展示名，通知根本走不到），
//! 不重试、不坏数据。删除不需要等任务跑完。在途行此时必然仍为 `pending`
//! （状态只在处理结束时写回），同事务清掉即可。

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
        .description("删除配置、它的字段映射与全部任务行；请求流水保留")
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
        // 先收掉任务的属性扩展行：task_applicant 没有外键，不主动清会留下
        // 指向已删除任务（乃至已删除配置）的孤儿 PII。
        let task_ids: Vec<i64> = context
            .approval_tasks()
            .query()
            .select_fields(&["id"])?
            .where_eq("config_id", serde_json::json!(input.config_id))?
            .all_in_tx(&mut transaction)
            .await?
            .iter()
            .map(|record| record.require("id"))
            .collect::<Result<_, BaseError>>()?;
        let deleted_applicants = if task_ids.is_empty() {
            0
        } else {
            let values: Vec<serde_json::Value> =
                task_ids.iter().map(|id| serde_json::json!(id)).collect();
            context
                .approval_task_applicants()
                .query()
                .where_in("task_id", values)?
                .delete_in_tx(&mut transaction)
                .await?
        };
        let deleted_pending_tasks = context
            .approval_tasks()
            .query()
            .where_eq("config_id", serde_json::json!(input.config_id))?
            .where_eq("state", serde_json::json!("pending"))?
            .delete_in_tx(&mut transaction)
            .await?;
        // 已完结任务也必须清：task 表的 record_id 是全局唯一键，保留旧配置的
        // 完结行会让重建配置后的同记录播种永久卡住（#31）。请求流水仍在
        // feishu_approval_request_log，不受影响。
        let deleted_finished_tasks = context
            .approval_tasks()
            .query()
            .where_eq("config_id", serde_json::json!(input.config_id))?
            .delete_in_tx(&mut transaction)
            .await?;
        let deleted_tasks = deleted_pending_tasks + deleted_finished_tasks;
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
                ("deleted_tasks", serde_json::json!(deleted_tasks)),
                ("deleted_applicants", serde_json::json!(deleted_applicants)),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;

        Ok(serde_json::json!({
            "config_id": input.config_id,
            "deleted_field_maps": deleted_maps,
            "deleted_pending_tasks": deleted_pending_tasks,
            "deleted_tasks": deleted_tasks,
            "deleted_applicants": deleted_applicants,
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

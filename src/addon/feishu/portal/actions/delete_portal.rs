//! 删除查询门户：连同它的访问日志一起收掉。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::infrastructure::audit;

/// 删除门户的输入契约。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct DeletePortalInput {
    /// 目标门户 `id`。
    pub(super) portal_id: i64,
}

impl ParamInput for DeletePortalInput {
    fn params() -> Params {
        Params::new()
    }
}

impl DeletePortalInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.portal_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "portal_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        Ok(())
    }
}

/// 注册删除门户端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("delete_portal"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Delete, "/api/v1/feishu/portals")
        .display_name("删除查询门户")
        .description("删除门户及其访问日志")
        .permissions(["feishu.portal.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: DeletePortalInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<u64, BaseError> = async {
        // 先删日志再删门户：中途失败时留在库里的是一份**可归因**的状态
        // （门户还在、日志少了一段），而不是一批指向不存在门户的孤儿日志。
        // 日志表是 append-only 的审计事实，门户一删它的归属就再也解释不了。
        let deleted_logs = context
            .portal_logs()
            .query()
            .where_eq("portal_id", serde_json::json!(input.portal_id))?
            .delete_in_tx(&mut transaction)
            .await?;

        let deleted = context
            .portals()
            .query()
            .where_eq("id", serde_json::json!(input.portal_id))?
            .delete_in_tx(&mut transaction)
            .await?;
        if deleted == 0 {
            return Err(BaseError::RecordNotFound("门户不存在".to_string()));
        }

        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_datasource_portal", input.portal_id)?,
            None,
            Some(audit::summary([
                ("outcome_code", serde_json::json!("deleted_portal")),
                ("deleted_logs", serde_json::json!(deleted_logs)),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;

        Ok(deleted_logs)
    }
    .await;

    let deleted_logs = FeishuContext::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        serde_json::json!({
            "portal_id": input.portal_id,
            "deleted_logs": deleted_logs,
        }),
        "删除成功",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_non_positive_portal_id() {
        assert!(DeletePortalInput { portal_id: 0 }.validate().is_err());
        assert!(DeletePortalInput { portal_id: -1 }.validate().is_err());
    }

    #[test]
    fn accepts_a_positive_portal_id() {
        assert!(DeletePortalInput { portal_id: 42 }.validate().is_ok());
    }
}

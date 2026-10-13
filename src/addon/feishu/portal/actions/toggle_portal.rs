//! 启用/停用查询门户。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::infrastructure::audit;

/// 启停门户的输入契约。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct TogglePortalInput {
    /// 目标门户 `id`。
    pub(super) portal_id: i64,
    /// `true` 启用，`false` 停用。
    pub(super) enabled: bool,
}

impl ParamInput for TogglePortalInput {
    fn params() -> Params {
        Params::new()
    }
}

impl TogglePortalInput {
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

/// 注册启停门户端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("toggle_portal"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Put, "/api/v1/feishu/portals/toggle")
        .display_name("启停查询门户")
        .description("启用或停用门户；停用后公开 URL 返回未找到，配置与日志保留")
        .permissions(["feishu.portal.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: TogglePortalInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<(), BaseError> = async {
        let mut record = Record::new();
        record.insert("enabled", serde_json::json!(input.enabled));

        let affected = context
            .portals()
            .query()
            .where_eq("id", serde_json::json!(input.portal_id))?
            .update_in_tx(&mut transaction, record)
            .await?;
        if affected == 0 {
            return Err(BaseError::RecordNotFound("门户不存在".to_string()));
        }

        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_datasource_portal", input.portal_id)?,
            None,
            Some(audit::summary([
                (
                    "outcome_code",
                    serde_json::json!(if input.enabled {
                        "enabled_portal"
                    } else {
                        "disabled_portal"
                    }),
                ),
                ("enabled", serde_json::json!(input.enabled)),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;

    FeishuContext::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        serde_json::json!({
            "portal_id": input.portal_id,
            "enabled": input.enabled,
        }),
        if input.enabled {
            "已启用"
        } else {
            "已停用"
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_non_positive_portal_id() {
        let input = TogglePortalInput {
            portal_id: 0,
            enabled: true,
        };
        assert!(input.validate().is_err());
    }

    #[test]
    fn accepts_both_states() {
        for enabled in [true, false] {
            let input = TogglePortalInput {
                portal_id: 1,
                enabled,
            };
            assert!(input.validate().is_ok());
        }
    }
}

//! 解绑当前用户的飞书身份：先删凭证，再删绑定。

use std::sync::Arc;

use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use super::super::{credentials, require_current_user};
use crate::addon::account::{ExternalLoginPort, PROVIDER_FEISHU};
use crate::addon::feishu::domain::context::FeishuContext;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct UnbindInput {
    confirm: String,
    /// 当前账号密码，用作 Step-up。
    password: String,
}

impl ParamInput for UnbindInput {
    fn params() -> Params {
        Params::new()
    }
}

pub(super) fn register(
    module: ModuleSpec,
    context: Arc<FeishuContext>,
    external_login: Arc<dyn ExternalLoginPort>,
) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("feishu_unbind"),
            move |ctx, input| {
                handle(
                    ctx,
                    input,
                    Arc::clone(&context),
                    Arc::clone(&external_login),
                )
            },
        )
        .route(HttpMethod::Delete, "/api/v1/users/me/identities/feishu")
        .display_name("解绑飞书身份")
        .description("删除当前用户的飞书凭证与身份绑定")
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: UnbindInput,
    context: Arc<FeishuContext>,
    external_login: Arc<dyn ExternalLoginPort>,
) -> Result<ApiResponse, BaseError> {
    if input.confirm != "true" {
        return Err(BaseError::ParamInvalid(
            "confirm".to_string(),
            "请先确认解绑飞书身份".to_string(),
        ));
    }
    let user_id = require_current_user(&ctx)?;
    let identities = external_login.list_identities(&ctx, user_id).await?;
    let Some(identity) = identities
        .into_iter()
        .find(|identity| identity.provider == PROVIDER_FEISHU)
    else {
        return ApiResponse::success(serde_json::json!({ "unbound": true }), "未绑定飞书身份");
    };
    if input.password.is_empty() {
        return Err(BaseError::ParamInvalid(
            "password".to_string(),
            "请输入当前账号密码以确认身份".to_string(),
        ));
    }
    external_login
        .verify_current_password(&ctx, user_id, &input.password)
        .await?;
    let cancelled_jobs =
        crate::addon::feishu::batch_approval::cancel_user_jobs(&context, user_id).await?;
    let _ = credentials::delete_credential(&ctx, &context, identity.id).await?;
    let affected = external_login
        .unbind_identity(&ctx, user_id, PROVIDER_FEISHU, &identity.tenant_key)
        .await?;
    // 解绑已完成，审计写失败只降级为日志，不能把成功操作报成 500。
    if let Ok(event) = crate::audit::succeeded_event(
        &ctx,
        None,
        Some(crate::audit::entity("user", user_id)?),
        crate::audit::entity("user_identity", identity.id)?,
        Some(crate::audit::summary([(
            "provider",
            serde_json::json!(PROVIDER_FEISHU),
        )])?),
        Some(crate::audit::summary([
            ("unbound", serde_json::json!(true)),
            ("cancelled_jobs", serde_json::json!(cancelled_jobs)),
        ])?),
    ) {
        if let Ok(database) = ctx.tools().mysql() {
            if let Err(error) = crate::audit::append_independent(database.pool(), &event).await {
                tracing::warn!(error = %error, user_id, "写飞书解绑审计失败");
            }
        }
    }
    ApiResponse::success(
        serde_json::json!({
            "unbound": true,
            "affected": affected,
            "cancelled_jobs": cancelled_jobs,
        }),
        "已解绑飞书身份",
    )
}

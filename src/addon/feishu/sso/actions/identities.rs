//! 当前用户的外部身份绑定列表（账号设置「飞书账号」区块）。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Serialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use super::super::{mask_email, require_current_user};
use crate::addon::account::ExternalLoginPort;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct IdentitiesInput {}

impl ParamInput for IdentitiesInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct IdentityView {
    provider: String,
    name: Option<String>,
    email_masked: Option<String>,
    bound_at: i64,
    last_login_at: Option<i64>,
}

pub(super) fn register(
    module: ModuleSpec,
    external_login: Arc<dyn ExternalLoginPort>,
) -> ModuleSpec {
    // auth: authenticated-only 自服务操作——登录用户只能访问自己的飞书身份与批量审批数据
    module
        .action_fn(
            yang_base::action_name!("feishu_identities"),
            move |ctx, input| handle(ctx, input, Arc::clone(&external_login)),
        )
        .route(HttpMethod::Get, "/api/v1/users/me/identities")
        .display_name("外部身份绑定")
        .description("列出当前用户已绑定的外部身份")
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    _input: IdentitiesInput,
    external_login: Arc<dyn ExternalLoginPort>,
) -> Result<ApiResponse, BaseError> {
    let user_id = require_current_user(&ctx)?;
    let identities = external_login.list_identities(&ctx, user_id).await?;
    let views: Vec<IdentityView> = identities
        .into_iter()
        .map(|identity| IdentityView {
            provider: identity.provider,
            name: identity.name,
            email_masked: mask_email(identity.email.as_deref()),
            bound_at: identity.bound_at,
            last_login_at: identity.last_login_at,
        })
        .collect();
    ApiResponse::success(views, "ok")
}

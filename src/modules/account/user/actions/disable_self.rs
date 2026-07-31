use super::super::service::UserService;
use async_trait::async_trait;
use serde::Serialize;
use std::sync::Arc;
use yang_base::action::{Action as BusinessAction, ActionContext, ApiResponse};
use yang_base::definition::{ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct DisableSelfInput {}

impl ParamInput for DisableSelfInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(yang_base::Action)]
#[action(
    name = "disable_self",
    display_name = "停用当前账号",
    description = "停用当前账号及全部平台/企业关系，并撤销此前签发的全部会话",
    method = "POST",
    path = "/api/v1/users/disable"
)]
struct DisableSelfAction {
    service: Arc<UserService>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct DisableSelfResult {
    account_disabled: bool,
    immediate_convergence: bool,
    relogin_required: bool,
}

#[async_trait]
impl BusinessAction for DisableSelfAction {
    type Input = DisableSelfInput;
    type Output = ApiResponse;

    async fn index(
        &self,
        ctx: ActionContext,
        _input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let secure = super::super::browser_session::validate_same_origin(&ctx.request)?;
        let user_id = ctx.actor()?.user_id();
        let immediate_convergence = self.service.disable_self(&ctx, user_id).await?;
        super::super::browser_session::clear_response(
            ApiResponse::success(
                DisableSelfResult {
                    account_disabled: true,
                    immediate_convergence,
                    relogin_required: true,
                },
                if immediate_convergence {
                    "账号已停用，全部会话已撤销"
                } else {
                    "账号已停用，Redis 即时收敛待后台重试"
                },
            )?,
            secure,
        )
    }
}

pub(super) fn register(
    module: ModuleSpec,
    service: Arc<UserService>,
) -> Result<ModuleSpec, BaseError> {
    Ok(module.native_action(DisableSelfAction { service }))
}

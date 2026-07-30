use async_trait::async_trait;
use yang_base::action::auth::{LogoutAction, LogoutInput};
use yang_base::action::{Action as BusinessAction, ActionContext, ApiResponse, TypedHandler};
use yang_base::definition::{ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BrowserLogoutInput {}

impl ParamInput for BrowserLogoutInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(yang_base::Action)]
#[action(
    name = "logout",
    display_name = "退出登录",
    description = "撤销当前 Token",
    method = "POST",
    path = "/api/v1/users/logout",
    public
)]
struct BrowserLogoutAction {
    inner: LogoutAction,
}

#[async_trait]
impl BusinessAction for BrowserLogoutAction {
    type Input = BrowserLogoutInput;
    type Output = ApiResponse;

    async fn index(
        &self,
        ctx: ActionContext,
        _input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let secure = super::super::browser_session::validate_same_origin(&ctx.request)?;
        let refresh_token = super::super::browser_session::refresh_token(&ctx.request)?;
        let target = ctx
            .request
            .token()
            .map(str::to_owned)
            .unwrap_or_else(|| refresh_token.clone());
        let result = self
            .inner
            .handle(
                ctx,
                LogoutInput {
                    token: target,
                    refresh_token: Some(refresh_token),
                },
            )
            .await?;
        super::super::browser_session::clear_response(
            ApiResponse::success(result, "已登出")?,
            secure,
        )
    }
}

pub(super) fn register(module: ModuleSpec) -> Result<ModuleSpec, BaseError> {
    Ok(module.native_action(BrowserLogoutAction {
        inner: LogoutAction::new(),
    }))
}

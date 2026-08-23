use super::super::service::UserService;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::auth::{
    BrowserSession, CredentialVerifier, LoginAction, LoginInput, VerifiedSubject,
};
use yang_base::action::{Action as BusinessAction, ActionContext, ApiResponse, TypedHandler};
use yang_base::definition::{ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

#[derive(Clone)]
struct UserCredentialVerifier {
    service: Arc<UserService>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct BrowserLoginInput {
    /// 用户名、邮箱或其他登录标识。
    username: String,
    /// 登录凭据。
    password: String,
    /// 可选业务扩展字段。
    #[serde(default)]
    extra: serde_json::Value,
}

impl ParamInput for BrowserLoginInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(yang_base::Action)]
#[action(
    name = "login",
    display_name = "登录",
    description = "校验账号密码并签发 Token",
    method = "POST",
    path = "/api/v1/users/login",
    public
)]
struct BrowserLoginAction {
    inner: LoginAction<UserCredentialVerifier>,
}

#[async_trait]
impl BusinessAction for BrowserLoginAction {
    type Input = BrowserLoginInput;
    type Output = ApiResponse;

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let secure = BrowserSession::validate_same_origin(&ctx.request)?;
        let tokens = self
            .inner
            .handle(
                ctx,
                LoginInput {
                    username: input.username,
                    password: input.password,
                    extra: input.extra,
                },
            )
            .await?;
        super::super::browser_session().token_response(
            tokens.access_token,
            tokens.refresh_token,
            secure,
        )
    }
}

#[async_trait]
impl CredentialVerifier for UserCredentialVerifier {
    async fn verify(
        &self,
        ctx: &ActionContext,
        input: &LoginInput,
    ) -> Result<VerifiedSubject, BaseError> {
        let user = self
            .service
            .authenticate(ctx, &input.username, &input.password)
            .await?;
        let claims = self.service.claims_for(ctx, user.id).await?;
        Ok(VerifiedSubject::new(user.id.to_string()).with_token_pair_claims(claims))
    }
}

pub(super) fn register(
    module: ModuleSpec,
    service: Arc<UserService>,
) -> Result<ModuleSpec, BaseError> {
    Ok(module.native_action(BrowserLoginAction {
        inner: LoginAction::new(UserCredentialVerifier { service }),
    }))
}

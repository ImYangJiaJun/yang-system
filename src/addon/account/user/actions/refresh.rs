use super::super::service::UserService;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::auth::{
    BrowserSession, RefreshAction, RefreshClaimsResolver, RefreshInput, TokenPairClaims,
};
use yang_base::action::{Action as BusinessAction, ActionContext, ApiResponse, TypedHandler};
use yang_base::definition::{ModuleSpec, ParamInput, Params};
use yang_base::token::TokenClaims;
use yang_base::BaseError;

#[derive(Clone)]
struct UserClaimsResolver {
    service: Arc<UserService>,
}

#[async_trait]
impl RefreshClaimsResolver for UserClaimsResolver {
    async fn resolve(
        &self,
        ctx: &ActionContext,
        subject: &str,
    ) -> Result<serde_json::Value, BaseError> {
        Ok(self.service.claims_for_subject(ctx, subject).await?.access)
    }

    async fn resolve_pair(
        &self,
        ctx: &ActionContext,
        subject: &str,
    ) -> Result<TokenPairClaims, BaseError> {
        self.service.claims_for_subject(ctx, subject).await
    }

    async fn resolve_pair_from_claims(
        &self,
        ctx: &ActionContext,
        old_claims: &TokenClaims,
    ) -> Result<TokenPairClaims, BaseError> {
        self.service.claims_for_refresh(ctx, old_claims).await
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BrowserRefreshInput {}

impl ParamInput for BrowserRefreshInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(yang_base::Action)]
#[action(
    name = "refresh",
    display_name = "刷新 Token",
    description = "轮换 Refresh Token 并签发新 Token 对",
    method = "POST",
    path = "/api/v1/users/refresh",
    public
)]
struct BrowserRefreshAction {
    inner: RefreshAction<UserClaimsResolver>,
}

#[async_trait]
impl BusinessAction for BrowserRefreshAction {
    type Input = BrowserRefreshInput;
    type Output = ApiResponse;

    async fn index(
        &self,
        ctx: ActionContext,
        _input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let secure = BrowserSession::validate_same_origin(&ctx.request)?;
        let refresh_token = super::super::browser_session().refresh_token(&ctx.request)?;
        let tokens = self
            .inner
            .handle(ctx, RefreshInput { refresh_token })
            .await?;
        super::super::browser_session().token_response(
            tokens.access_token,
            tokens.refresh_token,
            secure,
        )
    }
}

pub(super) fn register(
    module: ModuleSpec,
    service: Arc<UserService>,
) -> Result<ModuleSpec, BaseError> {
    Ok(module.native_action(BrowserRefreshAction {
        inner: RefreshAction::new(UserClaimsResolver { service }),
    }))
}

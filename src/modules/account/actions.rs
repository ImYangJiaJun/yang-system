use super::dto::{AccountView, EmptyInput, RegisterInput};
use super::service::AccountService;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::auth::{
    CredentialVerifier, LoginInput, RefreshClaimsResolver, VerifiedSubject,
};
use yang_base::action::{ActionContext, TypedHandler};
use yang_base::{Action, BaseError};

#[derive(Action)]
#[action(
    name = "register",
    display_name = "注册账号",
    description = "创建一个新账号",
    public
)]
pub(super) struct RegisterAction {
    service: Arc<AccountService>,
}

impl RegisterAction {
    pub(super) fn new(service: Arc<AccountService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl TypedHandler for RegisterAction {
    type Input = RegisterInput;
    type Output = AccountView;

    async fn handle(
        &self,
        _ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        self.service
            .register(&input.username, &input.password)
            .await
    }
}

#[derive(Clone)]
pub(super) struct AccountVerifier {
    service: Arc<AccountService>,
}

impl AccountVerifier {
    pub(super) fn new(service: Arc<AccountService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl CredentialVerifier for AccountVerifier {
    async fn verify(
        &self,
        _ctx: &ActionContext,
        input: &LoginInput,
    ) -> Result<VerifiedSubject, BaseError> {
        let account = self
            .service
            .authenticate(&input.username, &input.password)
            .await?;
        Ok(
            VerifiedSubject::new(account.id.to_string()).with_claims(serde_json::json!({
                "username": account.username,
                "roles": ["user"]
            })),
        )
    }
}

#[derive(Clone)]
pub(super) struct AccountClaimsResolver {
    service: Arc<AccountService>,
}

impl AccountClaimsResolver {
    pub(super) fn new(service: Arc<AccountService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl RefreshClaimsResolver for AccountClaimsResolver {
    async fn resolve(
        &self,
        _ctx: &ActionContext,
        subject: &str,
    ) -> Result<serde_json::Value, BaseError> {
        let account = self.service.active_by_subject(subject).await?;
        Ok(serde_json::json!({
            "username": account.username,
            "roles": ["user"]
        }))
    }
}

#[derive(Action)]
#[action(
    name = "me",
    display_name = "当前账号",
    description = "读取当前已认证账号"
)]
pub(super) struct MeAction {
    service: Arc<AccountService>,
}

impl MeAction {
    pub(super) fn new(service: Arc<AccountService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl TypedHandler for MeAction {
    type Input = EmptyInput;
    type Output = AccountView;

    async fn handle(
        &self,
        ctx: ActionContext,
        _input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let id = ctx
            .authenticated_user()
            .ok_or_else(|| BaseError::Unauthorized("需要登录".to_string()))?
            .id;
        self.service.view_by_id(id).await
    }
}

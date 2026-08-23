//! 框架内置认证 Action 的领域适配器。
//!
//! Login/Refresh/Step-up 由 `yang_base::action::auth` 的内置 Action 承载，
//! 这里只提供把账号领域服务接到框架端口的薄适配器。

use super::service::UserService;
use async_trait::async_trait;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use yang_base::action::auth::{
    CredentialVerifier, LoginInput, RefreshClaimsResolver, TokenPairClaims, VerifiedSubject,
};
use yang_base::action::ActionContext;
use yang_base::token::TokenClaims;
use yang_base::BaseError;

/// 把用户登录认证接到内置 `LoginAction` 的凭据校验端口。
#[derive(Clone)]
pub(in crate::addon::account::user) struct UserCredentialVerifier {
    service: Arc<UserService>,
}

impl UserCredentialVerifier {
    pub(in crate::addon::account::user) fn new(service: Arc<UserService>) -> Self {
        Self { service }
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

/// 把用户授权快照接到内置 `RefreshAction` 的声明解析端口。
#[derive(Clone)]
pub(in crate::addon::account::user) struct UserClaimsResolver {
    service: Arc<UserService>,
}

impl UserClaimsResolver {
    pub(in crate::addon::account::user) fn new(service: Arc<UserService>) -> Self {
        Self { service }
    }
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

/// 把 Step-up 重认证接到框架 challenge 完成端口，并记录已验证用户供审计使用。
#[derive(Clone)]
pub(in crate::addon::account::user) struct UserStepUpCredentialVerifier {
    service: Arc<UserService>,
    verified_user_id: Arc<AtomicI64>,
}

impl UserStepUpCredentialVerifier {
    pub(in crate::addon::account::user) fn new(
        service: Arc<UserService>,
        verified_user_id: Arc<AtomicI64>,
    ) -> Self {
        Self {
            service,
            verified_user_id,
        }
    }
}

#[async_trait]
impl CredentialVerifier for UserStepUpCredentialVerifier {
    async fn verify(
        &self,
        ctx: &ActionContext,
        input: &LoginInput,
    ) -> Result<VerifiedSubject, BaseError> {
        let user = self
            .service
            .authenticate_step_up(ctx, &input.username, &input.password)
            .await?;
        self.verified_user_id.store(user.id, Ordering::Release);
        Ok(VerifiedSubject::new(user.id.to_string()))
    }
}

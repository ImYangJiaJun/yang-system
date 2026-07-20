//! 用户领域共享服务。
//!
//! Action 只负责传输适配；注册、登录、刷新和当前用户用例在此集中。

use super::password::PasswordEngine;
use super::policy::{normalize_username, validate_password};
use super::rate_limit::{AuthOperation, AuthRateLimiter};
use super::repository::UserRepository;
use super::schema::{UserView, STATUS, USERNAME};
use crate::modules::account::{AuthorizationGrants, GrantResolver};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::table::Record;
use yang_base::BaseError;

pub(super) struct AuthenticatedUser {
    pub(super) id: i64,
    pub(super) username: String,
}

#[derive(Clone)]
pub(super) struct UserService {
    users: Arc<UserRepository>,
    passwords: Arc<PasswordEngine>,
    rate_limiter: Arc<AuthRateLimiter>,
    grant_resolver: Arc<dyn GrantResolver>,
}

impl UserService {
    pub(super) fn new(
        users: Arc<UserRepository>,
        passwords: Arc<PasswordEngine>,
        rate_limiter: Arc<AuthRateLimiter>,
        grant_resolver: Arc<dyn GrantResolver>,
    ) -> Self {
        Self {
            users,
            passwords,
            rate_limiter,
            grant_resolver,
        }
    }

    pub(super) async fn register(
        &self,
        ctx: &ActionContext,
        username: &str,
        plain_password: &str,
    ) -> Result<UserView, BaseError> {
        let username = normalize_username(username)?;
        validate_password(plain_password)?;
        self.rate_limiter
            .check(ctx, AuthOperation::Register, &username)
            .await?;
        if self.users.username_exists(ctx, &username).await? {
            return Err(username_exists_error());
        }
        let password_hash = self.passwords.hash(plain_password).await?;
        let id = match self.users.insert(ctx, &username, &password_hash).await {
            Ok(id) => id,
            Err(BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_))) => {
                return Err(username_exists_error());
            }
            Err(error) => return Err(error),
        };
        self.view_by_id(ctx, id).await
    }

    pub(super) async fn authenticate(
        &self,
        ctx: &ActionContext,
        username: &str,
        plain_password: &str,
    ) -> Result<AuthenticatedUser, BaseError> {
        let username = normalize_username(username)?;
        self.rate_limiter
            .check(ctx, AuthOperation::Login, &username)
            .await?;
        let user = self
            .users
            .find_credentials_by_username(ctx, &username)
            .await?
            .ok_or(BaseError::InvalidPassword)?;
        if !self
            .passwords
            .verify(plain_password, &user.password_hash)
            .await?
        {
            return Err(BaseError::InvalidPassword);
        }
        ensure_active_status(&user.status)?;
        Ok(AuthenticatedUser {
            id: user.id,
            username: user.username,
        })
    }

    pub(super) async fn active_user_by_subject(
        &self,
        ctx: &ActionContext,
        subject: &str,
    ) -> Result<AuthenticatedUser, BaseError> {
        let id = subject
            .parse::<i64>()
            .map_err(|_| BaseError::Unauthorized("Token subject 无效".to_string()))?;
        let user = self.active_record_by_id(ctx, id).await?;
        Ok(AuthenticatedUser {
            id,
            username: user.require(USERNAME)?,
        })
    }

    pub(super) async fn claims_for(
        &self,
        ctx: &ActionContext,
        user: &AuthenticatedUser,
    ) -> Result<serde_json::Value, BaseError> {
        let grants =
            AuthorizationGrants::user().extend(self.grant_resolver.resolve(ctx, user.id).await?);
        super::claims::claims_for_user(&user.username, &grants)
    }

    pub(super) async fn view_by_id(
        &self,
        ctx: &ActionContext,
        id: i64,
    ) -> Result<UserView, BaseError> {
        let user = self.active_record_by_id(ctx, id).await?;
        UserView::try_from(&user)
    }

    async fn active_record_by_id(&self, ctx: &ActionContext, id: i64) -> Result<Record, BaseError> {
        let user = self
            .users
            .find_by_id(ctx, id)
            .await?
            .ok_or_else(|| BaseError::UserNotFound(id.to_string()))?;
        let status: String = user.require(STATUS)?;
        ensure_active_status(&status)?;
        Ok(user)
    }
}

fn ensure_active_status(status: &str) -> Result<(), BaseError> {
    if status != "active" {
        return Err(BaseError::Unauthorized("用户已停用".to_string()));
    }
    Ok(())
}

fn username_exists_error() -> BaseError {
    BaseError::ParamInvalid("username".to_string(), "用户名已存在".to_string())
}

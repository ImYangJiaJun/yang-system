//! 用户领域共享服务。
//!
//! Action 只负责传输适配；注册、登录、刷新和当前用户用例在此集中。

use super::password::PasswordEngine;
use super::policy::{normalize_username, validate_new_password, validate_password};
use super::rate_limit::{AuthOperation, AuthRateLimiter};
use super::repository::{AuthorizationStateRecord, UserRepository};
use super::schema::{UserView, STATUS};
use crate::audit;
use crate::modules::account::{
    increment_locked_credential_versions, lock_user_credential, AuthorizationGrants, GrantResolver,
    LockedUserCredential,
};
use serde_json::json;
use std::sync::Arc;
use yang_base::action::auth::TokenPairClaims;
use yang_base::action::ActionContext;
use yang_base::table::Record;
use yang_base::token::TokenClaims;
use yang_base::BaseError;

pub(super) struct AuthenticatedUser {
    pub(super) id: i64,
}

struct AuthorizationSnapshot {
    username: String,
    authz_version: i64,
    credential_version: i64,
    grants: AuthorizationGrants,
}

#[derive(Clone)]
pub(super) struct UserService {
    users: Arc<UserRepository>,
    passwords: Arc<PasswordEngine>,
    rate_limiter: Arc<AuthRateLimiter>,
    grant_resolver: Arc<dyn GrantResolver>,
    issue_refresh_credential_version: bool,
}

impl UserService {
    pub(super) fn new(
        users: Arc<UserRepository>,
        passwords: Arc<PasswordEngine>,
        rate_limiter: Arc<AuthRateLimiter>,
        grant_resolver: Arc<dyn GrantResolver>,
        issue_refresh_credential_version: bool,
    ) -> Self {
        Self {
            users,
            passwords,
            rate_limiter,
            grant_resolver,
            issue_refresh_credential_version,
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
        Ok(AuthenticatedUser { id: user.id })
    }

    pub(super) async fn change_password(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        old_password: &str,
        new_password: &str,
    ) -> Result<(), BaseError> {
        if !self.issue_refresh_credential_version {
            return Err(BaseError::ConfigError(
                "改密能力必须在全部实例开启 Refresh 凭据版本签发后启用".to_string(),
            ));
        }
        validate_new_password(new_password)?;
        self.rate_limiter
            .check(ctx, AuthOperation::ChangePassword, &user_id.to_string())
            .await?;

        let observed = self
            .users
            .find_credentials_by_id(ctx, user_id)
            .await?
            .ok_or_else(|| BaseError::UserNotFound(user_id.to_string()))?;
        ensure_active_status(&observed.status)?;
        if !self
            .passwords
            .verify(old_password, &observed.password_hash)
            .await?
        {
            return Err(BaseError::InvalidPassword);
        }
        // 两次昂贵 Argon2 运算均在事务和用户行锁之外完成。
        let new_password_hash = self.passwords.hash(new_password).await?;

        let mut transaction = ctx.tools().mysql()?.transaction().await?;
        let result = async {
            let locked = lock_user_credential(&mut transaction, user_id).await?;
            ensure_active_status(locked.status())?;
            ensure_password_hash_unchanged(&locked, &observed.password_hash)?;
            self.users
                .update_password_hash_in_tx(ctx, &mut transaction, user_id, &new_password_hash)
                .await?;
            increment_locked_credential_versions(&mut transaction, &locked).await?;
            let event = audit::succeeded_event(
                ctx,
                None,
                Some(audit::entity("user", user_id)?),
                audit::entity("user", user_id)?,
                None,
                Some(audit::summary([("relogin_required", json!(true))])?),
            )?;
            audit::append_in_tx(&mut transaction, &event).await?;
            Ok(())
        }
        .await;
        finish_transaction(transaction, result).await
    }

    pub(super) async fn claims_for(
        &self,
        ctx: &ActionContext,
        user_id: i64,
    ) -> Result<TokenPairClaims, BaseError> {
        let snapshot = self.authorization_snapshot(ctx, user_id).await?;
        self.claims_from_snapshot(&snapshot)
    }

    pub(super) async fn claims_for_subject(
        &self,
        ctx: &ActionContext,
        subject: &str,
    ) -> Result<TokenPairClaims, BaseError> {
        let user_id = subject
            .parse::<i64>()
            .map_err(|_| BaseError::Unauthorized("Token subject 无效".to_string()))?;
        self.claims_for(ctx, user_id).await
    }

    pub(super) async fn claims_for_refresh(
        &self,
        ctx: &ActionContext,
        old_claims: &TokenClaims,
    ) -> Result<TokenPairClaims, BaseError> {
        let user_id = old_claims
            .sub
            .parse::<i64>()
            .map_err(|_| BaseError::Unauthorized("Token subject 无效".to_string()))?;
        let snapshot = self.authorization_snapshot(ctx, user_id).await?;
        super::claims::validate_refresh_credential_version(
            old_claims,
            snapshot.credential_version,
        )?;
        self.claims_from_snapshot(&snapshot)
    }

    fn claims_from_snapshot(
        &self,
        snapshot: &AuthorizationSnapshot,
    ) -> Result<TokenPairClaims, BaseError> {
        super::claims::claims_for_user(
            &snapshot.username,
            snapshot.authz_version,
            snapshot.credential_version,
            self.issue_refresh_credential_version,
            &snapshot.grants,
        )
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

    async fn authorization_snapshot(
        &self,
        ctx: &ActionContext,
        user_id: i64,
    ) -> Result<AuthorizationSnapshot, BaseError> {
        let mut transaction = ctx
            .tools()
            .mysql()?
            .read_only_transaction()
            .await
            .map_err(BaseError::from)?;
        let snapshot_result: Result<AuthorizationSnapshot, BaseError> = async {
            let state = self
                .users
                .find_authorization_state_in_tx(ctx, &mut transaction, user_id)
                .await?
                .ok_or_else(|| BaseError::UserNotFound(user_id.to_string()))?;
            validate_authorization_state(&state)?;
            let grants = AuthorizationGrants::user().extend(
                self.grant_resolver
                    .resolve(ctx, user_id, &mut transaction)
                    .await?,
            );
            Ok(AuthorizationSnapshot {
                username: state.username,
                authz_version: state.authz_version,
                credential_version: state.credential_version,
                grants,
            })
        }
        .await;

        match snapshot_result {
            Ok(snapshot) => {
                transaction.commit().await.map_err(BaseError::from)?;
                Ok(snapshot)
            }
            Err(error) => {
                if let Err(rollback_error) = transaction.rollback().await {
                    tracing::error!(
                        "授权快照失败后回滚事务也失败: user_id={}, error={}",
                        user_id,
                        rollback_error
                    );
                }
                Err(error)
            }
        }
    }
}

fn validate_authorization_state(state: &AuthorizationStateRecord) -> Result<(), BaseError> {
    ensure_active_status(&state.status)?;
    if state.authz_version < 1 {
        return Err(BaseError::Unauthorized("用户授权版本无效".to_string()));
    }
    if state.credential_version < 0 {
        return Err(BaseError::Unauthorized("用户凭据版本无效".to_string()));
    }
    Ok(())
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

fn ensure_password_hash_unchanged(
    locked: &LockedUserCredential,
    observed_password_hash: &str,
) -> Result<(), BaseError> {
    if locked.password_hash_matches(observed_password_hash) {
        return Ok(());
    }
    Err(BaseError::ParamInvalid(
        "old_password".to_string(),
        "密码已被其他请求修改，请重新登录后重试".to_string(),
    ))
}

async fn finish_transaction<T>(
    transaction: yang_db::Transaction,
    result: Result<T, BaseError>,
) -> Result<T, BaseError> {
    match result {
        Ok(value) => {
            transaction.commit().await.map_err(BaseError::from)?;
            Ok(value)
        }
        Err(error) => {
            if let Err(rollback_error) = transaction.rollback().await {
                tracing::error!("用户凭据事务回滚失败: error={}", rollback_error);
            }
            Err(error)
        }
    }
}

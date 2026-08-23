//! 用户领域共享服务。
//!
//! Action 只负责传输适配；注册、登录、刷新和当前用户用例在此集中。

use super::policy::{normalize_username, validate_new_password, validate_password};
use super::repository::{AuthorizationStateRecord, UserRepository};
use super::schema::{UserView, STATUS};
use super::status::UserStatus;
use crate::addon::account::{
    consume_password_reset_in_tx, find_password_reset_target_user,
    increment_locked_credential_versions, invalid_reset_token, lock_password_reset_in_tx,
    lock_user_credential, AuthorizationGrants, GrantResolver, LockedUserCredential,
    OwnerClaimOutcome, PasswordResetReference, SystemOwnerClaimer,
};
use crate::audit;
use serde_json::json;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use yang_base::action::auth::{
    normalize_email, AuthOperation, AuthRateLimiter, PasswordEngine, RegistrationEmailCodeAccepted,
    RegistrationEmailVerification, TokenPairClaims,
};
use yang_base::action::ActionContext;
use yang_base::table::Record;
use yang_base::token::TokenClaims;
use yang_base::BaseError;

pub(in crate::addon::account::user) struct AuthenticatedUser {
    pub(in crate::addon::account::user) id: i64,
}

struct AuthorizationSnapshot {
    username: String,
    authz_version: i64,
    credential_version: i64,
    grants: AuthorizationGrants,
}

#[derive(Clone)]
pub(in crate::addon::account::user) struct UserService {
    users: Arc<UserRepository>,
    passwords: Arc<PasswordEngine>,
    rate_limiter: Arc<AuthRateLimiter>,
    grant_resolver: Arc<dyn GrantResolver>,
    system_owner_claimer: Arc<dyn SystemOwnerClaimer>,
    issue_refresh_credential_version: bool,
}

impl UserService {
    pub(in crate::addon::account::user) fn new(
        users: Arc<UserRepository>,
        passwords: Arc<PasswordEngine>,
        rate_limiter: Arc<AuthRateLimiter>,
        grant_resolver: Arc<dyn GrantResolver>,
        system_owner_claimer: Arc<dyn SystemOwnerClaimer>,
        issue_refresh_credential_version: bool,
    ) -> Self {
        Self {
            users,
            passwords,
            rate_limiter,
            grant_resolver,
            system_owner_claimer,
            issue_refresh_credential_version,
        }
    }

    pub(in crate::addon::account::user) async fn register(
        &self,
        ctx: &ActionContext,
        username: &str,
        plain_password: &str,
        email: &str,
        email_code: &str,
    ) -> Result<UserView, BaseError> {
        let username = normalize_username(username)?;
        let email = normalize_email(email)?;
        validate_password(plain_password)?;
        self.rate_limiter
            .check(ctx, AuthOperation::Register, &username)
            .await?;
        if self.users.username_exists(ctx, &username).await? {
            return Err(username_exists_error());
        }
        RegistrationEmailVerification::from_context(ctx)?
            .consume(ctx, &email, email_code)
            .await?;
        let password_hash = self.passwords.hash(plain_password).await?;
        let email_verified_at = current_unix_timestamp()?;
        let mut transaction = ctx.tools().mysql()?.transaction().await?;
        let result = async {
            let id = match self
                .users
                .insert_in_tx(
                    ctx,
                    &mut transaction,
                    &username,
                    &password_hash,
                    &email,
                    email_verified_at,
                )
                .await
            {
                Ok(id) => id,
                Err(BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_))) => {
                    return Err(registration_identity_exists_error());
                }
                Err(error) => return Err(error),
            };
            if let OwnerClaimOutcome::Claimed { admin_id } = self
                .system_owner_claimer
                .claim(&mut transaction, id, &username)
                .await?
            {
                let event = audit::succeeded_system_event(
                    ctx,
                    "first-registration",
                    None,
                    Some(audit::entity("user", id)?),
                    audit::entity("admin_account", admin_id)?,
                    None,
                    Some(audit::summary([
                        ("admin", json!(true)),
                        ("system_owner", json!(true)),
                        ("user_id", json!(id)),
                    ])?),
                )?;
                audit::append_in_tx(&mut transaction, &event).await?;
            }
            Ok(id)
        }
        .await;
        let id = finish_registration_transaction(transaction, result).await?;
        self.view_by_id(ctx, id).await
    }

    pub(in crate::addon::account::user) async fn request_registration_email(
        &self,
        ctx: &ActionContext,
        email: &str,
    ) -> Result<RegistrationEmailCodeAccepted, BaseError> {
        let email = normalize_email(email)?;
        let deliver = !self.users.email_exists(ctx, &email).await?;
        RegistrationEmailVerification::from_context(ctx)?
            .request(ctx, &email, deliver)
            .await
    }

    pub(in crate::addon::account::user) async fn authenticate(
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
        ensure_active_status(user.status)?;
        Ok(AuthenticatedUser { id: user.id })
    }

    pub(in crate::addon::account::user) async fn authenticate_step_up(
        &self,
        ctx: &ActionContext,
        username: &str,
        plain_password: &str,
    ) -> Result<AuthenticatedUser, BaseError> {
        let username = normalize_username(username)?;
        let operation = AuthOperation::StepUpComplete;
        self.rate_limiter.check(ctx, operation, &username).await?;

        let user = match self
            .users
            .find_credentials_by_username(ctx, &username)
            .await?
        {
            Some(user) => user,
            None => {
                self.rate_limiter
                    .record_failure(ctx, operation, &username)
                    .await?;
                return Err(BaseError::InvalidPassword);
            }
        };
        if !self
            .passwords
            .verify(plain_password, &user.password_hash)
            .await?
        {
            self.rate_limiter
                .record_failure(ctx, operation, &username)
                .await?;
            return Err(BaseError::InvalidPassword);
        }
        if let Err(error) = ensure_active_status(user.status) {
            self.rate_limiter
                .record_failure(ctx, operation, &username)
                .await?;
            return Err(error);
        }
        self.rate_limiter
            .clear_failures(ctx, operation, &username)
            .await?;
        Ok(AuthenticatedUser { id: user.id })
    }

    pub(in crate::addon::account::user) async fn change_password(
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
        ensure_active_status(observed.status)?;
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
            let locked =
                lock_user_credential(ctx.tools().mysql()?.pool(), &mut transaction, user_id)
                    .await?;
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

    pub(in crate::addon::account::user) async fn reset_password(
        &self,
        ctx: &ActionContext,
        raw_reset_token: &str,
        new_password: &str,
    ) -> Result<(), BaseError> {
        if !self.issue_refresh_credential_version {
            return Err(BaseError::ConfigError(
                "密码重置能力必须在全部实例开启 Refresh 凭据版本签发后启用".to_string(),
            ));
        }
        validate_new_password(new_password)?;
        let attempt_fingerprint = PasswordResetReference::attempt_fingerprint(raw_reset_token)?;
        self.rate_limiter
            .check(
                ctx,
                AuthOperation::PasswordResetConsume,
                &attempt_fingerprint,
            )
            .await?;
        let reference = PasswordResetReference::parse(raw_reset_token)?;

        // 无论凭证是否存在，新摘要都在任何事务和行锁之前计算；全局/指纹限流约束成本。
        let new_password_hash = self.passwords.hash(new_password).await?;
        let target_user_id =
            find_password_reset_target_user(ctx.tools().mysql()?.pool(), &reference)
                .await?
                .ok_or_else(invalid_reset_token)?;

        let mut transaction = ctx.tools().mysql()?.transaction().await?;
        let result = async {
            // 所有创建与消费路径统一先锁用户、再锁凭证，避免同用户并发形成反向锁序。
            let locked_user = lock_user_credential(
                ctx.tools().mysql()?.pool(),
                &mut transaction,
                target_user_id,
            )
            .await?;
            ensure_active_status(locked_user.status())?;
            let locked_reset = lock_password_reset_in_tx(
                ctx.tools().mysql()?.pool(),
                &mut transaction,
                &reference,
            )
            .await?;
            if locked_reset.user_id() != target_user_id || !locked_reset.is_usable() {
                return Err(invalid_reset_token());
            }
            self.users
                .update_password_hash_in_tx(
                    ctx,
                    &mut transaction,
                    target_user_id,
                    &new_password_hash,
                )
                .await?;
            increment_locked_credential_versions(&mut transaction, &locked_user).await?;
            consume_password_reset_in_tx(&mut transaction, &locked_reset).await?;
            let event = audit::succeeded_system_event(
                ctx,
                format!("password-reset-{}", reference.fingerprint()),
                None,
                Some(audit::entity("user", target_user_id)?),
                audit::entity("password_reset", reference.fingerprint())?,
                None,
                Some(audit::summary([
                    ("relogin_required", json!(true)),
                    ("reset_fingerprint", json!(reference.fingerprint())),
                    ("user_id", json!(target_user_id)),
                ])?),
            )?;
            audit::append_in_tx(&mut transaction, &event).await?;
            Ok(())
        }
        .await;
        finish_transaction(transaction, result).await
    }

    /// 持久撤销当前用户的全部会话；返回 Redis 水位线是否已经即时写入。
    pub(in crate::addon::account::user) async fn revoke_all_sessions(
        &self,
        ctx: &ActionContext,
        user_id: i64,
    ) -> Result<bool, BaseError> {
        if !self.issue_refresh_credential_version {
            return Err(BaseError::ConfigError(
                "全量会话撤销必须在全部实例签发 Refresh 凭据版本后启用".to_string(),
            ));
        }
        let mut transaction = ctx.tools().mysql()?.transaction().await?;
        let result = async {
            let locked =
                lock_user_credential(ctx.tools().mysql()?.pool(), &mut transaction, user_id)
                    .await?;
            ensure_active_status(locked.status())?;
            increment_locked_credential_versions(&mut transaction, &locked).await?;
            let event = audit::succeeded_event(
                ctx,
                None,
                Some(audit::entity("user", user_id)?),
                audit::entity("session_set", user_id)?,
                None,
                Some(audit::summary([
                    ("relogin_required", json!(true)),
                    ("revocation_requested", json!(true)),
                ])?),
            )?;
            audit::append_in_tx(&mut transaction, &event).await?;
            Ok(())
        }
        .await;
        finish_transaction(transaction, result).await?;

        self.converge_subject_revocation(ctx, user_id, "account.user.logout", "session_set")
            .await
    }

    /// 在持锁事务内停用账号及全部授权关系；提交后尽力即时收敛 Redis 水位线。
    pub(in crate::addon::account::user) async fn disable_self(
        &self,
        ctx: &ActionContext,
        user_id: i64,
    ) -> Result<bool, BaseError> {
        if !self.issue_refresh_credential_version {
            return Err(BaseError::ConfigError(
                "账号停用必须在全部实例签发 Refresh 凭据版本后启用".to_string(),
            ));
        }
        super::lifecycle::disable_self(ctx, user_id).await?;
        self.converge_subject_revocation(ctx, user_id, "account.user.disable_self", "user")
            .await
    }

    async fn converge_subject_revocation(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        action: &'static str,
        target_kind: &'static str,
    ) -> Result<bool, BaseError> {
        match ctx
            .tools()
            .token()?
            .revoke_by_subject(&user_id.to_string())
            .await
        {
            Ok(()) => Ok(true),
            Err(error) => {
                let audit_event = audit::AuditEvent::new(
                    audit::AuditEventContext::new(
                        audit::AuditActor::user(user_id).map_err(invalid_audit_event)?,
                        None,
                        ctx.request_id(),
                    )
                    .map_err(invalid_audit_event)?,
                    action,
                    Some(audit::entity("user", user_id)?),
                    audit::entity(target_kind, user_id)?,
                    audit::AuditResult::Failed,
                    None,
                    Some(audit::summary([
                        ("error_code", json!(error.code_str())),
                        ("outcome_code", json!("redis_convergence_pending")),
                    ])?),
                )
                .map_err(invalid_audit_event)?;
                if let Err(audit_error) =
                    audit::append_independent(ctx.tools().mysql()?.pool(), &audit_event).await
                {
                    tracing::error!(
                        error_code = error.code_str(),
                        audit_error_code = audit_error.code_str(),
                        user_id,
                        "账号会话 Redis 收敛与失败审计均未完成"
                    );
                }
                Ok(false)
            }
        }
    }

    pub(in crate::addon::account::user) async fn claims_for(
        &self,
        ctx: &ActionContext,
        user_id: i64,
    ) -> Result<TokenPairClaims, BaseError> {
        let snapshot = self.authorization_snapshot(ctx, user_id).await?;
        self.claims_from_snapshot(&snapshot)
    }

    pub(in crate::addon::account::user) async fn claims_for_subject(
        &self,
        ctx: &ActionContext,
        subject: &str,
    ) -> Result<TokenPairClaims, BaseError> {
        let user_id = subject
            .parse::<i64>()
            .map_err(|_| BaseError::Unauthorized("Token subject 无效".to_string()))?;
        self.claims_for(ctx, user_id).await
    }

    pub(in crate::addon::account::user) async fn claims_for_refresh(
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

    pub(in crate::addon::account::user) async fn view_by_id(
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
        let status = UserStatus::from_storage(&user.require::<String>(STATUS)?)?;
        ensure_active_status(status)?;
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

async fn finish_registration_transaction<T>(
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
                tracing::error!(error = %rollback_error, "用户注册事务回滚失败");
            }
            Err(error)
        }
    }
}

fn validate_authorization_state(state: &AuthorizationStateRecord) -> Result<(), BaseError> {
    ensure_active_status(state.status)?;
    if state.authz_version < 1 {
        return Err(BaseError::Unauthorized("用户授权版本无效".to_string()));
    }
    if state.credential_version < 0 {
        return Err(BaseError::Unauthorized("用户凭据版本无效".to_string()));
    }
    Ok(())
}

fn ensure_active_status(status: UserStatus) -> Result<(), BaseError> {
    if !status.is_active() {
        return Err(BaseError::Unauthorized("用户已停用".to_string()));
    }
    Ok(())
}

fn username_exists_error() -> BaseError {
    BaseError::ParamInvalid("username".to_string(), "用户名已存在".to_string())
}

fn registration_identity_exists_error() -> BaseError {
    BaseError::ParamInvalid(
        "registration".to_string(),
        "用户名或邮箱已被其他请求注册，请重新获取验证码".to_string(),
    )
}

fn current_unix_timestamp() -> Result<i64, BaseError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| BaseError::ConfigError("系统时间早于 Unix epoch".to_string()))?
        .as_secs();
    i64::try_from(seconds).map_err(|_| BaseError::ConfigError("系统时间超出 i64 范围".to_string()))
}

fn invalid_audit_event(error: anyhow::Error) -> BaseError {
    BaseError::ConfigError(format!("构建账号生命周期审计事件失败: {error}"))
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

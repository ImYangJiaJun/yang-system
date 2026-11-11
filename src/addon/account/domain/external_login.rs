//! 外部 IdP 登录端口：账号域对 Feishu SSO 暴露的最小能力面。
//!
//! account 不依赖 feishu；feishu 的 `sso` module 通过本端口完成「查绑定 / 新建
//! 用户并绑定 / 绑定已有用户 / 校验账号密码 / 签发系统会话」。这样 OAuth 传输、
//! state、PKCE 全部留在 feishu addon，账号域的写入仍走既有的受信 Repository。

use super::context::Account;
use super::identity::{ExternalIdentityProfile, IdentityRecord, IDENTITY_ACTIVE};
use async_trait::async_trait;
use yang_base::action::auth::AuthOperation;
use yang_base::action::ActionContext;
use yang_base::BaseError;

/// SSO 侧可安全展示的绑定视图（不含任何 token）。
#[derive(Debug, Clone)]
pub(crate) struct ExternalIdentityView {
    pub(crate) id: i64,
    pub(crate) user_id: i64,
    pub(crate) provider: String,
    pub(crate) tenant_key: String,
    pub(crate) external_id: String,
    pub(crate) name: Option<String>,
    pub(crate) email: Option<String>,
    pub(crate) bound_at: i64,
    pub(crate) last_login_at: Option<i64>,
}

impl From<IdentityRecord> for ExternalIdentityView {
    fn from(record: IdentityRecord) -> Self {
        Self {
            id: record.id,
            user_id: record.user_id,
            provider: record.provider,
            tenant_key: record.tenant_key,
            external_id: record.external_id,
            name: record.external_name,
            email: record.external_email,
            bound_at: record.bound_at,
            last_login_at: record.last_login_at,
        }
    }
}

/// 一次成功签发后的系统 Token 对（Refresh 只由调用方写 Cookie）。
#[derive(Debug, Clone)]
pub(crate) struct IssuedSession {
    pub(crate) access_token: String,
    pub(crate) refresh_token: String,
}

/// 外部登录端口。所有方法都要求调用方持有有效的 `ActionContext`（数据库连接）。
#[async_trait]
pub(crate) trait ExternalLoginPort: Send + Sync {
    async fn find_identity(
        &self,
        ctx: &ActionContext,
        provider: &str,
        tenant_key: &str,
        external_id: &str,
    ) -> Result<Option<ExternalIdentityView>, BaseError>;

    async fn find_active_user_by_email(
        &self,
        ctx: &ActionContext,
        email: &str,
    ) -> Result<Option<i64>, BaseError>;

    /// 新建系统用户并在同一事务内写入绑定。**不参与** `system_owner` 引导。
    async fn create_user_and_bind(
        &self,
        ctx: &ActionContext,
        username: &str,
        password: &str,
        profile: &ExternalIdentityProfile,
    ) -> Result<(i64, ExternalIdentityView), BaseError>;

    /// 绑定已有用户；冲突时返回可行动的 `ParamInvalid`。
    async fn bind_identity(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        profile: &ExternalIdentityProfile,
    ) -> Result<ExternalIdentityView, BaseError>;

    /// 校验已有账号的用户名/邮箱 + 密码（+MFA），返回用户 ID。
    async fn verify_password(
        &self,
        ctx: &ActionContext,
        identifier: &str,
        password: &str,
        mfa_code: Option<&str>,
    ) -> Result<i64, BaseError>;

    /// 校验当前登录用户的密码，作为敏感操作的 Step-up。
    async fn verify_current_password(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        password: &str,
    ) -> Result<(), BaseError>;

    /// 为已确认的用户签发系统会话（含 `user_session` 与登录事件）。
    async fn issue_session(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        ip: &str,
        user_agent: &str,
    ) -> Result<IssuedSession, BaseError>;

    async fn list_identities(
        &self,
        ctx: &ActionContext,
        user_id: i64,
    ) -> Result<Vec<ExternalIdentityView>, BaseError>;

    /// 删除绑定并返回影响行数。调用方应先删除 feishu 侧凭证。
    async fn unbind_identity(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        provider: &str,
        tenant_key: &str,
    ) -> Result<u64, BaseError>;

    async fn has_system_owner(&self, ctx: &ActionContext) -> Result<bool, BaseError>;

    async fn user_is_active(&self, ctx: &ActionContext, user_id: i64) -> Result<bool, BaseError>;

    async fn user_requires_second_factor(
        &self,
        ctx: &ActionContext,
        user_id: i64,
    ) -> Result<bool, BaseError>;

    /// 用户已验证的 MFA 备用邮箱（无 TOTP/无邮箱返回 None）。
    async fn user_mfa_email(
        &self,
        ctx: &ActionContext,
        user_id: i64,
    ) -> Result<Option<String>, BaseError>;

    async fn verify_second_factor(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        code: &str,
        purpose: &'static str,
    ) -> Result<(), BaseError>;

    async fn touch_identity_login(
        &self,
        ctx: &ActionContext,
        identity_id: i64,
        profile: &ExternalIdentityProfile,
    ) -> Result<(), BaseError>;
}

#[async_trait]
impl ExternalLoginPort for Account {
    async fn find_identity(
        &self,
        ctx: &ActionContext,
        provider: &str,
        tenant_key: &str,
        external_id: &str,
    ) -> Result<Option<ExternalIdentityView>, BaseError> {
        Ok(self
            .identities()
            .find_by_external(ctx, provider, tenant_key, external_id)
            .await?
            .map(ExternalIdentityView::from))
    }

    async fn find_active_user_by_email(
        &self,
        ctx: &ActionContext,
        email: &str,
    ) -> Result<Option<i64>, BaseError> {
        self.users().find_active_user_by_email(ctx, email).await
    }

    async fn create_user_and_bind(
        &self,
        ctx: &ActionContext,
        username: &str,
        password: &str,
        profile: &ExternalIdentityProfile,
    ) -> Result<(i64, ExternalIdentityView), BaseError> {
        let username = crate::addon::account::domain::policy::normalize_username(username)?;
        crate::addon::account::domain::policy::validate_password_field(
            "password",
            password,
            Some(&username),
        )?;
        self.rate_limiter()
            .check(ctx, AuthOperation::Register, &username)
            .await?;
        if self.users().username_exists(ctx, &username).await? {
            return Err(BaseError::ParamInvalid(
                "username".to_string(),
                "用户名已存在".to_string(),
            ));
        }
        let password_hash = self.passwords().hash(password).await?;
        let now = now_unix_secs()?;
        let mut transaction = ctx.begin_transaction().await?;
        let result = async {
            let user_id = match self
                .users()
                .insert_in_tx(ctx, &mut transaction, &username, &password_hash)
                .await
            {
                Ok(id) => id,
                Err(error) if crate::db::is_unique_violation(&error) => {
                    return Err(BaseError::ParamInvalid(
                        "username".to_string(),
                        "用户名已被其他请求注册".to_string(),
                    ));
                }
                Err(error) => return Err(error),
            };
            let identity_id = match self
                .identities()
                .insert_in_tx(ctx, &mut transaction, user_id, profile, now)
                .await
            {
                Ok(id) => id,
                Err(error) if crate::db::is_constraint_violation(&error) => {
                    return Err(BaseError::ParamInvalid(
                        "identity".to_string(),
                        "该外部身份已绑定其他系统账号".to_string(),
                    ));
                }
                Err(error) => return Err(error),
            };
            Ok((user_id, identity_id))
        }
        .await;
        let (user_id, identity_id) = Account::finish_transaction(transaction, result).await?;
        Ok((
            user_id,
            view_from_profile(identity_id, user_id, profile, now, None),
        ))
    }

    async fn bind_identity(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        profile: &ExternalIdentityProfile,
    ) -> Result<ExternalIdentityView, BaseError> {
        if let Some(existing) = self
            .identities()
            .find_by_external(
                ctx,
                &profile.provider,
                &profile.tenant_key,
                &profile.external_id,
            )
            .await?
        {
            if existing.user_id == user_id {
                return Ok(ExternalIdentityView::from(existing));
            }
            return Err(BaseError::ParamInvalid(
                "identity".to_string(),
                "该外部身份已绑定其他系统账号".to_string(),
            ));
        }
        if let Some(existing) = self
            .identities()
            .find_by_user_and_provider(ctx, user_id, &profile.provider, &profile.tenant_key)
            .await?
        {
            if existing.external_id == profile.external_id {
                return Ok(ExternalIdentityView::from(existing));
            }
            return Err(BaseError::ParamInvalid(
                "identity".to_string(),
                "当前账号已绑定其他飞书账号，请先解绑".to_string(),
            ));
        }
        let now = now_unix_secs()?;
        let mut transaction = ctx.begin_transaction().await?;
        let result = self
            .identities()
            .insert_in_tx(ctx, &mut transaction, user_id, profile, now)
            .await;
        let identity_id = match result {
            Ok(id) => Account::finish_transaction(transaction, Ok(id)).await?,
            Err(error) if crate::db::is_constraint_violation(&error) => {
                let _ = transaction.rollback().await;
                return Err(BaseError::ParamInvalid(
                    "identity".to_string(),
                    "该外部身份已绑定其他系统账号".to_string(),
                ));
            }
            Err(error) => {
                let _ = transaction.rollback().await;
                return Err(error);
            }
        };
        Ok(view_from_profile(identity_id, user_id, profile, now, None))
    }

    async fn verify_password(
        &self,
        ctx: &ActionContext,
        identifier: &str,
        password: &str,
        mfa_code: Option<&str>,
    ) -> Result<i64, BaseError> {
        self.verify_password_credentials(ctx, identifier, password, mfa_code, true, "feishu_bind")
            .await
    }

    async fn verify_current_password(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        password: &str,
    ) -> Result<(), BaseError> {
        Account::verify_current_password(self, ctx, user_id, password).await
    }

    async fn issue_session(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        ip: &str,
        user_agent: &str,
    ) -> Result<IssuedSession, BaseError> {
        self.issue_login_session(ctx, user_id, ip, user_agent).await
    }

    async fn list_identities(
        &self,
        ctx: &ActionContext,
        user_id: i64,
    ) -> Result<Vec<ExternalIdentityView>, BaseError> {
        Ok(self
            .identities()
            .find_by_user(ctx, user_id)
            .await?
            .into_iter()
            .filter(|record| record.status == IDENTITY_ACTIVE)
            .map(ExternalIdentityView::from)
            .collect())
    }

    async fn unbind_identity(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        provider: &str,
        tenant_key: &str,
    ) -> Result<u64, BaseError> {
        let mut transaction = ctx.begin_transaction().await?;
        let result = self
            .identities()
            .delete_by_user_provider_in_tx(ctx, &mut transaction, user_id, provider, tenant_key)
            .await;
        Account::finish_transaction(transaction, result).await
    }

    async fn has_system_owner(&self, ctx: &ActionContext) -> Result<bool, BaseError> {
        self.system_authorization().has_system_owner(ctx).await
    }

    async fn user_is_active(&self, ctx: &ActionContext, user_id: i64) -> Result<bool, BaseError> {
        Ok(self
            .users()
            .find_credentials_by_id(ctx, user_id)
            .await?
            .is_some_and(|record| record.status.is_active()))
    }

    async fn user_requires_second_factor(
        &self,
        ctx: &ActionContext,
        user_id: i64,
    ) -> Result<bool, BaseError> {
        Ok(self
            .users()
            .find_totp_state_by_id(ctx, user_id)
            .await?
            .is_some_and(|state| state.totp_activated_at.is_some()))
    }

    async fn user_mfa_email(
        &self,
        ctx: &ActionContext,
        user_id: i64,
    ) -> Result<Option<String>, BaseError> {
        Ok(self
            .users()
            .find_totp_state_by_id(ctx, user_id)
            .await?
            .and_then(|state| state.email))
    }

    async fn verify_second_factor(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        code: &str,
        purpose: &'static str,
    ) -> Result<(), BaseError> {
        let Some(state) = self.users().find_totp_state_by_id(ctx, user_id).await? else {
            return Err(BaseError::InvalidPassword);
        };
        let secret = self.users().decrypt_totp_secret(ctx, &state).await?;
        self.verify_second_factor(ctx, user_id, &state, &secret, code, true, purpose)
            .await
    }

    async fn touch_identity_login(
        &self,
        ctx: &ActionContext,
        identity_id: i64,
        profile: &ExternalIdentityProfile,
    ) -> Result<(), BaseError> {
        let now = now_unix_secs()?;
        let mut transaction = ctx.begin_transaction().await?;
        let result = self
            .identities()
            .touch_login_in_tx(ctx, &mut transaction, identity_id, profile, now)
            .await;
        Account::finish_transaction(transaction, result).await
    }
}

fn view_from_profile(
    identity_id: i64,
    user_id: i64,
    profile: &ExternalIdentityProfile,
    bound_at: i64,
    last_login_at: Option<i64>,
) -> ExternalIdentityView {
    ExternalIdentityView {
        id: identity_id,
        user_id,
        provider: profile.provider.clone(),
        tenant_key: profile.tenant_key.clone(),
        external_id: profile.external_id.clone(),
        name: profile.name.clone(),
        email: profile.email.clone(),
        bound_at,
        last_login_at,
    }
}

fn now_unix_secs() -> Result<i64, BaseError> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| BaseError::ConfigError("系统时间早于 Unix epoch".to_string()))?
        .as_secs();
    i64::try_from(seconds).map_err(|_| BaseError::ConfigError("系统时间超出 i64 范围".to_string()))
}

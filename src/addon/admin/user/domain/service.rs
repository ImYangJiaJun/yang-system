//! 平台账号用例服务。

use super::model::{AdminAccountPage, AdminAccountView, PageRequest};
use super::repository::AdminRepository;
use crate::addon::account::{AuthOperation, AuthRateLimiter, GeneratedPasswordReset};
use schemars::JsonSchema;
use serde::Serialize;
use std::fmt;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::BaseError;

#[derive(Serialize, JsonSchema)]
pub(in crate::addon::admin::user) struct PasswordResetCreated {
    user_id: i64,
    reset_token: String,
    reset_fingerprint: String,
    expires_in_seconds: u64,
}

impl fmt::Debug for PasswordResetCreated {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PasswordResetCreated")
            .field("user_id", &self.user_id)
            .field("reset_token", &"[REDACTED]")
            .field("reset_fingerprint", &self.reset_fingerprint)
            .field("expires_in_seconds", &self.expires_in_seconds)
            .finish()
    }
}

pub(in crate::addon::admin::user) struct AdminService {
    repository: AdminRepository,
    rate_limiter: Arc<AuthRateLimiter>,
    password_reset_ttl_seconds: u64,
    password_reset_enabled: bool,
}

impl AdminService {
    pub(in crate::addon::admin::user) fn new(
        repository: AdminRepository,
        rate_limiter: Arc<AuthRateLimiter>,
        password_reset_ttl_seconds: u64,
        password_reset_enabled: bool,
    ) -> Self {
        Self {
            repository,
            rate_limiter,
            password_reset_ttl_seconds,
            password_reset_enabled,
        }
    }

    pub(in crate::addon::admin::user) async fn list(
        &self,
        ctx: &ActionContext,
        page: Option<i64>,
        limit: Option<i64>,
        search: Option<&str>,
    ) -> Result<AdminAccountPage, BaseError> {
        let request = PageRequest::parse(page, limit)?;
        let search = normalize_optional("search", search, 100)?;
        self.repository.list(ctx, request, search.as_deref()).await
    }

    pub(in crate::addon::admin::user) async fn add(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        name: &str,
        position: Option<&str>,
        admin: bool,
    ) -> Result<AdminAccountView, BaseError> {
        validate_id("user_user", user_id)?;
        let name = normalize_required("name", name, 50)?;
        let position = normalize_optional("position", position, 50)?;
        self.repository
            .add(ctx, user_id, &name, position.as_deref(), admin)
            .await
    }

    pub(in crate::addon::admin::user) async fn create_password_reset(
        &self,
        ctx: &ActionContext,
        target_user_id: i64,
    ) -> Result<PasswordResetCreated, BaseError> {
        if !self.password_reset_enabled {
            return Err(BaseError::ConfigError(
                "密码重置能力必须在全部实例开启 Refresh 凭据版本签发后启用".to_string(),
            ));
        }
        validate_id("user_id", target_user_id)?;
        let requested_by_user_id = ctx
            .authenticated_user()
            .ok_or_else(|| BaseError::Unauthorized("创建密码重置凭证需要登录".to_string()))?
            .id;
        self.rate_limiter
            .check(
                ctx,
                AuthOperation::PasswordResetCreate,
                &format!("{requested_by_user_id}-{target_user_id}"),
            )
            .await?;
        let reset = GeneratedPasswordReset::generate()?;
        self.repository
            .create_password_reset(
                ctx,
                target_user_id,
                requested_by_user_id,
                &reset,
                self.password_reset_ttl_seconds,
            )
            .await?;
        Ok(PasswordResetCreated {
            user_id: target_user_id,
            reset_token: reset.raw_token().to_string(),
            reset_fingerprint: reset.reference().fingerprint().to_string(),
            expires_in_seconds: self.password_reset_ttl_seconds,
        })
    }

    pub(in crate::addon::admin::user) async fn set_status(
        &self,
        ctx: &ActionContext,
        id: i64,
        status: &str,
    ) -> Result<AdminAccountView, BaseError> {
        validate_id("id", id)?;
        if !matches!(status, super::super::ACTIVE_STATUS | "disabled") {
            return Err(BaseError::ParamInvalid(
                "status".to_string(),
                "平台账号状态必须是 active 或 disabled".to_string(),
            ));
        }
        self.repository.set_status(ctx, id, status).await
    }

    pub(in crate::addon::admin::user) async fn set_admin(
        &self,
        ctx: &ActionContext,
        id: i64,
        admin: bool,
    ) -> Result<AdminAccountView, BaseError> {
        validate_id("id", id)?;
        self.repository.set_admin(ctx, id, admin).await
    }
}

fn validate_id(field: &str, id: i64) -> Result<(), BaseError> {
    if id < 1 {
        return Err(BaseError::ParamInvalid(
            field.to_string(),
            format!("{field} 必须是正整数"),
        ));
    }
    Ok(())
}

pub(in crate::addon::admin::user) fn normalize_required(
    field: &str,
    value: &str,
    max_length: usize,
) -> Result<String, BaseError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max_length {
        return Err(BaseError::ParamInvalid(
            field.to_string(),
            format!("{field} 长度必须在 1..={max_length} 之间"),
        ));
    }
    Ok(value.to_string())
}

pub(in crate::addon::admin::user) fn normalize_optional(
    field: &str,
    value: Option<&str>,
    max_length: usize,
) -> Result<Option<String>, BaseError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > max_length {
        return Err(BaseError::ParamInvalid(
            field.to_string(),
            format!("{field} 长度不能超过 {max_length}"),
        ));
    }
    Ok(Some(value.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_profile_values_are_trimmed_and_bounded() {
        assert_eq!(
            normalize_required("name", "  Root Admin  ", 50)
                .unwrap_or_else(|error| panic!("姓名应有效: {error}")),
            "Root Admin"
        );
        assert_eq!(
            normalize_optional("position", Some("  Owner  "), 50)
                .unwrap_or_else(|error| panic!("职务应有效: {error}")),
            Some("Owner".to_string())
        );
        assert_eq!(
            normalize_optional("position", Some("   "), 50)
                .unwrap_or_else(|error| panic!("空职务应归一化: {error}")),
            None
        );
        assert!(normalize_required("name", "", 50).is_err());
        assert!(normalize_optional("position", Some(&"x".repeat(51)), 50).is_err());
        assert!(validate_id("id", 0).is_err());
        assert!(validate_id("id", 1).is_ok());
    }

    #[test]
    fn password_reset_response_debug_redacts_the_raw_token() {
        let response = PasswordResetCreated {
            user_id: 7,
            reset_token: "raw-secret-reset-token".to_string(),
            reset_fingerprint: "0123456789abcdef".to_string(),
            expires_in_seconds: 900,
        };
        let debug = format!("{response:?}");
        assert!(!debug.contains("raw-secret-reset-token"));
        assert!(debug.contains("[REDACTED]"));
        assert!(debug.contains("0123456789abcdef"));
    }
}

//! 用户领域共享服务。
//!
//! 这里只保留跨 Action 复用的查询和校验；注册、登录等用例逻辑仍由各 Action 文件拥有。

use super::repository::CredentialRepository;
use super::schema::{STATUS, USER_ID, USER_VIEW_FIELDS};
use crate::config::SecuritySettings;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::table::{Record, TableQuery};
use yang_base::BaseError;

#[derive(Clone)]
pub(super) struct UserService {
    security: Arc<SecuritySettings>,
    credentials: Arc<CredentialRepository>,
}

impl UserService {
    pub(super) fn new(
        security: Arc<SecuritySettings>,
        credentials: Arc<CredentialRepository>,
    ) -> Self {
        Self {
            security,
            credentials,
        }
    }

    pub(super) fn credentials(&self) -> &CredentialRepository {
        &self.credentials
    }

    pub(super) fn query(&self, ctx: &ActionContext) -> Result<TableQuery, BaseError> {
        ctx.table_query()
    }

    pub(super) async fn find_by_id(
        &self,
        ctx: &ActionContext,
        id: i64,
    ) -> Result<Option<Record>, BaseError> {
        let rows = self
            .query(ctx)?
            .select_fields(USER_VIEW_FIELDS)?
            .where_eq(USER_ID, serde_json::Value::Number(id.into()))?
            .all()
            .await?;
        Ok(rows.into_iter().next())
    }

    pub(super) fn ensure_active(&self, user: &Record) -> Result<(), BaseError> {
        let status: String = user.require(STATUS)?;
        self.ensure_active_status(&status)
    }

    pub(super) fn ensure_active_status(&self, status: &str) -> Result<(), BaseError> {
        if status != "active" {
            return Err(BaseError::Unauthorized("用户已停用".to_string()));
        }
        Ok(())
    }

    pub(super) fn normalize_username(&self, username: &str) -> Result<String, BaseError> {
        let normalized = username.trim().to_ascii_lowercase();
        let length = normalized.len();
        if length < self.security.username_min_length || length > self.security.username_max_length
        {
            return Err(BaseError::ParamInvalid(
                "username".to_string(),
                format!(
                    "长度必须在 {}..={} 之间",
                    self.security.username_min_length, self.security.username_max_length
                ),
            ));
        }
        if !normalized
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(BaseError::ParamInvalid(
                "username".to_string(),
                "只允许 ASCII 字母、数字、下划线和连字符".to_string(),
            ));
        }
        Ok(normalized)
    }

    pub(super) fn validate_password(&self, password: &str) -> Result<(), BaseError> {
        let length = password.chars().count();
        if length < self.security.password_min_length || length > self.security.password_max_length
        {
            return Err(BaseError::ParamInvalid(
                "password".to_string(),
                format!(
                    "长度必须在 {}..={} 之间",
                    self.security.password_min_length, self.security.password_max_length
                ),
            ));
        }
        Ok(())
    }
}

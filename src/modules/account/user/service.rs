//! 用户领域共享服务。
//!
//! 这里只保留跨 Action 复用的查询和校验；注册、登录等用例逻辑仍由各 Action 文件拥有。

use super::password::PasswordEngine;
use super::rate_limit::AuthRateLimiter;
use super::repository::CredentialRepository;
use super::schema::{STATUS, USER_ID, USER_VIEW_FIELDS};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::table::{Record, TableQuery};
use yang_base::BaseError;

#[derive(Clone)]
pub(super) struct UserService {
    credentials: Arc<CredentialRepository>,
    passwords: Arc<PasswordEngine>,
    rate_limiter: Arc<AuthRateLimiter>,
}

impl UserService {
    pub(super) fn new(
        credentials: Arc<CredentialRepository>,
        passwords: Arc<PasswordEngine>,
        rate_limiter: Arc<AuthRateLimiter>,
    ) -> Self {
        Self {
            credentials,
            passwords,
            rate_limiter,
        }
    }

    pub(super) fn credentials(&self) -> &CredentialRepository {
        &self.credentials
    }

    pub(super) fn passwords(&self) -> &PasswordEngine {
        &self.passwords
    }

    pub(super) fn rate_limiter(&self) -> &AuthRateLimiter {
        &self.rate_limiter
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
}

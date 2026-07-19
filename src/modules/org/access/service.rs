//! pre-tenant 企业用例服务。

use super::repository::TenantRepository;
use crate::modules::org::pagination::{Page, PageRequest};
use schemars::JsonSchema;
use serde::Serialize;
use yang_base::action::ActionContext;
use yang_base::BaseError;

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct TenantSummary {
    pub(super) id: i64,
    pub(super) name: String,
    pub(super) code: String,
}

pub(super) struct TenantService {
    repository: TenantRepository,
}

impl TenantService {
    pub(super) fn new(repository: TenantRepository) -> Self {
        Self { repository }
    }

    pub(super) async fn list(
        &self,
        ctx: &ActionContext,
        page: Option<i64>,
        limit: Option<i64>,
    ) -> Result<Page<TenantSummary>, BaseError> {
        let user = ctx
            .authenticated_user()
            .ok_or_else(|| BaseError::Unauthorized("租户发现需要已认证用户".to_string()))?;
        let request = PageRequest::parse(page, limit)?;
        self.repository.list_for_user(ctx, user.id, request).await
    }

    pub(super) async fn create(
        &self,
        ctx: &ActionContext,
        name: &str,
        code: &str,
    ) -> Result<TenantSummary, BaseError> {
        let user = ctx
            .authenticated_user()
            .ok_or_else(|| BaseError::Unauthorized("创建租户需要已认证用户".to_string()))?;
        let name = normalize_name(name)?;
        let code = normalize_code(code)?;
        match self
            .repository
            .create_for_user(ctx, user.id, &name, &code)
            .await
        {
            Err(BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_))) => Err(
                BaseError::ParamInvalid("code".to_string(), "企业编号已存在".to_string()),
            ),
            result => result,
        }
    }
}

fn normalize_name(name: &str) -> Result<String, BaseError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(BaseError::ParamInvalid(
            "name".to_string(),
            "企业名称长度必须在 1..=100 之间".to_string(),
        ));
    }
    Ok(name.to_string())
}

fn normalize_code(code: &str) -> Result<String, BaseError> {
    let code = code.trim().to_ascii_uppercase();
    if !(2..=32).contains(&code.len())
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(BaseError::ParamInvalid(
            "code".to_string(),
            "企业编号必须为 2..=32 位 ASCII 字母、数字、下划线或连字符".to_string(),
        ));
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onboarding_normalizes_name_and_code() {
        assert_eq!(
            normalize_name("  Example Corp  ")
                .unwrap_or_else(|error| panic!("企业名称应有效: {error}")),
            "Example Corp"
        );
        assert_eq!(
            normalize_code(" acme-01 ").unwrap_or_else(|error| panic!("企业编号应有效: {error}")),
            "ACME-01"
        );
        assert!(normalize_code("a b").is_err());
    }
}

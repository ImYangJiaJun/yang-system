//! 平台账号用例服务。

use super::repository::AdminRepository;
use schemars::JsonSchema;
use serde::Serialize;
use yang_base::action::ActionContext;
use yang_base::BaseError;

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct BootstrapResult {
    id: i64,
    user_user: i64,
    username: String,
    status: String,
    admin: bool,
    refresh_token_required: bool,
}

pub(super) struct AdminService {
    repository: AdminRepository,
}

impl AdminService {
    pub(super) fn new(repository: AdminRepository) -> Self {
        Self { repository }
    }

    pub(super) async fn bootstrap(
        &self,
        ctx: &ActionContext,
        name: &str,
        position: Option<&str>,
    ) -> Result<BootstrapResult, BaseError> {
        let user = ctx
            .authenticated_user()
            .ok_or_else(|| BaseError::Unauthorized("初始化平台账号需要登录".to_string()))?;
        let user_id = user.id;
        let username = user.username.clone();
        let name = normalize_required("name", name, 50)?;
        let position = normalize_optional("position", position, 50)?;
        let id = self
            .repository
            .bootstrap(ctx, user_id, &name, position.as_deref())
            .await?;

        Ok(BootstrapResult {
            id,
            user_user: user_id,
            username,
            status: super::ACTIVE_STATUS.to_string(),
            admin: true,
            refresh_token_required: true,
        })
    }
}

pub(super) fn normalize_required(
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

pub(super) fn normalize_optional(
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
    }
}

//! Token Claims 的唯一构造与可信用户投影。

use crate::modules::account::AuthorizationGrants;
use serde::{Deserialize, Serialize};
use yang_base::action::User;
use yang_base::token::TokenClaims;
use yang_base::BaseError;

const APP_CLAIMS_VERSION: u8 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppClaims {
    version: u8,
    username: String,
    roles: Vec<String>,
    permissions: Vec<String>,
}

pub(super) fn claims_for_user(
    username: &str,
    grants: &AuthorizationGrants,
) -> Result<serde_json::Value, BaseError> {
    serde_json::to_value(AppClaims {
        version: APP_CLAIMS_VERSION,
        username: username.to_string(),
        roles: grants.roles().map(str::to_string).collect(),
        permissions: grants.permissions().map(str::to_string).collect(),
    })
    .map_err(|error| BaseError::Unknown(format!("构造用户 Token Claims 失败: {error}")))
}

pub(crate) fn user_from_claims(claims: &TokenClaims) -> Result<User, BaseError> {
    let id = claims
        .sub
        .parse::<i64>()
        .map_err(|_| BaseError::Unauthorized("Token subject 无效".to_string()))?;
    let app_claims: AppClaims = serde_json::from_value(claims.custom.clone())
        .map_err(|_| BaseError::Unauthorized("Token 自定义声明格式无效".to_string()))?;
    if app_claims.version != APP_CLAIMS_VERSION {
        return Err(BaseError::Unauthorized(format!(
            "不支持的 Token Claims 版本: {}",
            app_claims.version
        )));
    }
    if app_claims.username.trim().is_empty() {
        return Err(BaseError::Unauthorized(
            "Token username 不能为空".to_string(),
        ));
    }
    Ok(User::new(id, app_claims.username)
        .with_roles(app_claims.roles)
        .with_permissions(app_claims.permissions))
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::token::TokenType;

    #[test]
    fn token_claims_project_strict_roles_and_permissions() {
        let claims = TokenClaims::new(
            "test",
            "7",
            "test-api",
            60,
            0,
            0,
            "test-jti",
            TokenType::Access,
            serde_json::json!({
                "version": 1,
                "username": "alice",
                "roles": ["user"],
                "permissions": ["org.user:read"]
            }),
        );

        let user = user_from_claims(&claims)
            .unwrap_or_else(|error| panic!("有效声明应投影为用户: {error}"));
        assert_eq!(user.id, 7);
        assert_eq!(user.username, "alice");
        assert!(user.has_role("user"));
        assert!(user.has_permission("org.user:read"));
    }

    #[test]
    fn token_claims_fail_closed_on_invalid_subject_or_shape() {
        let claims = |subject: &str, custom| {
            TokenClaims::new(
                "test",
                subject,
                "test-api",
                60,
                0,
                0,
                "test-jti",
                TokenType::Access,
                custom,
            )
        };
        let valid_shape = serde_json::json!({
            "version": 1,
            "username": "alice",
            "roles": ["user"],
            "permissions": ["org.user:read"]
        });
        assert!(matches!(
            user_from_claims(&claims("not-an-id", valid_shape)),
            Err(BaseError::Unauthorized(_))
        ));
        assert!(matches!(
            user_from_claims(&claims(
                "7",
                serde_json::json!({
                    "version": 1,
                    "username": "alice",
                    "roles": ["user", 123],
                    "permissions": ["org.user:read"]
                })
            )),
            Err(BaseError::Unauthorized(_))
        ));
    }

    #[test]
    fn login_and_refresh_share_the_same_claims_snapshot() {
        let claims = claims_for_user("alice", &AuthorizationGrants::user())
            .unwrap_or_else(|error| panic!("用户声明应可序列化: {error}"));
        assert_eq!(claims["version"], APP_CLAIMS_VERSION);
        assert_eq!(claims["roles"], serde_json::json!(["user"]));
        assert_eq!(
            claims["permissions"],
            serde_json::json!(["org.org:read", "org.user:read"])
        );
    }
}

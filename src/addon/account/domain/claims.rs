//! Token Claims 的唯一构造与可信用户投影。

use super::grants::AuthorizationGrants;
use serde::{Deserialize, Serialize};
use yang_base::action::auth::TokenPairClaims;
use yang_base::action::User;
use yang_base::token::TokenClaims;
use yang_base::BaseError;

const APP_CLAIMS_VERSION: u8 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppClaims {
    version: u8,
    username: String,
    authz_version: i64,
    roles: Vec<String>,
    permissions: Vec<String>,
}

pub(super) fn claims_for_user(
    username: &str,
    authz_version: i64,
    credential_version: i64,
    issue_refresh_credential_version: bool,
    grants: &AuthorizationGrants,
) -> Result<TokenPairClaims, BaseError> {
    if authz_version < 1 {
        return Err(BaseError::Unauthorized(
            "用户授权版本必须是正整数".to_string(),
        ));
    }
    if credential_version < 0 {
        return Err(BaseError::Unauthorized(
            "用户凭据版本不能是负数".to_string(),
        ));
    }
    let access = serde_json::to_value(AppClaims {
        version: APP_CLAIMS_VERSION,
        username: username.to_string(),
        authz_version,
        roles: grants.roles().map(str::to_string).collect(),
        permissions: grants.permissions().map(str::to_string).collect(),
    })
    .map_err(|error| BaseError::Unknown(format!("构造用户 Token Claims 失败: {error}")))?;
    let refresh = if issue_refresh_credential_version {
        serde_json::json!({ "credential_version": credential_version })
    } else {
        serde_json::json!({ "authz_version": authz_version })
    };
    Ok(TokenPairClaims::new(access).with_refresh(refresh))
}

pub(super) fn validate_refresh_credential_version(
    claims: &TokenClaims,
    current: i64,
) -> Result<(), BaseError> {
    if current < 0 {
        return Err(BaseError::Unauthorized(
            "用户凭据版本不能是负数".to_string(),
        ));
    }
    let presented = match claims.custom.get("credential_version") {
        None => 0,
        Some(value) => value
            .as_i64()
            .filter(|version| *version >= 0)
            .ok_or_else(|| BaseError::Unauthorized("Refresh Token 凭据版本无效".to_string()))?,
    };
    if presented != current {
        return Err(BaseError::Unauthorized(
            "Refresh Token 凭据版本已失效".to_string(),
        ));
    }
    Ok(())
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
    if app_claims.authz_version < 1 {
        return Err(BaseError::AuthorizationVersionInvalid);
    }
    Ok(User::new(id, app_claims.username)
        .with_roles(app_claims.roles)
        .with_permissions(app_claims.permissions))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::Algorithm;
    use yang_base::token::{TokenManager, TokenType};

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
                "authz_version": 7,
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
            "authz_version": 7,
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
        assert!(matches!(
            user_from_claims(&claims(
                "7",
                serde_json::json!({
                    "version": 1,
                    "username": "alice",
                    "authz_version": 0,
                    "roles": ["user"],
                    "permissions": ["org.user:read"]
                })
            )),
            Err(BaseError::AuthorizationVersionInvalid)
        ));
    }

    #[test]
    fn login_and_refresh_share_the_same_claims_snapshot() {
        let claims = claims_for_user("alice", 7, 0, false, &AuthorizationGrants::user())
            .unwrap_or_else(|error| panic!("用户声明应可序列化: {error}"));
        assert_eq!(claims.access["version"], APP_CLAIMS_VERSION);
        assert_eq!(claims.access["authz_version"], 7);
        assert!(claims.access.get("credential_version").is_none());
        assert_eq!(claims.access["roles"], serde_json::json!(["user"]));
        assert_eq!(claims.access["permissions"], serde_json::json!([]));
        assert_eq!(claims.refresh, serde_json::json!({ "authz_version": 7 }));
        assert!(claims_for_user("alice", 0, 0, false, &AuthorizationGrants::user()).is_err());
        assert!(claims_for_user("alice", 7, -1, false, &AuthorizationGrants::user()).is_err());
    }

    #[test]
    fn enforced_protocol_only_adds_credential_version_to_refresh() {
        let claims = claims_for_user("alice", 7, 3, true, &AuthorizationGrants::user())
            .unwrap_or_else(|error| panic!("开启签发后声明应可序列化: {error}"));

        assert!(claims.access.get("credential_version").is_none());
        assert_eq!(
            claims.refresh,
            serde_json::json!({ "credential_version": 3 })
        );
    }

    #[test]
    fn authorization_version_survives_jwt_round_trip() {
        let manager = TokenManager::new_symmetric(
            "claims-round-trip-secret-32-bytes",
            Algorithm::HS256,
            "test".to_string(),
            "test-api".to_string(),
            60,
            120,
        );
        let custom = claims_for_user("alice", 7, 3, true, &AuthorizationGrants::user())
            .unwrap_or_else(|error| panic!("授权快照应可序列化: {error}"));
        let (access, refresh) = manager
            .generate_token_pair_with_refresh_claims("7", custom.access, custom.refresh)
            .unwrap_or_else(|error| panic!("Token 对应可签发: {error}"));

        let access_claims = manager
            .verify_token(&access)
            .unwrap_or_else(|error| panic!("Access Token 应可验签: {error}"));
        let refresh_claims = manager
            .verify_token(&refresh)
            .unwrap_or_else(|error| panic!("Refresh Token 应可验签: {error}"));

        assert_eq!(access_claims.custom["authz_version"], 7);
        assert_eq!(access_claims.custom["roles"], serde_json::json!(["user"]));
        assert_eq!(refresh_claims.custom["credential_version"], 3);
        assert!(refresh_claims.custom.get("authz_version").is_none());
        assert!(
            refresh_claims.custom.get("roles").is_none(),
            "Refresh Token 只能携带最小版本声明"
        );
    }

    #[test]
    fn refresh_credential_version_comparison_is_fail_closed() {
        let claims = |custom| {
            TokenClaims::new(
                "test",
                "7",
                "test-api",
                60,
                0,
                0,
                "test-refresh-jti",
                TokenType::Refresh,
                custom,
            )
        };

        assert!(validate_refresh_credential_version(&claims(serde_json::Value::Null), 0).is_ok());
        assert!(validate_refresh_credential_version(
            &claims(serde_json::json!({ "authz_version": 7 })),
            0,
        )
        .is_ok());
        for (custom, current) in [
            (serde_json::json!({ "credential_version": 0 }), 1),
            (serde_json::json!({ "credential_version": 2 }), 1),
            (serde_json::json!({ "credential_version": -1 }), 0),
            (serde_json::json!({ "credential_version": "1" }), 1),
        ] {
            assert!(
                validate_refresh_credential_version(&claims(custom), current).is_err(),
                "落后、领先、负数和错误类型都必须拒绝"
            );
        }
        assert!(validate_refresh_credential_version(
            &claims(serde_json::json!({ "credential_version": 3 })),
            3,
        )
        .is_ok());
    }
}

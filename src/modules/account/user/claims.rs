//! Token Claims 到可信用户上下文的投影。

use yang_base::action::User;
use yang_base::token::TokenClaims;

pub(crate) fn user_from_claims(claims: &TokenClaims) -> User {
    let id = claims.sub.parse::<i64>().unwrap_or_default();
    let username = claims
        .custom
        .get("username")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&claims.sub)
        .to_string();
    let roles = claims
        .custom
        .get("roles")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string);
    let permissions = claims
        .custom
        .get("permissions")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string);
    User::new(id, username)
        .with_roles(roles)
        .with_permissions(permissions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::token::TokenType;

    #[test]
    fn token_claims_project_roles_and_permissions_without_trusting_other_shapes() {
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
                "username": "alice",
                "roles": ["user", 123],
                "permissions": ["org.user:read", null, {"forged": true}]
            }),
        );

        let user = user_from_claims(&claims);
        assert_eq!(user.id, 7);
        assert_eq!(user.username, "alice");
        assert!(user.has_role("user"));
        assert!(!user.has_role("123"));
        assert!(user.has_permission("org.user:read"));
        assert!(!user.has_permission("forged"));
    }
}

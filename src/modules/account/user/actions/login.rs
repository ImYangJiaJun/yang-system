use super::super::schema::{PASSWORD_HASH, STATUS, USERNAME, USER_ID};
use super::super::service::UserService;
#[cfg(test)]
use super::register::hash_password;
use argon2::password_hash::{Error as PasswordHashError, PasswordHash};
use argon2::{Argon2, PasswordVerifier};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use yang_base::action::auth::{CredentialVerifier, LoginAction, LoginInput, VerifiedSubject};
use yang_base::action::ActionContext;
use yang_base::definition::{ActionName, ActionSpec, HttpMethod, ModuleSpec, RouteSpec};
use yang_base::table::Record;
use yang_base::BaseError;

const USER_CREDENTIAL_FIELDS: &[&str] = &[USER_ID, USERNAME, PASSWORD_HASH, STATUS];

#[derive(Clone)]
struct UserCredentialVerifier {
    service: Arc<UserService>,
}

impl UserService {
    async fn authenticate(
        &self,
        ctx: &ActionContext,
        username: &str,
        plain_password: &str,
    ) -> Result<Record, BaseError> {
        let username = self.normalize_username(username)?;
        let user = self
            .find_credentials_by_username(ctx, &username)
            .await?
            .ok_or(BaseError::InvalidPassword)?;
        let password_hash: String = user.require(PASSWORD_HASH)?;
        if !verify_password(plain_password, &password_hash)? {
            return Err(BaseError::InvalidPassword);
        }
        self.ensure_active(&user)?;
        Ok(user)
    }

    async fn find_credentials_by_username(
        &self,
        ctx: &ActionContext,
        username: &str,
    ) -> Result<Option<Record>, BaseError> {
        let rows = self
            .query(ctx)?
            .select_fields(USER_CREDENTIAL_FIELDS)?
            .where_eq(USERNAME, Value::String(username.to_string()))?
            .all()
            .await?;
        Ok(rows.into_iter().next())
    }
}

fn verify_password(password: &str, encoded: &str) -> Result<bool, BaseError> {
    let parsed = PasswordHash::new(encoded)
        .map_err(|_| BaseError::Unknown("数据库中的密码哈希格式无效".to_string()))?;
    match Argon2::default().verify_password(password.as_bytes(), &parsed) {
        Ok(()) => Ok(true),
        Err(PasswordHashError::Password) => Ok(false),
        Err(_) => Err(BaseError::Unknown("密码校验失败".to_string())),
    }
}

#[async_trait]
impl CredentialVerifier for UserCredentialVerifier {
    async fn verify(
        &self,
        ctx: &ActionContext,
        input: &LoginInput,
    ) -> Result<VerifiedSubject, BaseError> {
        let user = self
            .service
            .authenticate(ctx, &input.username, &input.password)
            .await?;
        let id: i64 = user.require(USER_ID)?;
        let username: String = user.require(USERNAME)?;
        Ok(
            VerifiedSubject::new(id.to_string()).with_claims(serde_json::json!({
                "username": username,
                "roles": ["user"],
                "permissions": ["org.org:read", "org.user:read"]
            })),
        )
    }
}

pub(super) fn register(
    module: ModuleSpec,
    service: Arc<UserService>,
) -> Result<ModuleSpec, BaseError> {
    let name =
        ActionName::new("login").map_err(|error| BaseError::ConfigError(error.to_string()))?;
    let spec = ActionSpec::new(
        name,
        RouteSpec::new(HttpMethod::Post, "/api/v1/users/login", "users.login"),
    )
    .display_name("登录")
    .description("校验账号密码并签发 Token")
    .public(true)
    .tag("users");
    Ok(module.action(spec, LoginAction::new(UserCredentialVerifier { service })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_hash_round_trip() {
        let encoded = hash_password("correct-horse-battery-staple")
            .unwrap_or_else(|error| panic!("密码应成功哈希: {error}"));
        assert!(verify_password("correct-horse-battery-staple", &encoded)
            .unwrap_or_else(|error| panic!("密码应成功校验: {error}")));
        assert!(!verify_password("wrong-password", &encoded)
            .unwrap_or_else(|error| panic!("错误密码应得到 false: {error}")));
        assert!(!encoded.contains("correct-horse-battery-staple"));
    }
}

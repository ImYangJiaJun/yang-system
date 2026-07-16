//! 用户与认证领域模块。

mod login;
mod logout;
mod me;
mod refresh;
mod register;

use crate::config::SecuritySettings;
use argon2::password_hash::{Error as PasswordHashError, PasswordHash, SaltString};
use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use rand_core::OsRng;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::MySqlPool;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use yang_base::action::{TokenAuthMiddleware, User};
use yang_base::router::ModuleRouter;
use yang_base::table::{
    FieldPermissions, TableConfig, TableEntity as TableEntityTrait, TableQuery,
};
use yang_base::token::TokenClaims;
use yang_base::{BaseError, TableEntity};

const SYSTEM_ROLE: &str = "system";

/// 用户领域需要两个运行时路由器：公开认证接口不经过强制 Token 中间件，
/// 受保护接口则必须经过 Token 校验。二者共享同一个领域服务和数据表。
pub struct UserModules {
    pub authentication: ModuleRouter,
    pub user: ModuleRouter,
}

pub fn build_modules(
    pool: Arc<MySqlPool>,
    security: Arc<SecuritySettings>,
) -> Result<UserModules, BaseError> {
    let table = user_table_config()?;
    let service = Arc::new(UserService::new(pool, Arc::clone(&table), security));

    let authentication = ModuleRouter::new("user_auth", "用户认证");
    let authentication = register::register(authentication, Arc::clone(&service))?;
    let authentication = login::register(authentication, Arc::clone(&service))?;
    let authentication = refresh::register(authentication, Arc::clone(&service))?;
    let authentication = logout::register(authentication)?;

    let user = ModuleRouter::new("user", "用户管理")
        .with_table_config(table)
        .middleware(TokenAuthMiddleware::new(user_from_claims));
    let user = me::register(user, service)?;

    Ok(UserModules {
        authentication,
        user,
    })
}

#[derive(Clone, Deserialize, Serialize, JsonSchema, sqlx::FromRow, TableEntity)]
#[table(name = "users")]
struct UserRow {
    #[entity(primary_key, auto_increment)]
    id: i64,
    #[entity(max_length = 64, unique)]
    username: String,
    #[entity(max_length = 255)]
    #[serde(skip_serializing)]
    #[schemars(skip)]
    password_hash: String,
    #[entity(max_length = 16)]
    status: String,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
struct UserView {
    id: i64,
    username: String,
    status: String,
    created_at: i64,
    updated_at: i64,
}

#[derive(Clone)]
struct UserService {
    pool: Arc<MySqlPool>,
    table: Arc<TableConfig>,
    system_roles: Arc<[String]>,
    security: Arc<SecuritySettings>,
}

impl UserService {
    fn new(pool: Arc<MySqlPool>, table: Arc<TableConfig>, security: Arc<SecuritySettings>) -> Self {
        Self {
            pool,
            table,
            system_roles: Arc::from(vec![SYSTEM_ROLE.to_string()]),
            security,
        }
    }

    async fn register(&self, username: &str, plain_password: &str) -> Result<UserView, BaseError> {
        let username = self.normalize_username(username)?;
        self.validate_password(plain_password)?;
        if self.find_by_username(&username).await?.is_some() {
            return Err(BaseError::ParamInvalid(
                "username".to_string(),
                "用户名已存在".to_string(),
            ));
        }
        let password_hash = hash_password(plain_password)?;
        let user = self.insert(&username, &password_hash).await?;
        Ok(UserView::from(&user))
    }

    async fn authenticate(
        &self,
        username: &str,
        plain_password: &str,
    ) -> Result<UserRow, BaseError> {
        let username = self.normalize_username(username)?;
        let user = self
            .find_by_username(&username)
            .await?
            .ok_or(BaseError::InvalidPassword)?;
        if !verify_password(plain_password, &user.password_hash)? {
            return Err(BaseError::InvalidPassword);
        }
        self.ensure_active(user)
    }

    async fn active_by_subject(&self, subject: &str) -> Result<UserRow, BaseError> {
        let id = subject
            .parse::<i64>()
            .map_err(|_| BaseError::Unauthorized("Token subject 无效".to_string()))?;
        let user = self
            .find_by_id(id)
            .await?
            .ok_or_else(|| BaseError::UserNotFound(id.to_string()))?;
        self.ensure_active(user)
    }

    async fn view_by_id(&self, id: i64) -> Result<UserView, BaseError> {
        let user = self
            .find_by_id(id)
            .await?
            .ok_or_else(|| BaseError::UserNotFound(id.to_string()))?;
        let user = self.ensure_active(user)?;
        Ok(UserView::from(&user))
    }

    fn query(&self) -> TableQuery {
        TableQuery::new(
            Arc::clone(&self.table),
            Arc::clone(&self.system_roles),
            Some(Arc::clone(&self.pool)),
        )
    }

    async fn find_by_username(&self, username: &str) -> Result<Option<UserRow>, BaseError> {
        let rows = self
            .query()
            .where_eq("username", Value::String(username.to_string()))?
            .select::<UserRow>()
            .await?;
        Ok(rows.into_iter().next())
    }

    async fn find_by_id(&self, id: i64) -> Result<Option<UserRow>, BaseError> {
        let rows = self
            .query()
            .where_eq("id", Value::Number(id.into()))?
            .select::<UserRow>()
            .await?;
        Ok(rows.into_iter().next())
    }

    async fn insert(&self, username: &str, password_hash: &str) -> Result<UserRow, BaseError> {
        let data = HashMap::from([
            ("username".to_string(), Value::String(username.to_string())),
            (
                "password_hash".to_string(),
                Value::String(password_hash.to_string()),
            ),
            ("status".to_string(), Value::String("active".to_string())),
        ]);
        let (_, id) = match self.query().insert_returning_id(data).await {
            Ok(result) => result,
            Err(BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_))) => {
                return Err(BaseError::ParamInvalid(
                    "username".to_string(),
                    "用户名已存在".to_string(),
                ));
            }
            Err(error) => return Err(error),
        };
        let id = i64::try_from(id)
            .map_err(|_| BaseError::Unknown("用户主键超出 i64 范围".to_string()))?;
        self.find_by_id(id)
            .await?
            .ok_or_else(|| BaseError::UserNotFound(id.to_string()))
    }

    fn ensure_active(&self, user: UserRow) -> Result<UserRow, BaseError> {
        if user.status != "active" {
            return Err(BaseError::Unauthorized("用户已停用".to_string()));
        }
        Ok(user)
    }

    fn normalize_username(&self, username: &str) -> Result<String, BaseError> {
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

    fn validate_password(&self, password: &str) -> Result<(), BaseError> {
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

impl From<&UserRow> for UserView {
    fn from(user: &UserRow) -> Self {
        Self {
            id: user.id,
            username: user.username.clone(),
            status: user.status.clone(),
            created_at: user.created_at,
            updated_at: user.updated_at,
        }
    }
}

fn user_table_config() -> Result<Arc<TableConfig>, BaseError> {
    let mut config = UserRow::table_config()
        .clone()
        .display_name("用户")
        .timestamps(true, true, false);
    let protected = HashSet::from([SYSTEM_ROLE.to_string()]);
    let password = config
        .fields
        .get_mut("password_hash")
        .ok_or_else(|| BaseError::ConfigError("UserRow 缺少 password_hash 字段配置".to_string()))?;
    password.permissions = FieldPermissions {
        readable_roles: protected.clone(),
        writable_roles: protected.clone(),
        filterable_roles: protected.clone(),
        sortable_roles: protected,
    };
    password.filterable = false;
    password.sortable = false;
    Ok(Arc::new(config))
}

fn hash_password(password: &str) -> Result<String, BaseError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|value| value.to_string())
        .map_err(|_| BaseError::Unknown("密码哈希失败".to_string()))
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

fn user_from_claims(claims: &TokenClaims) -> User {
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
    User::new(id, username).with_roles(roles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_schema_uses_generated_id_and_protects_password_hash() {
        let config =
            user_table_config().unwrap_or_else(|error| panic!("用户表配置应有效: {error}"));
        let id = config
            .fields
            .get("id")
            .unwrap_or_else(|| panic!("应存在 id 字段"));
        let password = config
            .fields
            .get("password_hash")
            .unwrap_or_else(|| panic!("应存在 password_hash 字段"));

        assert!(id.auto_increment);
        assert!(!password.filterable);
        assert!(!password.sortable);
        assert_eq!(password.permissions.readable_roles.len(), 1);
    }

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

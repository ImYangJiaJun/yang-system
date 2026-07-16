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
use serde::Serialize;
use serde_json::Value;
use sqlx::MySqlPool;
use std::sync::Arc;
use yang_base::action::{TokenAuthMiddleware, User};
use yang_base::router::ModuleRouter;
use yang_base::table::{Field, Record, Table, TableDefinition, TableHandle, TableQuery};
use yang_base::token::TokenClaims;
use yang_base::BaseError;

const SYSTEM_ROLE: &str = "system";
const USER_ID: &str = "id";
const USERNAME: &str = "username";
const PASSWORD_HASH: &str = "password_hash";
const STATUS: &str = "status";
const CREATED_AT: &str = "created_at";
const UPDATED_AT: &str = "updated_at";
const USER_VIEW_FIELDS: &[&str] = &[USER_ID, USERNAME, STATUS, CREATED_AT, UPDATED_AT];
const USER_CREDENTIAL_FIELDS: &[&str] = &[USER_ID, USERNAME, PASSWORD_HASH, STATUS];

/// 构建单一用户领域模块。
///
/// `TokenAuthMiddleware` 只作用于受保护 Action，因此注册、登录、刷新、登出与
/// 当前用户接口可以共享同一份领域服务、表定义和运行时路由器。
pub fn build_module(
    pool: Arc<MySqlPool>,
    security: Arc<SecuritySettings>,
) -> Result<ModuleRouter, BaseError> {
    let table = user_table_definition()?;
    let service = Arc::new(UserService::new(table.bind(pool), security));

    ModuleRouter::new("user", "用户管理")
        .table(table)
        .middleware(TokenAuthMiddleware::new(user_from_claims))
        .apis([
            register::api(Arc::clone(&service)),
            login::api(Arc::clone(&service)),
            refresh::api(Arc::clone(&service)),
            logout::api(),
            me::api(service),
        ])
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
    table: TableHandle,
    security: Arc<SecuritySettings>,
}

impl UserService {
    fn new(table: TableHandle, security: Arc<SecuritySettings>) -> Self {
        Self { table, security }
    }

    async fn register(&self, username: &str, plain_password: &str) -> Result<UserView, BaseError> {
        let username = self.normalize_username(username)?;
        self.validate_password(plain_password)?;
        if self.username_exists(&username).await? {
            return Err(BaseError::ParamInvalid(
                "username".to_string(),
                "用户名已存在".to_string(),
            ));
        }
        let password_hash = hash_password(plain_password)?;
        let user = self.insert(&username, &password_hash).await?;
        UserView::try_from(&user)
    }

    async fn authenticate(
        &self,
        username: &str,
        plain_password: &str,
    ) -> Result<Record, BaseError> {
        let username = self.normalize_username(username)?;
        let user = self
            .find_credentials_by_username(&username)
            .await?
            .ok_or(BaseError::InvalidPassword)?;
        let password_hash: String = user.require(PASSWORD_HASH)?;
        if !verify_password(plain_password, &password_hash)? {
            return Err(BaseError::InvalidPassword);
        }
        self.ensure_active(&user)?;
        Ok(user)
    }

    async fn active_by_subject(&self, subject: &str) -> Result<Record, BaseError> {
        let id = subject
            .parse::<i64>()
            .map_err(|_| BaseError::Unauthorized("Token subject 无效".to_string()))?;
        let user = self
            .find_by_id(id)
            .await?
            .ok_or_else(|| BaseError::UserNotFound(id.to_string()))?;
        self.ensure_active(&user)?;
        Ok(user)
    }

    async fn view_by_id(&self, id: i64) -> Result<UserView, BaseError> {
        let user = self
            .find_by_id(id)
            .await?
            .ok_or_else(|| BaseError::UserNotFound(id.to_string()))?;
        self.ensure_active(&user)?;
        UserView::try_from(&user)
    }

    fn query(&self) -> TableQuery {
        self.table.query([SYSTEM_ROLE])
    }

    async fn username_exists(&self, username: &str) -> Result<bool, BaseError> {
        let rows = self
            .query()
            .select_fields(&[USER_ID])?
            .where_eq(USERNAME, Value::String(username.to_string()))?
            .all()
            .await?;
        Ok(!rows.is_empty())
    }

    async fn find_credentials_by_username(
        &self,
        username: &str,
    ) -> Result<Option<Record>, BaseError> {
        let rows = self
            .query()
            .select_fields(USER_CREDENTIAL_FIELDS)?
            .where_eq(USERNAME, Value::String(username.to_string()))?
            .all()
            .await?;
        Ok(rows.into_iter().next())
    }

    async fn find_by_id(&self, id: i64) -> Result<Option<Record>, BaseError> {
        let rows = self
            .query()
            .select_fields(USER_VIEW_FIELDS)?
            .where_eq(USER_ID, Value::Number(id.into()))?
            .all()
            .await?;
        Ok(rows.into_iter().next())
    }

    async fn insert(&self, username: &str, password_hash: &str) -> Result<Record, BaseError> {
        let record = Record::new()
            .set(USERNAME, username)
            .set(PASSWORD_HASH, password_hash)
            .set(STATUS, "active");
        let (_, id) = match self.query().insert_returning_id(record).await {
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

    fn ensure_active(&self, user: &Record) -> Result<(), BaseError> {
        let status: String = user.require(STATUS)?;
        if status != "active" {
            return Err(BaseError::Unauthorized("用户已停用".to_string()));
        }
        Ok(())
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

impl TryFrom<&Record> for UserView {
    type Error = BaseError;

    fn try_from(user: &Record) -> Result<Self, Self::Error> {
        Ok(Self {
            id: user.require(USER_ID)?,
            username: user.require(USERNAME)?,
            status: user.require(STATUS)?,
            created_at: user.require(CREATED_AT)?,
            updated_at: user.require(UPDATED_AT)?,
        })
    }
}

fn user_table_definition() -> Result<TableDefinition, BaseError> {
    Table::new("users")
        .label("用户")
        .fields([
            Field::id(USER_ID),
            Field::string(USERNAME, 64).required().unique(),
            Field::string(PASSWORD_HASH, 255)
                .required()
                .secret()
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
            Field::string(STATUS, 16).required(),
            Field::created_at(CREATED_AT),
            Field::updated_at(UPDATED_AT),
        ])
        .build()
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
        let definition =
            user_table_definition().unwrap_or_else(|error| panic!("用户表定义应有效: {error}"));
        let id = definition
            .field(USER_ID)
            .unwrap_or_else(|| panic!("应存在 id 字段"));
        let password = definition
            .field(PASSWORD_HASH)
            .unwrap_or_else(|| panic!("应存在 password_hash 字段"));

        assert_eq!(definition.name(), "users");
        assert_eq!(definition.primary_key(), USER_ID);
        assert!(id.is_auto_increment());
        assert!(!password.is_filterable());
        assert!(!password.is_sortable());
    }

    #[test]
    fn user_view_does_not_serialize_password_hash_from_record() {
        let record = Record::new()
            .set(USER_ID, 7)
            .set(USERNAME, "alice")
            .set(PASSWORD_HASH, "secret-hash")
            .set(STATUS, "active")
            .set(CREATED_AT, 10)
            .set(UPDATED_AT, 11);

        let view = UserView::try_from(&record)
            .unwrap_or_else(|error| panic!("完整记录应转换为用户视图: {error}"));
        let value = serde_json::to_value(view)
            .unwrap_or_else(|error| panic!("用户视图应可序列化: {error}"));

        assert_eq!(value.get(USERNAME), Some(&serde_json::json!("alice")));
        assert!(value.get(PASSWORD_HASH).is_none());
    }

    #[tokio::test]
    async fn password_hash_is_only_readable_by_system_role() {
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"));
        let definition =
            user_table_definition().unwrap_or_else(|error| panic!("用户表定义应有效: {error}"));
        let table = definition.bind(Arc::new(pool));

        let denied = table.query(["user"]).select_fields(&[PASSWORD_HASH]);
        assert!(matches!(
            denied,
            Err(BaseError::FieldPermissionDenied(_, field, _)) if field == PASSWORD_HASH
        ));
        assert!(table
            .query([SYSTEM_ROLE])
            .select_fields(&[PASSWORD_HASH])
            .is_ok());
    }

    #[test]
    fn user_view_rejects_incomplete_or_invalid_record() {
        let incomplete = Record::new().set(USER_ID, 7);
        assert!(UserView::try_from(&incomplete).is_err());

        let invalid = Record::new()
            .set(USER_ID, "not-an-integer")
            .set(USERNAME, "alice")
            .set(STATUS, "active")
            .set(CREATED_AT, 10)
            .set(UPDATED_AT, 11);
        assert!(UserView::try_from(&invalid).is_err());
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

//! 用户与认证领域模块。

mod login;
mod logout;
mod me;
mod refresh;
mod register;
mod register_via_plugin;

use crate::config::SecuritySettings;
use schemars::JsonSchema;
use serde::Serialize;
use std::sync::Arc;
use yang_base::action::{TokenAuthMiddleware, UiCatalogAction, User};
use yang_base::definition::{Key, ModuleName, ModuleSpec, Str, TableName, TableSpec, Timestamp};
#[cfg(test)]
use yang_base::table::TableDefinition;
use yang_base::table::{Record, TableQuery};
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

/// 构建单一用户领域模块。
///
/// `TokenAuthMiddleware` 只作用于受保护 Action，因此注册、登录、刷新、登出与
/// 当前用户接口可以共享同一份领域服务、表定义和运行时路由器。
pub fn build_module(security: Arc<SecuritySettings>) -> Result<ModuleSpec, BaseError> {
    let table_spec = user_table_spec()?;
    let service = Arc::new(UserService::new(security));
    let module_name = ModuleName::new("account.user")
        .map_err(|error| BaseError::ConfigError(error.to_string()))?;
    let module = ModuleSpec::new(module_name)
        .table(table_spec)
        .middleware(TokenAuthMiddleware::new(user_from_claims).authenticate_public_actions())
        .native_action(UiCatalogAction);
    let module = register::register(module, Arc::clone(&service))?;
    let module = register_via_plugin::register(module)?;
    let module = login::register(module, Arc::clone(&service))?;
    let module = refresh::register(module, Arc::clone(&service))?;
    let module = logout::register(module)?;
    let module = me::register(module, service)?;

    Ok(module)
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
    security: Arc<SecuritySettings>,
}

impl UserService {
    fn new(security: Arc<SecuritySettings>) -> Self {
        Self { security }
    }

    fn query(&self, ctx: &yang_base::action::ActionContext) -> Result<TableQuery, BaseError> {
        ctx.table_query()
    }

    async fn find_by_id(
        &self,
        ctx: &yang_base::action::ActionContext,
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

fn user_table_spec() -> Result<TableSpec, BaseError> {
    let fields = yang_base::fields! {
        id => Key::new().title("ID"),
        username => Str::new()
                .title("用户名")
                .require(true)
                .max_length(64)
                .unique(true),
        password_hash => Str::new()
                .title("密码摘要")
                .require(true)
                .max_length(255)
                .secret(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
        status => Str::new().title("状态").require(true).max_length(16),
        created_at => Timestamp::new().title("创建时间").created_at(),
        updated_at => Timestamp::new().title("更新时间").updated_at(),
    };
    let table_name =
        TableName::new("users").map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(TableSpec::new(table_name).title("用户").fields(fields))
}

#[cfg(test)]
fn user_table_definition() -> Result<TableDefinition, BaseError> {
    user_table_spec()?.table_definition()
}

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
    fn token_claims_project_roles_and_permissions_without_trusting_other_shapes() {
        let claims = TokenClaims::new(
            "test",
            "7",
            "test-api",
            60,
            0,
            0,
            "test-jti",
            yang_base::token::TokenType::Access,
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

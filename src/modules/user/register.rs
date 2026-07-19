use super::{UserService, UserView, PASSWORD_HASH, STATUS, USERNAME, USER_ID};
use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHasher};
use async_trait::async_trait;
use rand_core::OsRng;
use serde_json::Value;
use std::sync::Arc;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{ModuleSpec, Password, Str};
use yang_base::table::Record;
use yang_base::{Action, BaseError};

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) RegisterInput {
        username: Str::new().title("用户名").require(true).max_length(64),
        password: Password::new().title("登录密码").require(true).min_length(10).max_length(128),
    }
}

#[derive(Action)]
#[action(
    name = "register",
    display_name = "注册用户",
    description = "创建一个新用户",
    method = "POST",
    path = "/api/v1/users/register",
    success_status = 201,
    public
)]
struct RegisterAction {
    service: Arc<UserService>,
}

impl UserService {
    async fn register(
        &self,
        ctx: &ActionContext,
        username: &str,
        plain_password: &str,
    ) -> Result<UserView, BaseError> {
        let username = self.normalize_username(username)?;
        self.validate_password(plain_password)?;
        if self.username_exists(ctx, &username).await? {
            return Err(username_exists_error());
        }
        let password_hash = hash_password(plain_password)?;
        let user = self.insert(ctx, &username, &password_hash).await?;
        UserView::try_from(&user)
    }

    async fn username_exists(
        &self,
        ctx: &ActionContext,
        username: &str,
    ) -> Result<bool, BaseError> {
        let rows = self
            .query(ctx)?
            .select_fields(&[USER_ID])?
            .where_eq(USERNAME, Value::String(username.to_string()))?
            .all()
            .await?;
        Ok(!rows.is_empty())
    }

    async fn insert(
        &self,
        ctx: &ActionContext,
        username: &str,
        password_hash: &str,
    ) -> Result<Record, BaseError> {
        let record = Record::new()
            .set(USERNAME, username)
            .set(PASSWORD_HASH, password_hash)
            .set(STATUS, "active");
        let (_, id) = match self.query(ctx)?.insert_returning_id(record).await {
            Ok(result) => result,
            Err(BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_))) => {
                return Err(username_exists_error());
            }
            Err(error) => return Err(error),
        };
        let id = i64::try_from(id)
            .map_err(|_| BaseError::Unknown("用户主键超出 i64 范围".to_string()))?;
        self.find_by_id(ctx, id)
            .await?
            .ok_or_else(|| BaseError::UserNotFound(id.to_string()))
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

fn username_exists_error() -> BaseError {
    BaseError::ParamInvalid("username".to_string(), "用户名已存在".to_string())
}

pub(super) fn hash_password(password: &str) -> Result<String, BaseError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|value| value.to_string())
        .map_err(|_| BaseError::Unknown("密码哈希失败".to_string()))
}

#[async_trait]
impl ActionHandler for RegisterAction {
    type Input = RegisterInput;
    type Output = UserView;

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        self.service
            .register(&ctx, &input.username, &input.password)
            .await
    }
}

pub(super) fn register(
    module: ModuleSpec,
    service: Arc<UserService>,
) -> Result<ModuleSpec, BaseError> {
    Ok(module.native_action(RegisterAction { service }))
}

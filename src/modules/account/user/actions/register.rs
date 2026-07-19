use super::super::schema::UserView;
use super::super::service::UserService;
use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHasher};
use async_trait::async_trait;
use rand_core::OsRng;
use std::sync::Arc;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{ModuleSpec, Password, Str};
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
        if self.credentials().username_exists(ctx, &username).await? {
            return Err(username_exists_error());
        }
        let password_hash = hash_password(plain_password)?;
        let id = match self
            .credentials()
            .insert(ctx, &username, &password_hash)
            .await
        {
            Ok(id) => id,
            Err(BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_))) => {
                return Err(username_exists_error());
            }
            Err(error) => return Err(error),
        };
        let user = self
            .find_by_id(ctx, id)
            .await?
            .ok_or_else(|| BaseError::UserNotFound(id.to_string()))?;
        UserView::try_from(&user)
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

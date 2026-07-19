use super::super::policy::{
    normalize_username, validate_password, PASSWORD_MAX_LENGTH, PASSWORD_MIN_LENGTH,
    USERNAME_MAX_LENGTH, USERNAME_MIN_LENGTH, USERNAME_PATTERN,
};
use super::super::rate_limit::AuthOperation;
use super::super::schema::UserView;
use super::super::service::UserService;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{ModuleSpec, Password, Str};
use yang_base::{Action, BaseError};

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) RegisterInput {
        username: Str::new()
            .title("用户名")
            .require(true)
            .min_length(USERNAME_MIN_LENGTH)
            .max_length(USERNAME_MAX_LENGTH)
            .pattern(USERNAME_PATTERN),
        password: Password::new()
            .title("登录密码")
            .require(true)
            .min_length(PASSWORD_MIN_LENGTH)
            .max_length(PASSWORD_MAX_LENGTH),
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
        let username = normalize_username(username)?;
        validate_password(plain_password)?;
        self.rate_limiter()
            .check(ctx, AuthOperation::Register, &username)
            .await?;
        if self.credentials().username_exists(ctx, &username).await? {
            return Err(username_exists_error());
        }
        let password_hash = self.passwords().hash(plain_password).await?;
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

use super::super::policy::{
    PASSWORD_MAX_LENGTH, PASSWORD_MIN_LENGTH, USERNAME_MAX_LENGTH, USERNAME_MIN_LENGTH,
    USERNAME_PATTERN,
};
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
        email: Str::new()
            .title("注册邮箱")
            .require(true)
            .max_length(254)
            .email(),
        email_code: Str::new()
            .title("邮箱验证码")
            .require(true)
            .min_length(6)
            .max_length(6)
            .pattern(r"^[0-9]{6}$"),
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
            .register(
                &ctx,
                &input.username,
                &input.password,
                &input.email,
                &input.email_code,
            )
            .await
    }
}

pub(super) fn register(
    module: ModuleSpec,
    service: Arc<UserService>,
) -> Result<ModuleSpec, BaseError> {
    Ok(module.native_action(RegisterAction { service }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn registration_contract_requires_email_ownership_proof() {
        let params = <RegisterInput as ParamInput>::params();
        let names = params
            .as_slice()
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, ["username", "password", "email", "email_code"]);
        assert!(params.as_slice().iter().all(|param| param.required));
    }
}

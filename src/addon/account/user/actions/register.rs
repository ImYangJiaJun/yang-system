//! 创建一个新用户。

use super::super::domain::policy::{
    PASSWORD_MAX_LENGTH, PASSWORD_MIN_LENGTH, USERNAME_MAX_LENGTH, USERNAME_MIN_LENGTH,
    USERNAME_PATTERN,
};
use super::super::domain::schema::UserView;
use super::super::domain::service::UserService;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{Password, Str};
use yang_base::BaseError;

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

pub(super) async fn handle(
    ctx: ActionContext,
    input: RegisterInput,
    service: Arc<UserService>,
) -> Result<UserView, BaseError> {
    service
        .register(
            &ctx,
            &input.username,
            &input.password,
            &input.email,
            &input.email_code,
        )
        .await
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

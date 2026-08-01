use super::super::policy::{PASSWORD_MAX_LENGTH, PASSWORD_MIN_LENGTH};
use super::super::service::UserService;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::{Action as ActionHandler, ActionContext, ApiResponse};
use yang_base::definition::{ModuleSpec, Password};
use yang_base::{Action, BaseError};

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) ChangePasswordInput {
        old_password: Password::new()
            .title("当前密码")
            .require(true)
            .min_length(1)
            .max_length(PASSWORD_MAX_LENGTH),
        new_password: Password::new()
            .title("新密码")
            .require(true)
            .min_length(PASSWORD_MIN_LENGTH)
            .max_length(PASSWORD_MAX_LENGTH),
    }
}

#[derive(Action)]
#[action(
    name = "change_password",
    display_name = "修改密码",
    description = "校验当前密码并使已有会话失效",
    method = "POST",
    path = "/api/v1/users/change-password"
)]
struct ChangePasswordAction {
    service: Arc<UserService>,
}

#[async_trait]
impl ActionHandler for ChangePasswordAction {
    type Input = ChangePasswordInput;
    type Output = ApiResponse;

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let secure = super::super::browser_session::validate_same_origin(&ctx.request)?;
        let user_id = ctx
            .authenticated_user()
            .ok_or_else(|| BaseError::Unauthorized("需要登录".to_string()))?
            .id;
        self.service
            .change_password(&ctx, user_id, &input.old_password, &input.new_password)
            .await?;
        change_password_response(secure)
    }
}

fn change_password_response(secure: bool) -> Result<ApiResponse, BaseError> {
    super::super::browser_session::relogin_response("密码已修改，请重新登录", secure)
}

pub(super) fn register(
    module: ModuleSpec,
    service: Arc<UserService>,
) -> Result<ModuleSpec, BaseError> {
    Ok(module.native_action(ChangePasswordAction { service }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_rejects_client_supplied_target_user() {
        let injected = serde_json::from_value::<ChangePasswordInput>(serde_json::json!({
            "user_id": 99,
            "old_password": "current-password",
            "new_password": "replacement-password"
        }));
        assert!(injected.is_err());
    }

    #[test]
    fn success_requires_relogin_and_clears_refresh_cookie_without_secrets() {
        let response = change_password_response(true)
            .unwrap_or_else(|error| panic!("改密响应应可构建: {error}"));
        assert_eq!(
            response.data,
            Some(serde_json::json!({ "relogin_required": true }))
        );
        let headers = response.response_headers();
        assert!(headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("set-cookie")
                && value.contains("yang_refresh=;")
                && value.contains("Max-Age=0")
                && value.contains("HttpOnly")
                && value.contains("Secure")
        }));
        assert!(headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("cache-control") && value == "no-store"
        }));
        let serialized = serde_json::to_string(&response)
            .unwrap_or_else(|error| panic!("响应应可序列化: {error}"));
        assert!(!serialized.contains("password"));
        assert!(!serialized.contains("token"));
    }
}

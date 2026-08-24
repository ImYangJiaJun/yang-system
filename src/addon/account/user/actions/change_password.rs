//! 校验当前密码并使已有会话失效。

use crate::addon::account::domain::policy::{
    validate_new_password, PASSWORD_MAX_LENGTH, PASSWORD_MIN_LENGTH,
};
use crate::addon::account::{Account, LockedUserCredential};
use crate::audit;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::auth::{AuthOperation, BrowserSession};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Password};
use yang_base::BaseError;

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

pub(super) async fn handle(
    ctx: ActionContext,
    input: ChangePasswordInput,
    account: Arc<Account>,
) -> Result<ApiResponse, BaseError> {
    let secure = BrowserSession::validate_same_origin(&ctx.request)?;
    let user_id = ctx
        .authenticated_user()
        .ok_or_else(|| BaseError::Unauthorized("需要登录".to_string()))?
        .id;
    if !account.credential_mutations_enabled() {
        return Err(BaseError::ConfigError(
            "改密能力必须在全部实例开启 Refresh 凭据版本签发后启用".to_string(),
        ));
    }
    validate_new_password(&input.new_password)?;
    account
        .rate_limiter()
        .check(&ctx, AuthOperation::ChangePassword, &user_id.to_string())
        .await?;

    let observed = account
        .users()
        .find_credentials_by_id(&ctx, user_id)
        .await?
        .ok_or_else(|| BaseError::UserNotFound(user_id.to_string()))?;
    Account::ensure_active(observed.status)?;
    if !account
        .passwords()
        .verify(&input.old_password, &observed.password_hash)
        .await?
    {
        return Err(BaseError::InvalidPassword);
    }
    // 两次昂贵 Argon2 运算均在事务和用户行锁之外完成。
    let new_password_hash = account.passwords().hash(&input.new_password).await?;

    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let locked = account
            .lock_credential_in_tx(&ctx, &mut transaction, user_id)
            .await?;
        Account::ensure_active(locked.status())?;
        ensure_password_hash_unchanged(&locked, &observed.password_hash)?;
        account
            .users()
            .update_password_hash_in_tx(&ctx, &mut transaction, user_id, &new_password_hash)
            .await?;
        Account::increment_versions_in_tx(&mut transaction, &locked).await?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", user_id)?),
            audit::entity("user", user_id)?,
            None,
            Some(audit::summary([("relogin_required", json!(true))])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;
    Account::finish_transaction(transaction, result).await?;

    change_password_response(secure)
}

fn change_password_response(secure: bool) -> Result<ApiResponse, BaseError> {
    Account::browser_session().relogin_response("密码已修改，请重新登录", secure)
}

fn ensure_password_hash_unchanged(
    locked: &LockedUserCredential,
    observed_password_hash: &str,
) -> Result<(), BaseError> {
    if locked.password_hash_matches(observed_password_hash) {
        return Ok(());
    }
    Err(BaseError::ParamInvalid(
        "old_password".to_string(),
        "密码已被其他请求修改，请重新登录后重试".to_string(),
    ))
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    // 发布开关关闭时不注册。
    if !account.credential_mutations_enabled() {
        return module;
    }
    module
        .action_fn(
            yang_base::action_name!("change_password"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(HttpMethod::Post, "/api/v1/users/change-password")
        .display_name("修改密码")
        .description("校验当前密码并使已有会话失效")
        .register()
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

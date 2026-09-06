//! 请求向新邮箱发送换绑验证码（需登录 + Step-up）。

use crate::addon::account::Account;
use crate::config::ChangeEmailVerificationConfig;
use schemars::JsonSchema;
use serde::Serialize;
use std::sync::Arc;
use yang_base::action::auth::{normalize_email, RegistrationEmailVerification};
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) RequestChangeEmailInput {
        new_email: Str::new()
            .title("新邮箱")
            .require(true)
            .max_length(254)
            .email(),
    }
}

/// 请求被接受后的统一响应（不暴露邮箱是否可注册）。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(super) struct ChangeEmailCodeAccepted {
    /// 请求已被接受（不保证邮件真实投递）。
    pub accepted: bool,
    /// 验证码有效期（秒）。
    pub expires_in: u64,
    /// 重发冷却（秒）。
    pub resend_after: u64,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: RequestChangeEmailInput,
    _account: Arc<Account>,
) -> Result<ChangeEmailCodeAccepted, BaseError> {
    // 换绑验证码使用独立 config 槽（与注册验证码 key 域/密钥隔离）。
    let change_config = ctx
        .tools()
        .config::<ChangeEmailVerificationConfig>()?
        .engine_config();
    let email = normalize_email(&input.new_email)?;
    let verification = RegistrationEmailVerification::from_config(change_config)?;
    let accepted = verification.request(&ctx, &email, true).await?;
    Ok(ChangeEmailCodeAccepted {
        accepted: accepted.accepted,
        expires_in: accepted.expires_in,
        resend_after: accepted.resend_after,
    })
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("request_change_email"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(HttpMethod::Post, "/api/v1/users/change-email-verifications")
        .display_name("请求换绑验证码")
        .description("向新邮箱发送一次性换绑验证码（独立验证码 key 域）")
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_requires_single_email_field() {
        let params = <RequestChangeEmailInput as ParamInput>::params();
        let names = params
            .as_slice()
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["new_email"]);
        assert!(params.as_slice().iter().all(|param| param.required));
    }
}

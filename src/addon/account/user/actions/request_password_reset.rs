//! 自助密码找回签发端：防枚举统一响应，仅在邮箱对应启用用户时签发并投递短期重置凭证。

use crate::addon::account::domain::email_delivery::{
    PasswordResetEmailSenderHandle, PasswordResetLinkConfig,
};
use crate::addon::account::Account;
use schemars::JsonSchema;
use serde::Serialize;
use std::sync::Arc;
use yang_base::action::auth::{normalize_email, AuthOperation};
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) RequestPasswordResetInput {
        email: Str::new()
            .title("账号邮箱")
            .require(true)
            .max_length(254)
            .email(),
    }
}

/// 请求被接受后的统一响应（不暴露邮箱是否注册）。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(super) struct RequestPasswordResetAccepted {
    /// 请求已被接受（不保证邮件真实投递）。
    pub accepted: bool,
    /// 重置凭证有效期（秒）。
    pub expires_in: u64,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: RequestPasswordResetInput,
    account: Arc<Account>,
) -> Result<RequestPasswordResetAccepted, BaseError> {
    let email = normalize_email(&input.email)?;
    // 无论邮箱是否注册都先消耗同一限流维度，避免绕过与枚举差异。
    account
        .rate_limiter()
        .check(&ctx, AuthOperation::PasswordResetCreate, &email)
        .await?;
    let ttl_seconds = account.password_reset_ttl_seconds();
    // 邮箱不存在或用户已停用时只消耗限额、不签发不投递，响应与成功路径一致。
    let Some(user_id) = account
        .users()
        .find_active_user_by_email(&ctx, &email)
        .await?
    else {
        return Ok(accepted(ttl_seconds));
    };
    let issued = account.issue_password_reset(&ctx, user_id).await?;
    let link = ctx.tools().config::<PasswordResetLinkConfig>()?;
    let sender = ctx.tools().extension::<PasswordResetEmailSenderHandle>()?;
    // 投递失败只记日志，不改变响应，避免泄露邮箱存在性；明文凭证只进入邮件链接。
    if let Err(error) = sender
        .send_password_reset_link(&email, &link.reset_url(issued.raw_token()), ttl_seconds)
        .await
    {
        tracing::warn!(
            error = %error,
            reset_fingerprint = issued.fingerprint(),
            "密码重置邮件投递失败"
        );
    }
    Ok(accepted(ttl_seconds))
}

fn accepted(ttl_seconds: u64) -> RequestPasswordResetAccepted {
    RequestPasswordResetAccepted {
        accepted: true,
        expires_in: ttl_seconds,
    }
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("request_password_reset"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(HttpMethod::Post, "/api/v1/users/request-password-reset")
        .display_name("请求密码重置")
        .description("向已验证邮箱投递一次性密码重置链接；响应不暴露邮箱是否注册")
        .success_status(202)
        .public()
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_requires_a_single_email_field() {
        let params = <RequestPasswordResetInput as ParamInput>::params();
        let names = params
            .as_slice()
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, ["email"]);
        assert!(params.as_slice().iter().all(|param| param.required));
        // 输入不得携带身份字段：自助找回的目标只能由邮箱解析，防止客户端指定他人。
        let injected = serde_json::from_value::<RequestPasswordResetInput>(serde_json::json!({
            "email": "alice@example.com",
            "user_id": 99
        }));
        assert!(injected.is_err());
    }

    #[test]
    fn accepted_response_never_carries_existence_signal() {
        let response = accepted(900);
        assert!(response.accepted);
        assert_eq!(response.expires_in, 900);
        let serialized = serde_json::to_value(&response)
            .unwrap_or_else(|error| panic!("响应应可序列化: {error}"));
        assert_eq!(
            serialized,
            serde_json::json!({ "accepted": true, "expires_in": 900 })
        );
    }
}

//! 校验新邮箱验证码并完成换绑（需登录 + Step-up）。

use crate::addon::account::Account;
use crate::audit;
use crate::config::ChangeEmailVerificationConfig;
use serde_json::json;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use yang_base::action::auth::{normalize_email, RegistrationEmailVerification};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) ChangeEmailInput {
        new_email: Str::new()
            .title("新邮箱")
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
    input: ChangeEmailInput,
    account: Arc<Account>,
) -> Result<ApiResponse, BaseError> {
    let user_id = ctx
        .authenticated_user()
        .ok_or_else(|| BaseError::Unauthorized("需要登录".to_string()))?
        .id;
    if !account.credential_mutations_enabled() {
        return Err(BaseError::ConfigError(
            "邮箱换绑必须在全部实例开启 Refresh 凭据版本签发后启用".to_string(),
        ));
    }
    let new_email = normalize_email(&input.new_email)?;
    let change_config = ctx
        .tools()
        .config::<ChangeEmailVerificationConfig>()?
        .engine_config();
    // 验证码单次消费；任何失败返回统一无效验证码错误。
    RegistrationEmailVerification::from_config(change_config)?
        .consume(&ctx, &new_email, &input.email_code)
        .await?;
    let email_verified_at = current_unix_timestamp()?;

    let observed = account
        .users()
        .find_credentials_by_id(&ctx, user_id)
        .await?
        .ok_or_else(|| BaseError::UserNotFound(user_id.to_string()))?;
    Account::ensure_active(observed.status)?;

    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let locked = account
            .lock_credential_in_tx(&ctx, &mut transaction, user_id)
            .await?;
        Account::ensure_active(locked.status())?;
        account
            .users()
            .update_email_in_tx(
                &ctx,
                &mut transaction,
                user_id,
                &new_email,
                email_verified_at,
            )
            .await
            .map_err(|error| match error {
                BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_)) => {
                    BaseError::ParamInvalid(
                        "new_email".to_string(),
                        "该邮箱已被其他账号使用".to_string(),
                    )
                }
                other => other,
            })?;
        Account::increment_versions_in_tx(&mut transaction, &locked).await?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", user_id)?),
            audit::entity("user", user_id)?,
            None,
            Some(audit::summary([("email_changed", json!(true))])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;
    Account::finish_transaction(transaction, result).await?;

    // 凭据版本递增使旧 Refresh 失效；邮箱是登录标识（阶段 B-2 后），需重新登录。
    let immediate_convergence = account
        .converge_revocation(&ctx, user_id, "account.user.change_email", "user")
        .await?;

    ApiResponse::success(
        json!({
            "email_changed": true,
            "immediate_convergence": immediate_convergence,
            "relogin_required": true,
        }),
        if immediate_convergence {
            "邮箱已更换，请使用新邮箱重新登录"
        } else {
            "邮箱已更换，Redis 即时收敛待后台重试"
        },
    )
}

fn current_unix_timestamp() -> Result<i64, BaseError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| BaseError::ConfigError("系统时间早于 Unix epoch".to_string()))?
        .as_secs();
    i64::try_from(seconds).map_err(|_| BaseError::ConfigError("系统时间超出 i64 范围".to_string()))
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    // 发布开关关闭时不注册。
    if !account.credential_mutations_enabled() {
        return module;
    }
    module
        .action_fn(
            yang_base::action_name!("change_email"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(HttpMethod::Post, "/api/v1/users/change-email")
        .display_name("更换邮箱")
        .description("校验新邮箱验证码并完成换绑，撤销已有会话")
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_requires_email_and_code() {
        let params = <ChangeEmailInput as ParamInput>::params();
        let names = params
            .as_slice()
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["new_email", "email_code"]);
        assert!(params.as_slice().iter().all(|param| param.required));
    }

    #[test]
    fn input_rejects_client_supplied_target_user() {
        let injected = serde_json::from_value::<ChangeEmailInput>(serde_json::json!({
            "new_email": "bob@example.com",
            "email_code": "123456",
            "user_id": 99
        }));
        assert!(injected.is_err());
    }
}

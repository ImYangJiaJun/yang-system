//! 使用短期单次凭证重置密码；请求不依赖现有登录会话。

use crate::addon::account::domain::policy::{
    validate_new_password, PASSWORD_MAX_LENGTH, PASSWORD_MIN_LENGTH,
};
use crate::addon::account::{Account, PasswordResetReference};
use crate::audit;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::auth::{AuthOperation, BrowserSession};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Password, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) ResetPasswordInput {
        reset_token: Str::new()
            .title("密码重置凭证")
            .require(true)
            .min_length(1)
            .max_length(256),
        new_password: Password::new()
            .title("新密码")
            .require(true)
            .min_length(PASSWORD_MIN_LENGTH)
            .max_length(PASSWORD_MAX_LENGTH),
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: ResetPasswordInput,
    account: Arc<Account>,
) -> Result<ApiResponse, BaseError> {
    let secure = BrowserSession::validate_same_origin(&ctx.request)?;
    if !account.credential_mutations_enabled() {
        return Err(BaseError::ConfigError(
            "密码重置能力必须在全部实例开启 Refresh 凭据版本签发后启用".to_string(),
        ));
    }
    validate_new_password(&input.new_password)?;
    let attempt_fingerprint = PasswordResetReference::attempt_fingerprint(&input.reset_token)?;
    account
        .rate_limiter()
        .check(
            &ctx,
            AuthOperation::PasswordResetConsume,
            &attempt_fingerprint,
        )
        .await?;
    let reference = PasswordResetReference::parse(&input.reset_token)?;

    // 无论凭证是否存在，新摘要都在任何事务和行锁之前计算；全局/指纹限流约束成本。
    let new_password_hash = account.passwords().hash(&input.new_password).await?;
    let target_user_id = account
        .find_reset_target(&ctx, &reference)
        .await?
        .ok_or_else(Account::invalid_reset_token)?;

    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        // 所有创建与消费路径统一先锁用户、再锁凭证，避免同用户并发形成反向锁序。
        let locked_user = account
            .lock_credential_in_tx(&ctx, &mut transaction, target_user_id)
            .await?;
        Account::ensure_active(locked_user.status())?;
        let locked_reset = account
            .lock_reset_in_tx(&ctx, &mut transaction, &reference)
            .await?;
        if locked_reset.user_id() != target_user_id || !locked_reset.is_usable() {
            return Err(Account::invalid_reset_token());
        }
        account
            .users()
            .update_password_hash_in_tx(&ctx, &mut transaction, target_user_id, &new_password_hash)
            .await?;
        Account::increment_versions_in_tx(&mut transaction, &locked_user).await?;
        Account::consume_reset_in_tx(&mut transaction, &locked_reset).await?;
        let event = audit::succeeded_system_event(
            &ctx,
            format!("password-reset-{}", reference.fingerprint()),
            None,
            Some(audit::entity("user", target_user_id)?),
            audit::entity("password_reset", reference.fingerprint())?,
            None,
            Some(audit::summary([
                ("relogin_required", json!(true)),
                ("reset_fingerprint", json!(reference.fingerprint())),
                ("user_id", json!(target_user_id)),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;
    Account::finish_transaction(transaction, result).await?;

    Account::browser_session().relogin_response("密码已重置，请使用新密码登录", secure)
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    // 发布开关关闭时不注册。
    if !account.credential_mutations_enabled() {
        return module;
    }
    module
        .action_fn(
            yang_base::action_name!("reset_password"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(HttpMethod::Post, "/api/v1/users/reset-password")
        .display_name("重置密码")
        .description("消费短期单次凭证并使已有会话失效")
        .public()
        .register()
}

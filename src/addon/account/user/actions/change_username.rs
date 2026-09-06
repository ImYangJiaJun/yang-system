//! 修改当前用户名并撤销已有会话。

use crate::addon::account::domain::policy::{
    normalize_username, USERNAME_MAX_LENGTH, USERNAME_MIN_LENGTH, USERNAME_PATTERN,
};
use crate::addon::account::Account;
use crate::audit;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::auth::{AuthOperation, BrowserSession};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) ChangeUsernameInput {
        new_username: Str::new()
            .title("新用户名")
            .require(true)
            .min_length(USERNAME_MIN_LENGTH)
            .max_length(USERNAME_MAX_LENGTH)
            .pattern(USERNAME_PATTERN),
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: ChangeUsernameInput,
    account: Arc<Account>,
) -> Result<ApiResponse, BaseError> {
    let secure = BrowserSession::validate_same_origin(&ctx.request)?;
    let user_id = ctx
        .authenticated_user()
        .ok_or_else(|| BaseError::Unauthorized("需要登录".to_string()))?
        .id;
    if !account.credential_mutations_enabled() {
        return Err(BaseError::ConfigError(
            "修改用户名必须在全部实例开启 Refresh 凭据版本签发后启用".to_string(),
        ));
    }
    let new_username = normalize_username(&input.new_username)?;
    // 限流按用户 ID 计数，维度与改密一致（凭据写类操作的账号级保护）。
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

    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let locked = account
            .lock_credential_in_tx(&ctx, &mut transaction, user_id)
            .await?;
        Account::ensure_active(locked.status())?;
        let old_username = ctx
            .authenticated_user()
            .ok_or_else(|| BaseError::Unauthorized("需要登录".to_string()))?
            .username
            .clone();
        // 用户名唯一约束由数据库保证；冲突以约束错误返回，避免并发检查窗口。
        account
            .users()
            .update_username_in_tx(&ctx, &mut transaction, user_id, &new_username)
            .await
            .map_err(|error| match error {
                BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_)) => {
                    BaseError::ParamInvalid(
                        "new_username".to_string(),
                        "用户名已存在".to_string(),
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
            Some(audit::summary([("username", json!(old_username))])?),
            Some(audit::summary([("username", json!(new_username))])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;
    Account::finish_transaction(transaction, result).await?;

    // 用户名已变更，旧 Access Token 的 username claim 不再可信：全量撤销会话，
    // 与改密同语义要求重新登录（credential_version 递增使旧 Refresh 失效）。
    let immediate_convergence = account
        .converge_revocation(&ctx, user_id, "account.user.change_username", "user")
        .await?;

    Account::browser_session().clear_response(
        ApiResponse::success(
            json!({
                "username_changed": true,
                "immediate_convergence": immediate_convergence,
                "relogin_required": true,
            }),
            if immediate_convergence {
                "用户名已修改，请使用新用户名重新登录"
            } else {
                "用户名已修改，Redis 即时收敛待后台重试"
            },
        )?,
        secure,
    )
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    // 发布开关关闭时不注册。
    if !account.credential_mutations_enabled() {
        return module;
    }
    module
        .action_fn(
            yang_base::action_name!("change_username"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(HttpMethod::Post, "/api/v1/users/change-username")
        .display_name("修改用户名")
        .description("修改当前用户名并撤销已有会话")
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_requires_single_valid_username_field() {
        let params = <ChangeUsernameInput as ParamInput>::params();
        let names = params
            .as_slice()
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["new_username"]);
        assert!(params.as_slice().iter().all(|param| param.required));
    }

    #[test]
    fn input_rejects_client_supplied_target_user() {
        let injected = serde_json::from_value::<ChangeUsernameInput>(serde_json::json!({
            "new_username": "alice2",
            "user_id": 99
        }));
        assert!(injected.is_err());
    }
}

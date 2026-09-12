//! 匿名化删除当前账号（需登录 + Step-up + 二次确认，路线图 E-2b）。

use crate::addon::account::domain::status::UserStatus;
use crate::addon::account::Account;
use crate::audit;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::auth::BrowserSession;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) DeleteAccountInput {
        /// 二次确认文案：必须等于「delete my account」才执行。
        confirmation: Str::new()
            .title("确认文案")
            .require(true)
            .max_length(64),
    }
}

const CONFIRMATION_PHRASE: &str = "delete my account";

pub(super) async fn handle(
    ctx: ActionContext,
    input: DeleteAccountInput,
    account: Arc<Account>,
) -> Result<ApiResponse, BaseError> {
    let secure = BrowserSession::validate_same_origin(&ctx.request)?;
    let user_id = ctx
        .authenticated_user()
        .ok_or_else(|| BaseError::Unauthorized("需要登录".to_string()))?
        .id;
    if input.confirmation.trim() != CONFIRMATION_PHRASE {
        return Err(BaseError::ParamInvalid(
            "confirmation".to_string(),
            "二次确认文案不匹配".to_string(),
        ));
    }
    let deleted_username = format!("deleted_{user_id}");

    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let locked = account
            .lock_credential_in_tx(&ctx, &mut transaction, user_id)
            .await?;
        if !locked.status().is_active() {
            return Err(BaseError::PermissionDenied(
                "账号已停用或已删除".to_string(),
            ));
        }
        // FK 前置清理：作废该用户全部未消费重置凭证（匿名化后不允许再重置）。
        Account::invalidate_resets_in_tx(&mut transaction, user_id).await?;
        // 隐私清理：同事务删除头像行（匿名化后不得保留可识别图片）。
        account
            .avatars()
            .delete_in_tx(&ctx, &mut transaction, user_id)
            .await?;
        // 匿名化：username 改写保唯一、email 置 NULL 释放、status=deleted、双版本递增。
        Account::anonymize_locked_in_tx(&mut transaction, &locked, &deleted_username).await?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", user_id)?),
            audit::entity("user", user_id)?,
            Some(audit::summary([(
                "status",
                json!(UserStatus::Active.as_str()),
            )])?),
            Some(audit::summary([
                ("status", json!(UserStatus::Deleted.as_str())),
                ("anonymized", json!(true)),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;
    Account::finish_transaction(transaction, result).await?;

    // 双版本递增已使全部 Token/Refresh 失效；即时收敛 Redis 水位线。
    let immediate_convergence = account
        .converge_revocation(&ctx, user_id, "account.user.delete_account", "user")
        .await?;
    Account::browser_session().clear_response(
        ApiResponse::success(
            json!({
                "account_deleted": true,
                "immediate_convergence": immediate_convergence,
                "relogin_required": true,
            }),
            if immediate_convergence {
                "账号已匿名化删除，全部会话已失效"
            } else {
                "账号已匿名化删除，Redis 即时收敛待后台重试"
            },
        )?,
        secure,
    )
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    // 发布开关关闭时不注册（依赖双版本失效传播）。
    if !account.credential_mutations_enabled() {
        return module;
    }
    module
        .action_fn(
            yang_base::action_name!("delete_account"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(HttpMethod::Post, "/api/v1/users/delete")
        .display_name("删除账号")
        .description("匿名化删除当前账号（username 改写、email 释放、双版本失效）")
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_requires_confirmation_phrase() {
        let params = <DeleteAccountInput as ParamInput>::params();
        let names = params
            .as_slice()
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["confirmation"]);
        assert!(params.as_slice().iter().all(|param| param.required));
    }

    #[test]
    fn confirmation_phrase_is_exact() {
        assert_eq!(CONFIRMATION_PHRASE, "delete my account");
        for wrong in ["delete", "DELETE MY ACCOUNT", "delete account", ""] {
            assert!(wrong != CONFIRMATION_PHRASE, "{wrong:?} 必须不匹配");
        }
    }
}

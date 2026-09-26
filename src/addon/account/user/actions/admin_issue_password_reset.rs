//! 管理端为目标用户签发密码重置凭证（需独立权限 + Step-up，路线图 D-2）。
//!
//! **凭据签发与日常用户管理分属两个权限**：本 Action 要求
//! `account.users.reset_credentials`，而**不是**停用/启用共用的 `account.users.manage`。
//! 原因是一旦两者共用，持 `account.users.manage` 的非管理员就能给任意账号（含系统管理员）
//! 签发重置凭证、重置其口令并登录成他，一步拿到全部权限——`account.users.manage` 于是
//! 实质等价于 root。拆成独立权限后，这个危害面在权限粒度上可见、可被单独审计与收窄，
//! 不再和「日常启用/停用」混在同一个名字下。

use crate::addon::account::Account;
use crate::audit;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, Key, ModuleSpec};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) AdminIssuePasswordResetInput {
        #[param(source = path)]
        id: Key::new()
            .title("目标用户")
            .require(true),
    }
}

/// 管理签发响应：明文凭证只出现这一次。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(super) struct IssuedResetToken {
    /// 一次性重置凭证（明文只回显一次，禁止落库/日志）。
    pub reset_token: String,
    /// 凭证有效期（秒）。
    pub expires_in: u64,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: AdminIssuePasswordResetInput,
    account: Arc<Account>,
) -> Result<ApiResponse, BaseError> {
    let operator_id = ctx.actor()?.user_id();
    if input.id <= 0 {
        return Err(BaseError::ParamInvalid(
            "id".to_string(),
            "目标用户必须是正整数".to_string(),
        ));
    }
    // 目标用户必须存在且启用（停用账号不签发重置凭证）。
    let observed = account
        .users()
        .find_credentials_by_id(&ctx, input.id)
        .await?
        .ok_or_else(|| BaseError::UserNotFound(input.id.to_string()))?;
    Account::ensure_active(observed.status)?;

    // 凭证插入与成功审计同事务原子提交：崩溃时二者要么都落库、要么都不落库。
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let issued = account
            .issue_password_reset_by_in_tx(&mut transaction, input.id, Some(operator_id))
            .await?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", operator_id)?),
            audit::entity("password_reset", issued.fingerprint())?,
            None,
            Some(audit::summary([
                ("target_user_id", json!(input.id)),
                ("reset_fingerprint", json!(issued.fingerprint())),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(issued)
    }
    .await;
    let issued = Account::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        IssuedResetToken {
            reset_token: issued.raw_token().to_string(),
            expires_in: account.password_reset_ttl_seconds(),
        },
        "重置凭证已签发（明文只显示一次）",
    )
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("admin_issue_password_reset"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(HttpMethod::Post, "/api/v1/users/{id}/password-reset-tokens")
        .display_name("签发重置凭证")
        .description("管理端为目标用户签发一次性密码重置凭证（明文只回显一次）")
        // 独立权限：凭据签发能夺取任意账号（含系统管理员），危害面与同名的
        // `account.users.manage`（日常启用/停用）完全不同，必须分开声明。
        .permissions(["account.users.reset_credentials"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_requires_path_user_id() {
        let params = <AdminIssuePasswordResetInput as ParamInput>::params();
        let user_id = params
            .as_slice()
            .iter()
            .find(|param| param.name.as_str() == "id")
            .unwrap_or_else(|| panic!("应声明 id 参数"));
        assert_eq!(user_id.source, yang_base::definition::ParamSource::Path);
        assert!(user_id.required);
    }
}

//! 管理端停用目标账号（需权限 + Step-up，路线图 D-1）。

use crate::addon::account::domain::status::UserStatus;
use crate::addon::account::Account;
use crate::audit;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, Key, ModuleSpec};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) AdminDisableUserInput {
        #[param(source = path)]
        id: Key::new()
            .title("目标用户")
            .require(true),
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: AdminDisableUserInput,
    account: Arc<Account>,
) -> Result<ApiResponse, BaseError> {
    let operator_id = ctx.actor()?.user_id();
    if input.id <= 0 {
        return Err(BaseError::ParamInvalid(
            "id".to_string(),
            "目标用户必须是正整数".to_string(),
        ));
    }
    // 防锁定：操作者不能停用自身（否则最后一名持权管理员可把自己锁出系统，
    // 且无上帝账号兜底，只能靠运维手工 SQL 恢复）。
    if operator_id == input.id {
        return Err(BaseError::ParamInvalid(
            "id".to_string(),
            "不能停用当前操作者的账号".to_string(),
        ));
    }
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let locked = account
            .lock_credential_in_tx(&ctx, &mut transaction, input.id)
            .await?;
        if !locked.status().is_active() {
            return Err(BaseError::PermissionDenied("目标账号已停用".to_string()));
        }
        Account::disable_locked_in_tx(&mut transaction, &locked).await?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", operator_id)?),
            audit::entity("user", input.id)?,
            Some(audit::summary([(
                "status",
                json!(UserStatus::Active.as_str()),
            )])?),
            Some(audit::summary([(
                "status",
                json!(UserStatus::Disabled.as_str()),
            )])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;
    Account::finish_transaction(transaction, result).await?;

    let immediate_convergence = account
        .converge_revocation(&ctx, input.id, "account.user.admin_disable_user", "user")
        .await?;
    ApiResponse::success(
        json!({
            "user_id": input.id,
            "disabled": true,
            "immediate_convergence": immediate_convergence,
        }),
        if immediate_convergence {
            "账号已停用，全部会话已撤销"
        } else {
            "账号已停用，Redis 即时收敛待后台重试"
        },
    )
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("admin_disable_user"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(HttpMethod::Post, "/api/v1/users/{id}/disable")
        .display_name("停用用户")
        .description("管理端停用目标账号并撤销其全部会话")
        .permissions(["account.users.manage"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_requires_path_user_id() {
        let params = <AdminDisableUserInput as ParamInput>::params();
        let user_id = params
            .as_slice()
            .iter()
            .find(|param| param.name.as_str() == "id")
            .unwrap_or_else(|| panic!("应声明 id 参数"));
        assert_eq!(user_id.source, yang_base::definition::ParamSource::Path);
        assert!(user_id.required);
    }
}

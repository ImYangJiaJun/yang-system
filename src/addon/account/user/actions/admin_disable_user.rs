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
        // spec §8.1 附加规则：**只有全权组成员能修改全权组成员**。与 `add_group_member` /
        // `remove_group_member` 是同一条守卫（同一机制、同一句拒绝文案），经由账号域的
        // `SystemAuthorizationPort` 复用 access 侧的成员读——停用一名全权组成员改的也是
        // 该组成员的授权事实，与移出成员同理。
        //
        // **必须排在下面 §8.2 的最后管理员判定之前**：那条判定只在「停用后一名都不剩」
        // 时才拒绝，因此它对「组里还剩 >=2 名启用管理员」的停用一律放行。少了本守卫，
        // 持 `account.users.manage` 的非管理员就能把管理员逐个停用——每次都能通过最后
        // 管理员判定，反复执行直到组内只剩他指定的那一名，把守卫本身变成摆设。
        account
            .system_authorization()
            .ensure_operator_may_modify_system_admin_member(
                &ctx,
                &mut transaction,
                operator_id,
                input.id,
            )
            .await?;
        // spec §8.2：不能移除最后一名 active 系统管理员。
        if !account
            .system_authorization()
            .remains_an_admin_after(&ctx, &mut transaction, input.id)
            .await?
        {
            return Err(Account::last_system_admin_guard("停用"));
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

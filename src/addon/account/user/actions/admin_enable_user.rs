//! 管理端启用目标账号（需权限 + Step-up，路线图 D-1）。
//!
//! 与 [`super::admin_disable_user`] 完全对称：启用一名全权组成员同样会改动该组成员的
//! 授权事实（撤销一次合法的停用决定、让其权限与会话复活），因此同样受 spec §8.1 的
//! 「只有全权组成员能修改全权组成员」守卫约束。

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
    pub(super) AdminEnableUserInput {
        #[param(source = path)]
        id: Key::new()
            .title("目标用户")
            .require(true),
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: AdminEnableUserInput,
    account: Arc<Account>,
) -> Result<ApiResponse, BaseError> {
    let operator_id = ctx.actor()?.user_id();
    if input.id <= 0 {
        return Err(BaseError::ParamInvalid(
            "id".to_string(),
            "目标用户必须是正整数".to_string(),
        ));
    }
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let locked = account
            .lock_credential_in_tx(&ctx, &mut transaction, input.id)
            .await?;
        if !is_enable_target(locked.status()) {
            return Err(BaseError::PermissionDenied(
                "只有已停用的账号可以被启用".to_string(),
            ));
        }
        // spec §8.1 附加规则：**只有全权组成员能修改全权组成员**。与 `admin_disable_user`
        // 及 `add_group_member` / `remove_group_member` 是同一条守卫（同一机制、同一句拒绝
        // 文案），经由账号域的 `SystemAuthorizationPort` 复用 access 侧的成员读——启用一名
        // 被停用的全权组成员，恢复的也是该成员的授权事实，与停用/移出成员同理。
        //
        // 少了本守卫，持 `account.users.manage` 的非管理员就能把被合法停用的管理员**重新
        // 启用**：§8.2 的最后管理员判定、或另一名管理员的处置被单方面撤销，被停用者的权限
        // 与会话随之复活。停用与启用是同一枚硬币的两面，守卫必须完全对称。
        account
            .system_authorization()
            .ensure_operator_may_modify_system_admin_member(
                &ctx,
                &mut transaction,
                operator_id,
                input.id,
            )
            .await?;
        Account::activate_locked_in_tx(&mut transaction, &locked).await?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", operator_id)?),
            audit::entity("user", input.id)?,
            Some(audit::summary([(
                "status",
                json!(UserStatus::Disabled.as_str()),
            )])?),
            Some(audit::summary([(
                "status",
                json!(UserStatus::Active.as_str()),
            )])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;
    Account::finish_transaction(transaction, result).await?;
    ApiResponse::success(
        json!({ "user_id": input.id, "enabled": true }),
        "账号已启用",
    )
}

/// 启用动作只接受「已停用」目标。
///
/// 此前只拒绝 `is_active()`，于是 `status = deleted` 的匿名化账号也能通过守卫，被
/// [`Account::activate_locked_in_tx`] 翻转回 active：注销是不可逆事实（username 已改写为
/// `deleted_{id}` 墓碑名，邮箱、密码摘要、TOTP 材料均已清空/惰性化），复活会重建一个占用
/// 墓碑名的空壳账号，并使「注销」语义失效；审计事件也会谎称来态是 Disabled。
fn is_enable_target(status: UserStatus) -> bool {
    matches!(status, UserStatus::Disabled)
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("admin_enable_user"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(HttpMethod::Post, "/api/v1/users/{id}/enable")
        .display_name("启用用户")
        .description("管理端启用被停用的目标账号")
        .permissions(["account.users.manage"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_requires_path_user_id() {
        let params = <AdminEnableUserInput as ParamInput>::params();
        let user_id = params
            .as_slice()
            .iter()
            .find(|param| param.name.as_str() == "id")
            .unwrap_or_else(|| panic!("应声明 id 参数"));
        assert_eq!(user_id.source, yang_base::definition::ParamSource::Path);
        assert!(user_id.required);
    }

    /// 注销不可逆：`deleted` 账号不得被启用动作复活（回归：守卫曾只拒绝 `is_active`，
    /// 匿名化账号可被翻回 active）。
    #[test]
    fn only_disabled_accounts_are_valid_enable_targets() {
        assert!(is_enable_target(UserStatus::Disabled));
        assert!(
            !is_enable_target(UserStatus::Deleted),
            "已注销（匿名化）账号不得被启用"
        );
        assert!(
            !is_enable_target(UserStatus::Active),
            "已启用账号不得重复启用"
        );
    }
}

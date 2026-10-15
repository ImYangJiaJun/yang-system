//! `account.admin_users` Module（module 层）：后台用户管理模块装配。
//!
//! 框架要求模块自身持有可展示的 Action（`resolve_owned_action` 禁止跨模块引用），
//! 因此管理端的四个动作从 `account.user` 迁到这里，独立成一个可见模块：
//! 无表（`users` 表已由 `account.user` 声明，同表不可两处声明），动作与展示投影
//! 全部引用本模块自有的 Action。

mod actions;

use super::domain::context::Account;
use crate::authorization::AuthorizationVersionValidator;
use std::sync::Arc;
use yang_base::action::TokenAuthMiddleware;
use yang_base::definition::{ModuleName, ModulePresentationSpec, ModuleSpec};
use yang_base::BaseError;

/// 装配 `account.admin_users` Module：中间件 → Action 注册表 → 展示投影。
pub(super) fn build_module(
    account: Arc<Account>,
    authorization_validator: AuthorizationVersionValidator,
) -> Result<ModuleSpec, BaseError> {
    let module = ModuleSpec::new(
        ModuleName::new("account.admin_users")
            .map_err(|error| BaseError::ConfigError(error.to_string()))?,
    )
    .middleware(
        TokenAuthMiddleware::new(super::domain::claims::user_from_claims)
            .with_claims_validator(authorization_validator)
            .authenticate_public_actions(),
    );
    let module = actions::register_all(module, account);
    Ok(module.presentation(presentation()))
}

/// 前端展示投影（系统管理导航）。
fn presentation() -> ModulePresentationSpec {
    ModulePresentationSpec::new(crate::addon::admin_identity(), "用户", "admin_users")
        .app_route("/admin/users")
        .description("查看用户并为忘记密码的账号签发重置凭证")
        .order(30)
        .primary_action(yang_base::action!("account.admin_users.list_users"))
}

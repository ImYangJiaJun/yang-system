//! 账号 Addon（addon 层）：装配入口与对外端口。
//!
//! 两层结构：`user/` 是 module 层（`actions/` 里每个接口一个自包含文件，
//! 业务用例内联其中）；`domain/` 是 addon 层共享机制，Action 所需的全部
//! 能力经模块上下文 [`Account`] 单一出口提供。

mod admin_users;
pub(crate) mod domain;
mod user;

use crate::authorization::AuthorizationVersionValidator;
use crate::config::SecuritySettings;
use std::sync::Arc;
use yang_base::definition::AddonSpec;
use yang_base::BaseError;

// 组合根与基础设施使用的账号端口。
pub use domain::email_delivery;
pub(crate) use domain::grants::{AuthorizationGrants, CompositeGrantResolver, GrantResolver};
pub(crate) use domain::system_owner::{SystemAuthorizationPort, SystemOwnerClaimer};

// 外围授权域（access）使用的账号端口：授权快照类型与可信用户投影。
pub(crate) use domain::claims::user_from_claims;

// 基础设施使用的账号端口：飞书用户授权提醒要按表 DSL 读 `users`（取「第一个账号」
// 的邮箱），它没有 ActionContext、走不了 `Account` 的用例入口，但表声明是唯一事实
// 源，不该在别处复制一份。只导出**声明**，读写仍由调用方按自己的场景组装。
pub(crate) use user::table::user_table_spec;

// module 层（装配与 Action 文件）的统一入口：模块上下文与少量共享类型。
pub(crate) use domain::authz_version::LockedUserCredential;
pub(crate) use domain::context::Account;
pub(crate) use domain::password_reset::PasswordResetReference;
pub(crate) use domain::system_owner::OwnerClaimOutcome;

/// 装配授权失效公共端口（组合根调用一次）。
///
/// 读/写两个端口由同一账号域实现承载：`users.authz_version` 的 SQL 仍只在
/// `domain/authz_version.rs` 一个 writer 文件、一条执行路径。
pub(crate) fn authorization_port() -> crate::authorization::AuthorizationPort {
    let port = Arc::new(domain::authz_version::AccountAuthorizationPort);
    crate::authorization::AuthorizationPort::new(port.clone(), port)
}

/// 构建账号 Addon。
///
/// Addon 边界负责声明产品能力及其 Module；应用层不应直接拼装 `account.user`。
/// `account.user` 与 `account.admin_users` 共享同一份模块上下文 [`Account`]。
pub(crate) fn build_addon(
    security: Arc<SecuritySettings>,
    grant_resolver: Arc<dyn GrantResolver>,
    system_owner_claimer: Arc<dyn SystemOwnerClaimer>,
    system_authorization: Arc<dyn SystemAuthorizationPort>,
    authorization_validator: AuthorizationVersionValidator,
) -> Result<AddonSpec, BaseError> {
    let account = user::build_context(
        security,
        grant_resolver,
        system_owner_claimer,
        system_authorization,
    )?;
    Ok(AddonSpec::new(yang_base::addon!("account"))
        .module(user::build_module(
            Arc::clone(&account),
            authorization_validator.clone(),
        )?)
        .module(admin_users::build_module(account, authorization_validator)?))
}

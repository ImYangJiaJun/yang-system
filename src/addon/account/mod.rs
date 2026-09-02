//! 账号 Addon（addon 层）：装配入口与对外端口。
//!
//! 两层结构：`user/` 是 module 层（`actions/` 里每个接口一个自包含文件，
//! 业务用例内联其中）；`domain/` 是 addon 层共享机制，Action 所需的全部
//! 能力经模块上下文 [`Account`] 单一出口提供。

pub(crate) mod domain;
mod user;

use crate::authorization::AuthorizationVersionValidator;
use crate::authorization::StepUpServices;
use crate::config::SecuritySettings;
use std::sync::Arc;
use yang_base::definition::AddonSpec;
use yang_base::BaseError;

// 组合根与基础设施使用的账号端口。
pub(crate) use domain::authz_version::find_authorization_version;
pub use domain::email_delivery;
pub(crate) use domain::grants::{AuthorizationGrants, CompositeGrantResolver, GrantResolver};
pub(crate) use domain::system_owner::SystemOwnerClaimer;

// 外围授权域（access）使用的账号端口：授权版本锁/递增原语、授权快照类型与可信用户投影。
pub(crate) use domain::authz_version::{
    increment_locked_authorization_version, lock_authorization_version,
};
pub(crate) use domain::claims::user_from_claims;

// module 层（装配与 Action 文件）的统一入口：模块上下文与少量共享类型。
pub(crate) use domain::authz_version::LockedUserCredential;
pub(crate) use domain::context::Account;
pub(crate) use domain::password_reset::PasswordResetReference;
pub(crate) use domain::system_owner::OwnerClaimOutcome;

/// 返回不声明最终管理员的默认声明器。
///
/// 当前骨架只保留 account Addon，没有平台管理域来声明最终管理员；
/// 注册流程照常完成，任何账号都不会成为系统最终管理员。
pub(crate) fn no_system_owner_claimer() -> Arc<dyn SystemOwnerClaimer> {
    Arc::new(domain::system_owner::NoSystemOwnerClaimer)
}

/// 构建账号 Addon。
///
/// Addon 边界负责声明产品能力及其 Module；应用层不应直接拼装 `account.user`。
pub(crate) fn build_addon(
    security: Arc<SecuritySettings>,
    grant_resolver: Arc<dyn GrantResolver>,
    system_owner_claimer: Arc<dyn SystemOwnerClaimer>,
    authorization_validator: AuthorizationVersionValidator,
    step_up: Option<StepUpServices>,
) -> Result<AddonSpec, BaseError> {
    Ok(
        AddonSpec::new(yang_base::addon!("account")).module(user::build_module(
            security,
            grant_resolver,
            system_owner_claimer,
            authorization_validator,
            step_up,
        )?),
    )
}

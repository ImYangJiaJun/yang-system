//! 业务 Addon 集合。
//!
//! 这里仅声明顶层业务边界。每个子目录必须只暴露一个 `build_addon()` 入口，
//! 具体 Module、Action 和领域服务由 Addon 自己封装，避免应用组合根了解内部结构。

pub mod account;
#[cfg(feature = "admin")]
pub mod admin;
#[cfg(feature = "observability")]
pub mod observability;
#[cfg(feature = "org")]
pub mod org;
#[cfg(feature = "work")]
pub mod work;

use yang_base::definition::AccountIdentitySpec;

pub(crate) fn user_identity() -> AccountIdentitySpec {
    AccountIdentitySpec::new("user", "个人账户", "person").order(10)
}

#[cfg(feature = "org")]
pub(crate) fn organization_identity() -> AccountIdentitySpec {
    AccountIdentitySpec::new("org", "企业账号", "organization").order(20)
}

#[cfg(feature = "admin")]
pub(crate) fn administrator_identity() -> AccountIdentitySpec {
    AccountIdentitySpec::new("admin", "管理平台", "administrator").order(30)
}

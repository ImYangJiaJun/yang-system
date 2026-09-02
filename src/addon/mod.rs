//! 业务 Addon 集合。
//!
//! 这里仅声明顶层业务边界。每个子目录必须只暴露一个 `build_addon()` 入口，
//! 具体 Module、Action 和领域服务由 Addon 自己封装，避免应用组合根了解内部结构。

pub(crate) mod access;
pub mod account;
pub(crate) mod demo;

use yang_base::definition::AccountIdentitySpec;

pub(crate) fn user_identity() -> AccountIdentitySpec {
    AccountIdentitySpec::new("user", "个人账户", "person").order(10)
}

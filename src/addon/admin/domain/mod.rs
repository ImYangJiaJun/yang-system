//! 平台 Addon 级领域机制。

mod grants;
mod guard;

pub(super) use grants::AdminGrantResolver;
pub(crate) use guard::validate_system_owner_state;

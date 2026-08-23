//! 个人任务规划 Addon 级领域机制。

mod grants;
mod tenant;

pub(crate) use grants::grant_resolver;
pub(super) use tenant::PersonalWorkspaceResolver;

//! 企业成员领域机制。

mod fn_action;
mod guard;
mod view;

pub(super) mod repository;

pub(super) use fn_action::FnAction;
pub(super) use guard::OrgAdminGuardMiddleware;
pub(super) use view::build as build_list_view;

//! 任务领域机制。

mod fn_action;
mod view;

pub(super) mod repository;

pub(super) use fn_action::FnAction;
pub(super) use view::build_all as build_views;

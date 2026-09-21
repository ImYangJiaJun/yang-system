//! `feishu.datasource` Module：数据源注册表。
//!
//! 本文件是模块的"定义卡"：表、Action 注册表、展示投影与通用 TableView 按分区顺序装配。

pub(crate) mod actions;
pub(crate) mod table;

use std::sync::Arc;

use crate::addon::account::user_from_claims;
use crate::authorization::AuthorizationVersionValidator;
use yang_base::action::TokenAuthMiddleware;
use yang_base::definition::{
    ActionConfirmation, ActionInteraction, ActionName, ActionPlacement, ActionPresentationSpec,
    ActionRef, FieldName, FieldRef, ModuleName, ModulePresentationSpec, ModuleSpec, SortDirection,
    TableName, TableSortSpec, ViewName, ViewSpec,
};
use yang_base::BaseError;

use super::domain::context::FeishuContext;

/// 本 module 的名字。
const MODULE: &str = "feishu.datasource";
/// 本 module 的表名。
const TABLE: &str = "feishu_datasource";

/// 装配 `feishu.datasource` Module。
pub(crate) fn build_module(
    context: Arc<FeishuContext>,
    authorization_validator: AuthorizationVersionValidator,
) -> Result<ModuleSpec, BaseError> {
    let spec = ModuleSpec::new(module_name()?)
        .table(table::table_spec()?)
        .presentation(presentation())
        .view(view()?);
    let spec = with_authentication(spec, authorization_validator);
    Ok(actions::register_all(spec, context))
}

/// 挂上认证中间件。
///
/// **这一步不可省。** `TokenAuthMiddleware` 才是把 JWT 解成 `ctx.user`（含角色与权限）
/// 的地方；没有它，受保护 Action 在 `authorize()` 阶段拿不到身份，一律返回 401。
/// 症状具有迷惑性：接口返回 401 而不是 403，看着像「没登录」，实际是「没人建立身份」。
///
/// # 不要加 `authenticate_public_actions()`
///
/// 它会把这个中间件的 scope 从 `ProtectedActions` 抬到 `AllActions`，从而**也覆盖 public
/// 的机器入口**；而它抢的是与管理 Token 中间件**同一个** `Authorization` 头，且排在前面
/// ——静态管理 Token 会被当成 Access JWT 去验签并直接短路，写入入口因此永远拿不到请求。
///
/// 它想解决的问题并不存在：`Next::run` 对 `ProtectedActions` 的判据是
/// `!policy.is_public`，public Action 本来就被跳过，匿名请求自然放行。
pub(crate) fn with_authentication(
    module: ModuleSpec,
    authorization_validator: AuthorizationVersionValidator,
) -> ModuleSpec {
    module.middleware(
        TokenAuthMiddleware::new(user_from_claims).with_claims_validator(authorization_validator),
    )
}

/// 前端展示投影（控制台导航）。
fn presentation() -> ModulePresentationSpec {
    ModulePresentationSpec::new(crate::addon::user_identity(), "飞书数据源", "table")
        .description("飞书审批外部选项的数据源注册表")
        .order(40)
        .primary_action(yang_base::action!("feishu.datasource.list_datasources"))
}

/// 通用 TableView：数据 Action + 列 + 操作全部声明式投影，前端零代码。
fn view() -> Result<ViewSpec, BaseError> {
    let field = |name: &str| -> Result<FieldRef, BaseError> {
        Ok(FieldRef::new(
            TableName::new(TABLE).map_err(config_error)?,
            FieldName::new(name).map_err(config_error)?,
        ))
    };
    let action = |name: &str| -> Result<ActionRef, BaseError> {
        Ok(ActionRef::new(
            module_name()?,
            ActionName::new(name).map_err(config_error)?,
        ))
    };

    Ok(ViewSpec::new(ViewName::new("main").map_err(config_error)?)
        .title("数据源")
        .data_action(action("list_datasources")?)
        .field(field("source_key")?)
        .field(field("title")?)
        .field(field("status")?)
        .field(field("encrypt_enabled")?)
        .field(field("default_locale")?)
        .action(action("list_datasources")?)
        .present_action(
            action("create_datasource")?,
            ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Form),
        )
        .present_action(
            action("update_datasource")?,
            ActionPresentationSpec::new(ActionPlacement::Row, ActionInteraction::Form)
                .record_parameter("source_key"),
        )
        .present_action(
            action("delete_datasource")?,
            ActionPresentationSpec::new(ActionPlacement::Row, ActionInteraction::Invoke)
                .record_parameter("source_key")
                .confirmation(ActionConfirmation::new(
                    "删除数据源",
                    "删除后其下全部选项会被同时停用，且不可恢复。确认删除？",
                )),
        )
        .default_sort(TableSortSpec::new(field("source_key")?, SortDirection::Asc)))
}

fn module_name() -> Result<ModuleName, BaseError> {
    ModuleName::new(MODULE).map_err(config_error)
}

fn config_error(error: impl std::fmt::Display) -> BaseError {
    BaseError::ConfigError(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_name_and_table_are_declared() {
        let name = module_name().unwrap_or_else(|error| panic!("模块名应有效: {error}"));
        assert_eq!(name.as_str(), MODULE);
        assert_eq!(TABLE, "feishu_datasource");
    }

    #[test]
    fn view_projects_every_declared_column_and_action() {
        let spec = view().unwrap_or_else(|error| panic!("View 应可构造: {error}"));
        assert_eq!(
            spec.fields.len(),
            5,
            "应投影 source_key/title/status/encrypt_enabled/default_locale"
        );
        for presented in [
            "create_datasource",
            "update_datasource",
            "delete_datasource",
        ] {
            assert!(
                spec.actions
                    .iter()
                    .any(|action| action.action().as_str() == presented),
                "View 应展示 {presented}"
            );
        }
        let data_action = spec
            .data_action
            .as_ref()
            .unwrap_or_else(|| panic!("TableView 必须声明数据 Action"));
        assert_eq!(data_action.action().as_str(), "list_datasources");
    }
}

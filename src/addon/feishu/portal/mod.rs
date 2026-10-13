//! `feishu.portal` Module：数据源查询门户。
//!
//! 本 module 声明两张表（门户 + 访问日志）+ Action 注册表。
//! 门户的管理视图是数据源详情页的一部分（新增 Tab），不是引擎投影的通用 TableView。
//!
//! # 为什么刻意不声明 `view()`
//!
//! 与 `feishu.datasource` 同一取舍：门户的管理视图是数据源详情页的一部分，
//! 不是引擎投影的通用 TableView。省 `view()` 避免框架自动合成一个含全表列的视图。

pub(crate) mod actions;
pub(crate) mod domain;
pub(crate) mod table;

use std::sync::Arc;

use yang_base::action::TokenAuthMiddleware;
use yang_base::definition::{ModuleName, ModulePresentationSpec, ModuleSpec};
use yang_base::BaseError;

use crate::addon::account::user_from_claims;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::authorization::AuthorizationVersionValidator;

/// 本 module 的名字。
const MODULE: &str = "feishu.portal";

/// 装配 `feishu.portal` Module。
///
/// 挂 Action 注册表 + presentation + 认证中间件。
pub(crate) fn build_module(
    context: Arc<FeishuContext>,
    authorization_validator: AuthorizationVersionValidator,
) -> Result<ModuleSpec, BaseError> {
    // 框架硬形状：`ModuleSpec::table()` 是单个 `Option<TableSpec>`，只能挂一张表。
    // 所以门户表与日志表必须是两个 module（与 `datasource` + `datasource_field` 同一模式）。
    // 这里只挂主表，日志表由 `build_log_module()` 单独挂。
    let spec = ModuleSpec::new(module_name()?).table(table::table_spec()?);

    // # 为什么这里**要**加 `authenticate_public_actions()`（与 `datasource` 相反）
    //
    // `portal_query` 是 public，而 `access_mode = login` 靠
    // `ctx.authenticated_user()` 判权。默认的 `ProtectedActions` scope 会把 public
    // Action **整个跳过**，于是那个判据恒为 `None`——login 模式永远 403，是一条
    // 谁也走不通的配置。抬到 `AllActions` 后：带 Bearer 的请求会注入身份，
    // **完全没有** `Authorization` 头的请求仍然匿名放行（public / passphrase 不受影响）。
    //
    // `datasource::with_authentication` 文档里那条「不要加」的禁令针对的是**同一个
    // module 里还有管理 Token 入口**的情形——两个中间件抢同一个 `Authorization` 头。
    // 本 module 没有管理 Token 入口，不存在这个冲突。
    //
    // 代价（已知并接受）：带着**过期或无效** Bearer 头的请求会得到 401，而不是退回
    // 匿名。公开门户页因此不应附带 `Authorization` 头——它本来就是给匿名访客用的。
    let spec = spec.middleware(
        TokenAuthMiddleware::new(user_from_claims)
            .with_claims_validator(authorization_validator)
            .authenticate_public_actions(),
    );

    Ok(actions::register_all(spec, context).presentation(
        ModulePresentationSpec::new(crate::addon::admin_identity(), "飞书查询门户", "search")
            .app_route("/feishu/portals")
            .order(35)
            .primary_action(yang_base::action!("feishu.portal.list_portals")),
    ))
}

/// 本 module 的第二个 Module：只有日志表，没有 Action。
///
/// # 为什么要单独一个 Module
///
/// 同 `datasource::build_field_module()` 的说明：框架硬形状决定一张表 = 一个 module。
/// 日志表无独立控制台入口，也没有可投影的数据 Action。
pub(crate) fn build_log_module() -> Result<ModuleSpec, BaseError> {
    Ok(ModuleSpec::new(log_module_name()?).table(domain::log_table::table_spec()?))
}

/// 日志表所在 Module 的名字。
fn log_module_name() -> Result<ModuleName, BaseError> {
    ModuleName::new("feishu.portal_log").map_err(config_error)
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
    fn module_name_is_the_stable_qualified_identifier() {
        let name = module_name().unwrap_or_else(|error| panic!("模块名应有效: {error}"));
        assert_eq!(name.as_str(), MODULE);
    }

    #[test]
    fn log_module_name_is_the_stable_qualified_identifier() {
        let name = log_module_name().unwrap_or_else(|error| panic!("模块名应有效: {error}"));
        assert_eq!(name.as_str(), "feishu.portal_log");
    }

    #[test]
    fn table_spec_is_declared() {
        // 表必须进 Catalog，否则 schema 同步与数据面都会缺表。
        let spec = table::table_spec().unwrap_or_else(|error| panic!("表声明应有效: {error}"));
        let definition = spec
            .table_definition()
            .unwrap_or_else(|error| panic!("应可编译为表定义: {error}"));
        assert_eq!(definition.name(), "feishu_datasource_portal");
    }

    #[test]
    fn log_table_spec_is_declared() {
        let spec =
            domain::log_table::table_spec().unwrap_or_else(|error| panic!("表声明应有效: {error}"));
        let definition = spec
            .table_definition()
            .unwrap_or_else(|error| panic!("应可编译为表定义: {error}"));
        assert_eq!(definition.name(), "feishu_datasource_portal_log");
    }

    #[test]
    fn the_log_module_projects_nothing_to_the_frontend() {
        let spec = build_log_module().unwrap_or_else(|error| panic!("模块应可装配: {error}"));
        assert!(spec.views.is_empty(), "不得声明视图");
        assert!(spec.presentation.is_none(), "不得声明呈现");
    }
}

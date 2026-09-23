//! `feishu.datasource` Module：数据源注册表。
//!
//! 本文件是模块的"定义卡"：表与 Action 注册表。
//!
//! # 为什么刻意不声明 `presentation()` 与 `view()`
//!
//! 控制台的入口是**自建页面**（`frontend/src/features/feishu/`），不是引擎投影的
//! 通用 TableView。所以本模块不向 Catalog 投影任何界面。这两个必须**成对省略**：
//!
//! - 只省 `view()`：框架会给 `views` 为空的模块**自动合成**一个含全表列的
//!   `feishu.datasource.default` 视图（`builder/compile.rs` 的 `module.views.is_empty()`
//!   分支），只因它 `data_action` 为 `None` 才暂时没被投影出来——将来给模块加一个
//!   可用作数据源的 primary action，整张表就会静默复现。
//! - 只省 `presentation()`：模块离开 `catalog.modules`，但 view 仍在
//!   `catalog.table_views`，前端 `navigation.ts` 的 `unassignedViews` 会把表格
//!   挂到「工作台」分组下重新出现。
//!
//! 代价：原先在 `view()` 上声明的删除二次确认文案不再随 Catalog 下发，改由前端持有
//! 同一份文案。见 `docs/architecture/feishu-datasource-console.md` §4.2 与 §5.3-1。

pub(crate) mod actions;
pub(crate) mod domain;
pub(crate) mod table;

use std::sync::Arc;

use crate::addon::account::user_from_claims;
use crate::authorization::AuthorizationVersionValidator;
use yang_base::action::TokenAuthMiddleware;
use yang_base::definition::{ModuleName, ModuleSpec};
use yang_base::BaseError;

use super::domain::context::FeishuContext;

/// 本 module 的名字。
const MODULE: &str = "feishu.datasource";

/// 装配 `feishu.datasource` Module。
pub(crate) fn build_module(
    context: Arc<FeishuContext>,
    authorization_validator: AuthorizationVersionValidator,
) -> Result<ModuleSpec, BaseError> {
    let spec = ModuleSpec::new(module_name()?).table(table::table_spec()?);
    let spec = with_authentication(spec, authorization_validator);
    Ok(actions::register_all(spec, context))
}

/// 本 module 的第二个 Module：只有字段绑定表，没有 Action。
///
/// # 为什么要单独一个 Module
///
/// `ModuleSpec::table()` 是**单个** `Option<TableSpec>`（「设置 Module 主表」），
/// `AddonSpec` 又没有挂独立表的入口（只有 `module()`）。所以「一张表 = 一个 module」
/// 是框架的硬形状——绑定表要进 schema，就只能是自己的 module。
///
/// # 它为什么住在 `datasource/` 里而不是自己的目录
///
/// 架构门禁只把**含 `actions/` 的目录**认定为 module。绑定表没有自己的 Action
/// （它的读写都由本 module 的 Action 经 `FeishuContext` 跨表完成），所以独立目录
/// 会被判成「游离的机制目录」。故构造器与主表并列放在这里。
///
/// # 成对省略 `view()` 与 `presentation()`
///
/// 与主表同一条纪律，理由见本文件开头的模块文档：只省一个会让表静默出现在前端。
pub(crate) fn build_field_module() -> Result<ModuleSpec, BaseError> {
    Ok(ModuleSpec::new(field_module_name()?).table(domain::field_table::table_spec()?))
}

/// 字段绑定表所在 Module 的名字。
fn field_module_name() -> Result<ModuleName, BaseError> {
    ModuleName::new("feishu.datasource_field").map_err(config_error)
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
    fn table_spec_is_still_declared_after_dropping_the_view_projection() {
        // 去掉 presentation/view 只影响**界面投影**，表本身必须仍然进 Catalog，
        // 否则 schema 同步与数据面都会缺表。
        let spec = table::table_spec().unwrap_or_else(|error| panic!("表声明应有效: {error}"));
        let definition = spec
            .table_definition()
            .unwrap_or_else(|error| panic!("应可编译为表定义: {error}"));
        assert_eq!(definition.name(), "feishu_datasource");
    }

    #[test]
    fn the_field_module_declares_the_binding_table() {
        // 绑定表要进 schema 就必须有一个声明它的 Module——`ModuleSpec::table()`
        // 只收一张表，所以它只能是独立的一个。漏了这一步，表永远不会被创建。
        let spec = build_field_module().unwrap_or_else(|error| panic!("模块应可装配: {error}"));
        let table = spec.table.as_ref().expect("必须声明绑定表");
        let definition = table
            .table_definition()
            .unwrap_or_else(|error| panic!("应可编译为表定义: {error}"));
        assert_eq!(definition.name(), "feishu_datasource_field");
    }

    #[test]
    fn the_field_module_projects_nothing_to_the_frontend() {
        // 成对省略：只省 `view()` 会被框架自动合成一个含全表列的视图，
        // 只省 `presentation()` 会让 view 从「工作台」分组冒回来。
        let spec = build_field_module().unwrap_or_else(|error| panic!("模块应可装配: {error}"));
        assert!(spec.views.is_empty(), "不得声明视图");
        assert!(spec.presentation.is_none(), "不得声明呈现");
    }
}

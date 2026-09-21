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
}

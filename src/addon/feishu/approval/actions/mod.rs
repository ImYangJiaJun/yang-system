//! `feishu.approval` module 的 Action 注册表。
//!
//! 架构门禁要求 `actions/` 下每个文件恰好一个 `pub(super) async fn handle` +
//! 一个 `pub(super) fn register`，并在这里登记。
//!
//! # 两组端点，两条注册纪律
//!
//! - **控制台（7 个新 Action）无条件注册**：控制台走 JWT 鉴权，不依赖出站凭证存在
//!   （`create_config` / `list_widgets` 运行时按其自身逻辑校验凭证并返回 50301），
//!   所以注册在 `can_pull()` 门禁**之前**。
//! - **派发端点只在集成可用时注册**：与 `option` module 的机器入口同一条纪律——
//!   派发端点打飞书开放平台，需要 `app_id`/`app_secret` 齐备（`FeishuSettings::can_pull()`
//!   判定）。凭证缺失时注册出来只会返回「凭证未配置」，让人误以为端点可用；不注册则
//!   路由直接 404，语义更准。dispatch 及其 `ManagementTokenMiddleware` 保持门禁之后。
//!
//! # 鉴权：管理 Token 中间件 + 坐标白名单
//!
//! dispatch Action 保持 `public`（飞书工作流没有 JWT），由 `ManagementTokenMiddleware`
//! 校验静态 Bearer。中间件**必须注册**——Action 是 public 的，漏挂就裸奔。
//!
//! 光有 Token 还不够：管理 Token 是全局单值、不按数据源绑定（既有决策 A8），
//! 所以 handler 里还强制校验 `base_token`/`table_id` 落在已配置的启用行内
//! （见 `dispatch.rs`）。两道合起来才把可利用面收到「已配置的表格」。

pub(super) mod create_config;
pub(super) mod delete_config;
pub(super) mod dispatch;
pub(super) mod list_configs;
pub(super) mod list_requests;
pub(super) mod list_tasks;
pub(super) mod list_widgets;
pub(super) mod update_config;

use std::sync::Arc;

use yang_base::definition::{ActionName, ActionRef, ModuleName, ModuleSpec};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::middleware::ManagementTokenMiddleware;
use crate::config::FeishuSettings;

/// 注册本 module 的全部 Action。
pub(super) fn register_all(
    module: ModuleSpec,
    context: Arc<FeishuContext>,
    settings: Option<&FeishuSettings>,
) -> Result<ModuleSpec, BaseError> {
    // 控制台 Action 无条件注册（设计 §5：与 dispatch 相反——控制台不依赖飞书凭证
    // 存在；`create_config` 与 `list_widgets` 运行时按自身逻辑校验凭证并返回 50301）。
    let module = list_configs::register(module, Arc::clone(&context));
    let module = create_config::register(module, Arc::clone(&context));
    let module = update_config::register(module, Arc::clone(&context));
    let module = delete_config::register(module, Arc::clone(&context));
    let module = list_widgets::register(module, Arc::clone(&context));
    let module = list_requests::register(module, Arc::clone(&context));
    let module = list_tasks::register(module, Arc::clone(&context));

    let Some(settings) = settings.filter(|value| value.can_pull()) else {
        return Ok(module);
    };

    let mut module = dispatch::register(module, context);
    module = module.middleware(ManagementTokenMiddleware::new(
        &settings.management_api_token,
        action_ref("dispatch_approval")?,
    ));
    Ok(module)
}

/// 构造本 module 内某个 Action 的引用，供中间件的 `target_action()` 限定。
///
/// 精确限定而不是 `AllActions`：同 module 的只读 Action 不该被这把静态 Token 保护
/// （那会让控制台的 JWT 调用被拒）。
fn action_ref(name: &str) -> Result<ActionRef, BaseError> {
    let module = ModuleName::new("feishu.approval")
        .map_err(|error| BaseError::ConfigError(error.to_string()))?;
    let action =
        ActionName::new(name).map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(ActionRef::new(module, action))
}

#[cfg(test)]
mod tests {
    use super::*;

    use yang_base::definition::{HttpMethod, TableSpec};

    use crate::addon::feishu::domain::repository::Repository;

    /// 7 个控制台 Action 的路由：两态（有/无凭证）都必须注册。
    const CONSOLE_ROUTES: &[(&str, &str)] = &[
        ("list_configs", "/api/v1/feishu/approval/configs/query"),
        ("create_config", "/api/v1/feishu/approval/configs/create"),
        ("update_config", "/api/v1/feishu/approval/configs/update"),
        ("delete_config", "/api/v1/feishu/approval/configs/delete"),
        (
            "list_widgets",
            "/api/v1/feishu/approval/definitions/widgets",
        ),
        ("list_requests", "/api/v1/feishu/approval/requests/query"),
        ("list_tasks", "/api/v1/feishu/approval/tasks/query"),
    ];

    /// 装配好的 module（带一个可用的 `FeishuSettings`）。
    ///
    /// 用惰性连接池：这些用例只读路由表，不碰数据库。
    fn registered_module(with_credentials: bool) -> ModuleSpec {
        let pool = Arc::new(
            sqlx::MySqlPool::connect_lazy("mysql://user:pass@localhost:3306/yang")
                .unwrap_or_else(|error| panic!("惰性连接池应可构造: {error}")),
        );
        let definition = |spec: TableSpec| {
            spec.table_definition()
                .unwrap_or_else(|error| panic!("应可编译为表定义: {error}"))
        };
        let repository = |spec: Result<TableSpec, _>| {
            Repository::new(
                definition(spec.unwrap_or_else(|error| panic!("{error}"))),
                Arc::clone(&pool),
            )
        };
        let context = Arc::new(FeishuContext::new(
            repository(crate::addon::feishu::datasource::table::table_spec()),
            repository(crate::addon::feishu::datasource::domain::field_table::table_spec()),
            repository(crate::addon::feishu::option::table::table_spec()),
            repository(crate::addon::feishu::approval::table::table_spec()),
            repository(crate::addon::feishu::approval::domain::field_map_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::task_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::request_log_table::table_spec()),
            None,
        ));

        let settings = if with_credentials {
            Some(FeishuSettings {
                enabled: true,
                management_api_token: "a-real-management-token-value-1234".to_string(),
                encryption_key: None,
                app_id: Some("cli_real_app_id".to_string()),
                app_secret: Some("real-app-secret-value".to_string()),
                pull_interval_seconds: 900,
                alert_recipients: Vec::new(),
                alert_failure_threshold: 3,
                log_inbound_requests: false,
                approval_create_rate_per_minute: 90,
                approval_scan_interval_seconds: 30,
                approval_base_timezone: "Asia/Shanghai".to_string(),
            })
        } else {
            None
        };

        let spec = ModuleSpec::new(
            ModuleName::new("feishu.approval").unwrap_or_else(|error| panic!("{error}")),
        )
        .table(
            crate::addon::feishu::approval::table::table_spec()
                .unwrap_or_else(|error| panic!("{error}")),
        );
        register_all(spec, context, settings.as_ref())
            .unwrap_or_else(|error| panic!("应可注册: {error}"))
    }

    fn routes(module: &ModuleSpec) -> Vec<(HttpMethod, String)> {
        module
            .actions()
            .iter()
            .map(|spec| (spec.route.method, spec.route.path.clone()))
            .collect()
    }

    #[test]
    fn action_ref_points_at_this_module() {
        let reference =
            action_ref("dispatch_approval").unwrap_or_else(|error| panic!("应有效: {error}"));
        assert_eq!(reference.module().as_str(), "feishu.approval");
        assert_eq!(reference.action().as_str(), "dispatch_approval");
    }

    /// 7 个控制台路由在**有凭证**时全部注册。
    #[tokio::test]
    async fn console_routes_are_registered_with_credentials() {
        let module = registered_module(true);
        for (_, path) in CONSOLE_ROUTES {
            assert!(
                routes(&module).contains(&(HttpMethod::Post, path.to_string())),
                "带凭证时控制台路由 {path} 必须注册：{:?}",
                routes(&module)
            );
        }
    }

    /// 7 个控制台路由在**无凭证**时也全部注册（控制台不依赖出站凭证存在）。
    #[tokio::test]
    async fn console_routes_are_registered_without_credentials() {
        let module = registered_module(false);
        for (_, path) in CONSOLE_ROUTES {
            assert!(
                routes(&module).contains(&(HttpMethod::Post, path.to_string())),
                "无凭证时控制台路由 {path} 必须注册：{:?}",
                routes(&module)
            );
        }
    }

    #[tokio::test]
    async fn dispatch_route_is_registered_with_credentials() {
        let module = registered_module(true);
        assert!(
            routes(&module).contains(&(
                HttpMethod::Post,
                "/api/v1/feishu/approval/dispatch".to_string()
            )),
            "带凭证时派发路由必须注册：{:?}",
            routes(&module)
        );
    }

    #[tokio::test]
    async fn dispatch_route_is_absent_without_credentials() {
        // 与 option module 的机器入口同一条纪律：凭证缺失时端点打不通飞书，
        // 注册出来只会返回「凭证未配置」，让人误以为可用。不注册则 404，语义更准。
        let module = registered_module(false);
        assert!(
            !routes(&module).contains(&(
                HttpMethod::Post,
                "/api/v1/feishu/approval/dispatch".to_string()
            )),
            "无凭证时不得注册派发路由"
        );
    }

    #[tokio::test]
    async fn dispatch_action_is_public_for_the_machine_caller() {
        // 飞书多维表格工作流没有 JWT，所以 Action 必须是 public，靠
        // `ManagementTokenMiddleware` 校验静态 Token。
        //
        // **这条断言的意义在于：public 与「挂了中间件」必须成对**——Action 是
        // public 的，中间件一旦漏挂就完全裸奔（`domain/middleware.rs` 顶部注释）。
        // 中间件的挂载无法从外部内省（`ModuleSpec::middlewares()` 是 crate 私有），
        // 所以这里钉住 public 这一半；另一半由 `register_all` 的实现保证：
        // 注册 Action 的同一段代码紧接着挂中间件，没有第二条路径。
        let module = registered_module(true);
        let spec = module
            .actions()
            .iter()
            .find(|spec| spec.route.path == "/api/v1/feishu/approval/dispatch")
            .unwrap_or_else(|| panic!("派发路由必须存在"));
        assert!(spec.is_public, "派发 Action 必须 public（工作流无 JWT）");
    }
}

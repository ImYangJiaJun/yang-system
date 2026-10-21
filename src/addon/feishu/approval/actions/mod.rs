//! `feishu.approval` module 的 Action 注册表。
//!
//! 架构门禁要求 `actions/` 下每个文件恰好一个 `pub(super) async fn handle` +
//! 一个 `pub(super) fn register`，并在这里登记。
//!
//! # 两组端点，两条注册纪律
//!
//! - **控制台（14 个 Action）无条件注册**：控制台走 JWT 鉴权，不依赖出站凭证存在
//!   （`create_config` / `list_widgets` 运行时按其自身逻辑校验凭证并返回 50301），
//!   所以注册在 `can_pull()` 门禁**之前**。
//! - **派发端点只在集成可用时注册**：与 `option` module 的机器入口同一条纪律——
//!   派发端点打飞书开放平台，需要 `app_id`/`app_secret` 齐备（`FeishuSettings::can_pull()`
//!   判定）。凭证缺失时注册出来只会返回「凭证未配置」，让人误以为端点可用；不注册则
//!   路由直接 404，语义更准。dispatch 与用户身份授权回调（同样要打飞书）保持门禁之后。
//!
//! # 鉴权：按配置签发的访问 Token（2026-10-13 起）
//!
//! dispatch Action 保持 `public`（飞书工作流没有 JWT），鉴权在 handler 内完成：
//! 路由带 `{config_key}` 路径参数，按它查配置行、用请求体 `token` 的摘要比对
//! `token_hash`（`verify_token`，与外部数据源同一套）。Token **按配置隔离**，
//! 取代了旧的「全局单值 `management_api_token` + 请求体 `(base_token, table_id)`
//! 白名单」两道分散防线（设计 §9.1/§9.3 记录的决策，本端点执行之）。
//! Token 由 `create_config`（建配置时顺带签发）与 `rotate_config_token`（按需轮换）
//! 产出，明文只在响应里出现一次。

pub(super) mod create_config;
pub(super) mod delete_config;
pub(super) mod dispatch;
pub(super) mod list_configs;
pub(super) mod list_events;
pub(super) mod list_requests;
pub(super) mod list_tasks;
pub(super) mod list_widgets;
pub(super) mod oauth_authorize_url;
pub(super) mod oauth_callback;
pub(super) mod oauth_status;
pub(super) mod refresh_names;
pub(super) mod retry_event;
pub(super) mod reveal_config_token;
pub(super) mod rotate_config_token;
pub(super) mod update_config;

use std::sync::Arc;

use yang_base::definition::ModuleSpec;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
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
    let module = refresh_names::register(module, Arc::clone(&context));
    let module = create_config::register(module, Arc::clone(&context));
    let module = update_config::register(module, Arc::clone(&context));
    let module = delete_config::register(module, Arc::clone(&context));
    let module = list_widgets::register(module, Arc::clone(&context));
    let module = list_requests::register(module, Arc::clone(&context));
    let module = list_tasks::register(module, Arc::clone(&context));
    // 事件排查与人工重试只读写本库，不打飞书，因此不过 `can_pull()` 门禁。
    let module = list_events::register(module, Arc::clone(&context));
    let module = retry_event::register(module, Arc::clone(&context));
    // 飞书用户身份（OAuth）：状态与授权链接是控制台 Action，无条件注册
    // （授权链接在缺配置时按业务码 50302 失败，而不是让按钮凭空消失）。
    let module = oauth_status::register(module, Arc::clone(&context));
    let module = oauth_authorize_url::register(module, Arc::clone(&context));
    let module = rotate_config_token::register(module, Arc::clone(&context));
    let module = reveal_config_token::register(module, Arc::clone(&context));

    let Some(settings) = settings.filter(|value| value.can_pull()) else {
        return Ok(module);
    };
    // 门禁只判断 `can_pull()`，具体值本身不再被后续使用（中间件已移除）。
    let _ = settings;

    // 授权回调要打飞书的令牌端点，与派发端点同一条门禁。
    let module = oauth_callback::register(module, Arc::clone(&context));
    let module = dispatch::register(module, context);
    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::*;

    use yang_base::definition::{HttpMethod, ModuleName, TableSpec};

    use crate::addon::feishu::domain::repository::Repository;

    /// 14 个控制台 Action 的路由：两态（有/无凭证）都必须注册。
    const CONSOLE_ROUTES: &[(&str, &str)] = &[
        ("list_configs", "/api/v1/feishu/approval/configs/query"),
        ("create_config", "/api/v1/feishu/approval/configs/create"),
        ("update_config", "/api/v1/feishu/approval/configs/update"),
        ("delete_config", "/api/v1/feishu/approval/configs/delete"),
        (
            "refresh_names",
            "/api/v1/feishu/approval/configs/refresh-names",
        ),
        (
            "rotate_config_token",
            "/api/v1/feishu/approval/configs/rotate-token",
        ),
        (
            "reveal_config_token",
            "/api/v1/feishu/approval/configs/reveal-token",
        ),
        (
            "list_widgets",
            "/api/v1/feishu/approval/definitions/widgets",
        ),
        ("list_requests", "/api/v1/feishu/approval/requests/query"),
        ("list_tasks", "/api/v1/feishu/approval/tasks/query"),
        ("list_events", "/api/v1/feishu/approval/events/query"),
        ("retry_event", "/api/v1/feishu/approval/events/{id}/retry"),
        ("oauth_status", "/api/v1/feishu/oauth/status"),
        ("oauth_authorize_url", "/api/v1/feishu/oauth/authorize-url"),
    ];

    /// 装配好的 module（带一个可用的 `FeishuSettings`）。
    ///
    /// 用惰性连接池：这些用例只读路由表，不碰数据库。
    pub(super) fn registered_module(with_credentials: bool) -> ModuleSpec {
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
            repository(crate::addon::feishu::approval::domain::task_applicant_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::user_token_table::table_spec()),
            repository(crate::addon::feishu::portal::table::table_spec()),
            repository(crate::addon::feishu::portal::domain::log_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::event_table::table_spec()),
            Arc::new(
                yang_base::action::auth::PasswordEngine::new(1)
                    .unwrap_or_else(|e| panic!("PasswordEngine 应可构造: {e}")),
            ),
            None,
        ));

        let settings = if with_credentials {
            Some(FeishuSettings {
                enabled: true,
                management_api_token: "a-real-management-token-value-1234".to_string(),
                encryption_key: None,
                app_id: Some("cli_real_app_id".to_string()),
                app_secret: Some("real-app-secret-value".to_string()),
                oauth_redirect_uri: None,
                pull_interval_seconds: 900,
                alert_recipients: Vec::new(),
                alert_failure_threshold: 3,
                log_inbound_requests: false,
                approval_create_rate_per_minute: 90,
                approval_scan_interval_seconds: 30,
                approval_base_timezone: "Asia/Shanghai".to_string(),
                event_verification_token: None,
                event_encrypt_key: None,
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

    /// 14 个控制台路由在**有凭证**时全部注册。
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

    /// 14 个控制台路由在**无凭证**时也全部注册（控制台不依赖出站凭证存在）。
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
                "/api/v1/feishu/approval/dispatch/{config_key}".to_string()
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
                "/api/v1/feishu/approval/dispatch/{config_key}".to_string()
            )),
            "无凭证时不得注册派发路由"
        );
    }

    /// 授权回调与派发端点同门禁：要打飞书令牌端点，缺凭证时 404 比「注册出来
    /// 永远失败」语义更准。
    #[tokio::test]
    async fn oauth_callback_follows_the_outbound_gate() {
        let with_credentials = registered_module(true);
        assert!(
            routes(&with_credentials)
                .contains(&(HttpMethod::Get, "/api/v1/feishu/oauth/callback".to_string())),
            "带凭证时授权回调必须注册：{:?}",
            routes(&with_credentials)
        );

        let without_credentials = registered_module(false);
        assert!(
            !routes(&without_credentials)
                .contains(&(HttpMethod::Get, "/api/v1/feishu/oauth/callback".to_string())),
            "无凭证时不得注册授权回调"
        );
    }

    #[tokio::test]
    async fn dispatch_action_is_public_for_the_machine_caller() {
        // 飞书多维表格工作流没有 JWT，所以 Action 必须是 public，靠 handler 内
        // 按 `config_key` 校验的访问 Token 做鉴权。
        let module = registered_module(true);
        let spec = module
            .actions()
            .iter()
            .find(|spec| spec.route.path == "/api/v1/feishu/approval/dispatch/{config_key}")
            .unwrap_or_else(|| panic!("派发路由必须存在"));
        assert!(spec.is_public, "派发 Action 必须 public（工作流无 JWT）");
    }
}

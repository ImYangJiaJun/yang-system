use crate::addon::{access, account, demo};
use crate::authorization::StepUpServices;
use crate::authorization::{AuthorizationVersionCache, AuthorizationVersionValidator};
use crate::config::SecuritySettings;
use anyhow::Context;
use std::sync::Arc;
use yang_base::action::StepUpManager;
use yang_base::definition::{AppBuilder, BuiltApp};
use yang_base::tools::Tools;
use yang_runtime::observability::{ActionLogMiddleware, LogIdentity, RuntimeMetricNames};

pub(crate) const YANG_SYSTEM_METRIC_NAMES: RuntimeMetricNames = RuntimeMetricNames::new(
    "yang_system_action_requests_total",
    "yang_system_action_duration_seconds",
    "yang_system_build_info",
    "yang_system_readiness_checks_total",
    "yang_system_readiness_duration_seconds",
    "yang_system_readiness_ready",
    "yang_system_readiness_resource_healthy",
    "yang_system_resource_pool_connections",
);

pub struct Application {
    pub runtime: BuiltApp,
}

pub fn build_app(
    tools: Arc<Tools>,
    security: Arc<SecuritySettings>,
) -> anyhow::Result<Application> {
    let step_up_manager = tools
        .extension::<Arc<StepUpManager>>()
        .context("运行应用缺少 StepUpManager 扩展")?
        .clone();
    let step_up = StepUpServices::production(step_up_manager, tools.cache()?.clone())
        .context("构建生产 Step-up proof store 失败")?;
    build_application(tools, security, Some(step_up))
}

pub(crate) fn build_schema_app(
    tools: Arc<Tools>,
    security: Arc<SecuritySettings>,
) -> anyhow::Result<Application> {
    build_application(tools, security, None)
}

/// 构建仅用于元数据导出的应用（无 Step-up、无 Redis 依赖），
/// 供 `openapi-dump` 等开发期契约工具使用；目录内容与运行时同源。
pub fn build_metadata_app(
    tools: Arc<Tools>,
    security: Arc<SecuritySettings>,
) -> anyhow::Result<Application> {
    build_application(tools, security, None)
}

fn build_application(
    tools: Arc<Tools>,
    security: Arc<SecuritySettings>,
    step_up: Option<StepUpServices>,
) -> anyhow::Result<Application> {
    let authorization_cache = match tools.cache() {
        Ok(_) => Some(
            tools
                .extension::<AuthorizationVersionCache>()
                .context("Redis 运行态缺少 AuthorizationVersionCache 扩展")?
                .clone(),
        ),
        Err(yang_base::BaseError::RedisNotInitialized) => None,
        Err(error) => return Err(error).context("检查授权版本缓存运行态失败"),
    };
    // 授权失效公共端口：account 域提供唯一实现（单一 writer 语义不变），
    // 校验器只依赖读端口抽象，业务 Addon 经 AuthorizationPort 句柄使用。
    let authorization_port = account::authorization_port();
    let authorization_validator =
        AuthorizationVersionValidator::new(authorization_cache, Some(authorization_port.source()));
    let action_logging = ActionLogMiddleware::new(LogIdentity::from_tools(&tools));
    // 应用组合根只决定启用哪些 Addon；Addon 内部包含哪些 Module 由各领域自己维护。
    // access 提供授权存储与权限目录；账号域在 Token 签发时经 GrantResolver 合并直授权限。
    let permission_catalog = access::PermissionCatalogHandle::new();
    let access = access::build_addon(
        authorization_validator.clone(),
        step_up.clone(),
        permission_catalog.clone(),
        authorization_port,
    )
    .context("构建 access Addon 失败")?;
    let grant_resolvers: Vec<Arc<dyn account::GrantResolver>> = vec![access.grant_resolver()];
    let system_owner_claimer = account::no_system_owner_claimer();
    let demo =
        demo::build_addon(authorization_validator.clone()).context("构建 demo Addon 失败")?;
    let builder = AppBuilder::new()
        .addon(
            account::build_addon(
                Arc::clone(&security),
                Arc::new(account::CompositeGrantResolver::new(grant_resolvers)),
                system_owner_claimer,
                authorization_validator,
                step_up,
            )
            .context("构建 account Addon 失败")?
            .middleware(action_logging),
        )
        .addon(
            access
                .into_spec()
                .middleware(ActionLogMiddleware::new(LogIdentity::from_tools(&tools))),
        )
        .addon(demo.middleware(ActionLogMiddleware::new(LogIdentity::from_tools(&tools))));
    let runtime = builder
        .build(tools)
        .context("构建应用定义与 Registry 失败")?;
    // 决策 D3：Catalog 冻结后投影权限目录并安装一次，运行期只读。
    permission_catalog
        .install(access::project_permissions(runtime.catalog().addons()))
        .context("安装权限目录投影失败")?;

    Ok(Application { runtime })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::Algorithm;
    use sqlx::mysql::MySqlPoolOptions;
    use yang_base::definition::{ActionName, ActionRef, ModuleName};
    use yang_base::token::TokenManager;
    use yang_base::tools::ToolsBuilder;
    use yang_db::{Database, DatabaseConfig};

    fn test_tools() -> Arc<Tools> {
        let pool = MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"));
        let mysql = Database::from_pool(pool.clone(), DatabaseConfig::default())
            .unwrap_or_else(|error| panic!("测试 Database 应构建成功: {error}"));
        Arc::new(
            ToolsBuilder::new()
                .mysql(mysql)
                .token(TokenManager::new_symmetric(
                    "01234567890123456789012345678901",
                    Algorithm::HS256,
                    "test".to_string(),
                    "test-api".to_string(),
                    60,
                    120,
                ))
                .extension(Arc::new(
                    StepUpManager::new(
                        "independent-step-up-test-secret-0123456789abcdef",
                        "test-step-up",
                        "test-sensitive-actions",
                    )
                    .unwrap_or_else(|error| panic!("测试 Step-up manager 应有效: {error}")),
                ))
                .build()
                .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}")),
        )
    }

    fn test_security() -> Arc<SecuritySettings> {
        Arc::new(SecuritySettings {
            argon2_max_concurrency: 1,
            auth_rate_limit_window_seconds: 60,
            auth_rate_limit_ip_attempts: 30,
            auth_rate_limit_username_attempts: 10,
            password_reset_ttl_seconds: 900,
            issue_refresh_credential_version: true,
            trusted_proxy_cidrs: Vec::new(),
        })
    }

    /// account + access 骨架冒烟：应用必须可构建并产出两个 Addon 的 Catalog 与授权事实表。
    #[tokio::test]
    async fn enabled_feature_combination_builds_and_exposes_account_catalog() {
        let app = build_application(test_tools(), test_security(), None)
            .unwrap_or_else(|error| panic!("当前 feature 组合的应用应构建成功: {error:#}"));
        let module = app
            .runtime
            .catalog()
            .addons()
            .iter()
            .flat_map(|addon| &addon.modules)
            .find(|module| module.name.as_str() == "account.user")
            .unwrap_or_else(|| panic!("应存在 account.user 模块"));
        assert!(module
            .actions()
            .iter()
            .any(|action| action.name.as_str() == "register"));
        assert!(app
            .runtime
            .catalog()
            .addons()
            .iter()
            .flat_map(|addon| &addon.modules)
            .any(|module| module.name.as_str() == "access.grants"));
        assert!(app
            .runtime
            .table_definitions()
            .iter()
            .any(|definition| definition.name() == "users"));
        assert!(app
            .runtime
            .table_definitions()
            .iter()
            .any(|definition| definition.name() == "authz_grant"));
        let reference = ActionRef::new(
            ModuleName::new("account.user")
                .unwrap_or_else(|error| panic!("ModuleName 应有效: {error}")),
            ActionName::new("me").unwrap_or_else(|error| panic!("ActionName 应有效: {error}")),
        );
        assert!(app.runtime.registry().resolve(&reference).is_some());
        let access_module = app
            .runtime
            .catalog()
            .addons()
            .iter()
            .flat_map(|addon| &addon.modules)
            .find(|module| module.name.as_str() == "access.grants")
            .unwrap_or_else(|| panic!("应存在 access.grants 模块"));
        for action_name in [
            "grant_permission",
            "revoke_permission",
            "list_user_grants",
            "list_permissions",
        ] {
            assert!(
                access_module
                    .actions()
                    .iter()
                    .any(|action| action.name.as_str() == action_name),
                "access.grants 应注册 {action_name}"
            );
        }
        // 决策 D3：权限目录投影自 Catalog，必须包含管理 Action 声明的权限。
        let entries = access::project_permissions(app.runtime.catalog().addons());
        let projected: Vec<&str> = entries.iter().map(|entry| entry.permission()).collect();
        assert!(projected.contains(&"access.grants.read"));
        assert!(projected.contains(&"access.grants.write"));
    }

    /// P7 演示 addon 冒烟：demo.notes 全流程接入（Catalog、表、权限目录、投影边界）。
    #[tokio::test]
    async fn demo_addon_builds_and_projects_only_for_authorized_identities() {
        use yang_base::action::Request;

        let app = build_application(test_tools(), test_security(), None)
            .unwrap_or_else(|error| panic!("当前 feature 组合的应用应构建成功: {error:#}"));
        let demo_module = app
            .runtime
            .catalog()
            .addons()
            .iter()
            .flat_map(|addon| &addon.modules)
            .find(|module| module.name.as_str() == "demo.notes")
            .unwrap_or_else(|| panic!("应存在 demo.notes 模块"));
        assert!(app
            .runtime
            .table_definitions()
            .iter()
            .any(|definition| definition.name() == "demo_note"));
        for action_name in ["create_note", "update_note", "delete_note", "list_notes"] {
            assert!(
                demo_module
                    .actions()
                    .iter()
                    .any(|action| action.name.as_str() == action_name),
                "demo.notes 应注册 {action_name}"
            );
        }
        // 权限目录投影必须包含演示 Action 声明的权限（access 管线对新业务生效）。
        let entries = access::project_permissions(app.runtime.catalog().addons());
        let projected: Vec<&str> = entries.iter().map(|entry| entry.permission()).collect();
        assert!(projected.contains(&"demo.notes.read"));
        assert!(projected.contains(&"demo.notes.write"));

        // 投影边界：匿名请求的 UI Catalog 不得出现任何 demo.notes 内容；
        // 持有 demo.notes.* 权限的请求由同一投影逻辑放行（授权身份只能经
        // TokenAuthMiddleware 注入，应用单测无法伪造，授权路径由框架投影测试覆盖）。
        let anonymous = app
            .runtime
            .ui_catalog(&app.runtime.context(Request::new(serde_json::json!({}))))
            .unwrap_or_else(|error| panic!("匿名 UI Catalog 应可计算: {error}"));
        assert!(anonymous
            .modules
            .iter()
            .all(|module| module.module_id != "demo.notes"));
        assert!(anonymous
            .table_views
            .iter()
            .all(|view| !view.view_id.starts_with("demo.notes")));
        assert!(anonymous
            .actions
            .iter()
            .all(|action| !action.operation_id.starts_with("demo.notes.")));

        // 冻结 Catalog 中 demo.notes 声明了一个通用 TableView（前端零代码的载体）。
        assert_eq!(
            demo_module.views.len(),
            1,
            "demo.notes 应声明一个主 TableView"
        );
        let view = &demo_module.views[0];
        assert_eq!(
            view.data_action
                .as_ref()
                .map(|action| action.action().as_str()),
            Some("list_notes")
        );
        assert_eq!(view.fields.len(), 5);
    }

    /// OpenAPI spike（前端重构 ADR-4 检查点 0）：Catalog 投影的 OpenAPI 3.1 文档
    /// 必须覆盖全部已注册 Action 的路由与方法，operationId 与 Catalog 一致，
    /// 且写操作携带非空输入 Schema。该契约是前端 openapi-typescript 轨道的前提。
    #[tokio::test]
    async fn openapi_projection_covers_all_catalog_actions() {
        let app = build_application(test_tools(), test_security(), None)
            .unwrap_or_else(|error| panic!("应用应构建成功: {error:#}"));
        let document = app
            .runtime
            .catalog()
            .to_openapi(yang_base::definition::OpenApiInfo::new(
                "yang-system",
                "0.1.0",
            ))
            .unwrap_or_else(|error| panic!("Catalog 应投影 OpenAPI 文档: {error}"));
        assert_eq!(document["openapi"], "3.1.0", "应为 OpenAPI 3.1 文档");
        let paths = document["paths"]
            .as_object()
            .unwrap_or_else(|| panic!("OpenAPI 文档应包含 paths 对象"));
        let mut checked = 0usize;
        let mut write_with_input_schema = 0usize;
        for module in app
            .runtime
            .catalog()
            .addons()
            .iter()
            .flat_map(|addon| &addon.modules)
        {
            for action in module.actions() {
                let path_item = paths.get(&action.route.path).unwrap_or_else(|| {
                    panic!(
                        "OpenAPI 缺少路径 {}（{}）",
                        action.route.path, action.route.operation_id
                    )
                });
                let operation = path_item
                    .get(action.route.method.as_str().to_lowercase())
                    .unwrap_or_else(|| {
                        panic!(
                            "OpenAPI 路径 {} 缺少 {} 操作",
                            action.route.path,
                            action.route.method.as_str()
                        )
                    });
                assert_eq!(
                    operation["operationId"],
                    serde_json::json!(action.route.operation_id),
                    "operationId 应与 Catalog 一致"
                );
                if matches!(
                    action.route.method,
                    yang_base::definition::HttpMethod::Post
                        | yang_base::definition::HttpMethod::Put
                        | yang_base::definition::HttpMethod::Patch
                ) && !action.input_schema.is_null()
                {
                    let schema = &operation["requestBody"]["content"][if action.request_media_type
                        == yang_base::definition::ActionMediaType::Multipart
                    {
                        "multipart/form-data"
                    } else {
                        "application/json"
                    }]["schema"];
                    assert!(
                        !schema.is_null(),
                        "写操作 {} 的 OpenAPI requestBody 应携带输入 Schema",
                        action.route.operation_id
                    );
                    write_with_input_schema += 1;
                }
                checked += 1;
            }
        }
        assert!(
            checked >= 15,
            "覆盖检查应实际执行足够数量的 Action（实际 {checked}）"
        );
        assert!(
            write_with_input_schema >= 5,
            "应有足够写操作携带输入 Schema（实际 {write_with_input_schema}）"
        );
    }
}

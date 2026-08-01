use crate::authorization::StepUpServices;
use crate::authorization::{AuthorizationVersionCache, AuthorizationVersionValidator};
use crate::config::SecuritySettings;
use crate::modules::{account, admin, observability, org, work};
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
    let authorization_validator = AuthorizationVersionValidator::new(authorization_cache);
    let action_logging = ActionLogMiddleware::new(LogIdentity::from_tools(&tools));
    // 应用组合根只决定启用哪些 Addon；Addon 内部包含哪些 Module 由各领域自己维护。
    let runtime = AppBuilder::new()
        .addon(
            account::build_addon(
                Arc::clone(&security),
                Arc::new(account::CompositeGrantResolver::new(vec![
                    admin::grant_resolver(),
                    org::grant_resolver(),
                    work::grant_resolver(),
                ])),
                authorization_validator.clone(),
                step_up.clone(),
            )
            .context("构建 account Addon 失败")?
            .middleware(action_logging.clone()),
        )
        .addon(
            admin::build_addon(security, authorization_validator.clone(), step_up.clone())
                .context("构建 admin Addon 失败")?
                .middleware(action_logging.clone()),
        )
        .addon(
            observability::build_addon(authorization_validator.clone())
                .context("构建 observability Addon 失败")?
                .middleware(action_logging.clone()),
        )
        .addon(
            org::build_addon(authorization_validator.clone(), step_up)
                .context("构建 org Addon 失败")?
                .middleware(action_logging.clone()),
        )
        .addon(
            work::build_addon(authorization_validator)
                .context("构建 work Addon 失败")?
                .middleware(action_logging),
        )
        .build(tools)
        .context("构建应用定义与 Registry 失败")?;

    Ok(Application { runtime })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::Algorithm;
    use sqlx::mysql::MySqlPoolOptions;
    use yang_base::action::{Request, TenantContext, TenantId};
    use yang_base::definition::{ActionName, ActionRef, ModuleName, OpenApiInfo};
    use yang_base::token::TokenManager;
    use yang_base::tools::ToolsBuilder;
    use yang_db::{Database, DatabaseConfig};

    #[tokio::test]
    async fn catalog_and_registry_are_built_from_the_same_actions() {
        let pool = MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"));
        let mysql = Database::from_pool(pool.clone(), DatabaseConfig::default())
            .unwrap_or_else(|error| panic!("测试 Database 应构建成功: {error}"));
        let tools = Arc::new(
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
        );
        let security = Arc::new(SecuritySettings {
            argon2_max_concurrency: 1,
            auth_rate_limit_window_seconds: 60,
            auth_rate_limit_ip_attempts: 30,
            auth_rate_limit_username_attempts: 10,
            password_reset_ttl_seconds: 900,
            issue_refresh_credential_version: true,
            trusted_proxy_cidrs: Vec::new(),
        });
        let compatibility_security = Arc::new(SecuritySettings {
            issue_refresh_credential_version: false,
            ..(*security).clone()
        });
        let test_step_up = StepUpServices::in_memory(
            tools
                .extension::<Arc<StepUpManager>>()
                .unwrap_or_else(|error| panic!("测试 Tools 应有 Step-up manager: {error}"))
                .clone(),
        );
        let compatibility_app = build_application(
            Arc::clone(&tools),
            compatibility_security,
            Some(test_step_up.clone()),
        )
        .unwrap_or_else(|error| panic!("兼容阶段应用应构建成功: {error:#}"));
        let change_password_ref = ActionRef::new(
            ModuleName::new("account.user")
                .unwrap_or_else(|error| panic!("ModuleName 应有效: {error}")),
            ActionName::new("change_password")
                .unwrap_or_else(|error| panic!("ActionName 应有效: {error}")),
        );
        assert!(
            compatibility_app
                .runtime
                .registry()
                .resolve(&change_password_ref)
                .is_none(),
            "协议兼容阶段不得开放会制造非零凭据版本的 Action"
        );
        let app = build_application(tools, security, Some(test_step_up))
            .unwrap_or_else(|error| panic!("应用应构建成功: {error:#}"));
        let module = app
            .runtime
            .catalog()
            .addons()
            .iter()
            .flat_map(|addon| &addon.modules)
            .find(|module| module.name.as_str() == "account.user")
            .unwrap_or_else(|| panic!("应存在 account.user 模块"));
        let operations: Vec<_> = module
            .actions()
            .iter()
            .map(|action| {
                (
                    action.route.operation_id.as_str(),
                    action.route.method.as_str(),
                    action.route.path.as_str(),
                    action.success_status,
                    action.is_public,
                )
            })
            .collect();

        assert_eq!(app.runtime.table_definitions().len(), 6);
        let tables = app
            .runtime
            .table_definitions()
            .iter()
            .map(|definition| definition.name())
            .collect::<Vec<_>>();
        assert!(tables.contains(&"users"));
        assert!(tables.contains(&"admin_user"));
        assert!(tables.contains(&"org_org"));
        assert!(tables.contains(&"org_user"));
        assert!(tables.contains(&"work_project"));
        assert!(tables.contains(&"work_task"));
        assert_eq!(operations.len(), 11);
        assert!(operations.contains(&(
            "account.user.request_registration_email",
            "POST",
            "/api/v1/users/registration-email-verifications",
            202,
            true,
        )));
        assert!(operations.contains(&(
            "account.user.register",
            "POST",
            "/api/v1/users/register",
            201,
            true,
        )));
        assert!(operations.contains(&(
            "account.user.step_up_complete",
            "POST",
            "/api/v1/users/step-up/complete",
            200,
            true,
        )));
        assert!(operations.contains(&(
            "account.user.change_password",
            "POST",
            "/api/v1/users/change-password",
            200,
            false,
        )));
        assert!(operations.contains(&(
            "account.user.disable_self",
            "POST",
            "/api/v1/users/disable",
            200,
            false,
        )));
        assert!(operations.contains(&(
            "account.user.reset_password",
            "POST",
            "/api/v1/users/reset-password",
            200,
            true,
        )));
        let reference = ActionRef::new(
            ModuleName::new("account.user")
                .unwrap_or_else(|error| panic!("ModuleName 应有效: {error}")),
            ActionName::new("me").unwrap_or_else(|error| panic!("ActionName 应有效: {error}")),
        );
        assert!(app.runtime.registry().resolve(&reference).is_some());

        let document = app
            .runtime
            .catalog()
            .to_openapi(OpenApiInfo::new("yang-system", "0.1.0"))
            .unwrap_or_else(|error| panic!("Catalog 应生成 OpenAPI: {error}"));
        assert_eq!(
            document["paths"]["/api/v1/orgs"]["get"]["operationId"],
            "org.org.list"
        );
        assert_eq!(
            document["paths"]["/api/v1/orgs/options"]["post"]["operationId"],
            "org.org.select"
        );
        assert_eq!(app.runtime.compiled_views().len(), 7);

        let work_task_module = app
            .runtime
            .catalog()
            .addons()
            .iter()
            .flat_map(|addon| &addon.modules)
            .find(|module| module.name.as_str() == "work.task")
            .unwrap_or_else(|| panic!("应存在 work.task 模块"));
        assert_eq!(work_task_module.views.len(), 2);
        assert!(work_task_module
            .actions()
            .iter()
            .any(|action| action.name.as_str() == "complete"));

        for module in app
            .runtime
            .catalog()
            .addons()
            .iter()
            .flat_map(|addon| &addon.modules)
        {
            for action in module.actions() {
                assert_eq!(
                    action.route.operation_id,
                    format!("{}.{}", module.name, action.name),
                    "operation_id 必须与 Action 身份同源"
                );
                if action.name.as_str() != "ui_catalog" {
                    assert!(
                        action.route.path.starts_with("/api/v1/"),
                        "业务 Action 必须使用 /api/v1 前缀: {}",
                        action.route.path
                    );
                }
            }
        }

        let tenant_module = app
            .runtime
            .catalog()
            .addons()
            .iter()
            .flat_map(|addon| &addon.modules)
            .find(|module| module.name.as_str() == "org.tenant")
            .unwrap_or_else(|| panic!("应存在不依赖租户上下文的 org.tenant 模块"));
        let tenant_list = tenant_module
            .actions()
            .iter()
            .find(|action| action.name.as_str() == "list")
            .unwrap_or_else(|| panic!("应存在租户发现 Action"));
        assert_eq!(tenant_list.route.method.as_str(), "GET");
        assert_eq!(tenant_list.route.path.as_str(), "/api/v1/tenants");
        assert!(!tenant_list.is_public);
        let tenant_create = tenant_module
            .actions()
            .iter()
            .find(|action| action.name.as_str() == "create")
            .unwrap_or_else(|| panic!("应存在租户初始化 Action"));
        assert_eq!(tenant_create.route.method.as_str(), "POST");
        assert_eq!(tenant_create.route.path.as_str(), "/api/v1/tenants");
        assert_eq!(tenant_create.success_status, 201);
        assert!(!tenant_create.is_public);

        let org_user = app
            .runtime
            .table_definitions()
            .iter()
            .find(|definition| definition.name() == "org_user")
            .unwrap_or_else(|| panic!("应存在 org_user 表"));
        let missing_tenant = app
            .runtime
            .context(Request::new(serde_json::json!({})))
            .with_table_definition(org_user.clone())
            .table_query();
        assert!(matches!(
            missing_tenant,
            Err(yang_base::BaseError::Unauthorized(_))
        ));
        let tenant_query = app
            .runtime
            .context(Request::new(serde_json::json!({})))
            .with_table_definition(org_user.clone())
            .with_tenant(TenantContext::new(TenantId::new(7)))
            .table_query();
        assert!(tenant_query.is_ok());

        assert!(operations.iter().any(|operation| {
            operation.0 == "account.user.ui_catalog"
                && operation.1 == "GET"
                && operation.2 == "/.well-known/yang/ui-catalog"
                && operation.4
        }));

        let observability_module = app
            .runtime
            .catalog()
            .addons()
            .iter()
            .flat_map(|addon| &addon.modules)
            .find(|module| module.name.as_str() == "system.observability")
            .unwrap_or_else(|| panic!("应存在 system.observability 模块"));
        let frontend_error_report = observability_module
            .actions()
            .iter()
            .find(|action| action.name.as_str() == "report_frontend_error")
            .unwrap_or_else(|| panic!("应存在前端错误关联 Action"));
        assert_eq!(
            frontend_error_report.route.operation_id,
            "system.observability.report_frontend_error"
        );
        assert_eq!(
            frontend_error_report.route.path,
            "/api/v1/observability/frontend-errors"
        );
        assert!(
            !frontend_error_report.is_public,
            "前端错误上报必须要求已认证会话，避免公开日志与指标放大"
        );

        let admin_user_module = app
            .runtime
            .catalog()
            .addons()
            .iter()
            .flat_map(|addon| &addon.modules)
            .find(|module| module.name.as_str() == "admin.user")
            .unwrap_or_else(|| panic!("应存在 admin.user 模块"));
        let create_password_reset = admin_user_module
            .actions()
            .iter()
            .find(|action| action.name.as_str() == "create_password_reset")
            .unwrap_or_else(|| panic!("应存在 admin.user.create_password_reset"));
        assert_eq!(
            create_password_reset.route.path.as_str(),
            "/api/v1/admin/users/password-reset"
        );
        assert!(!create_password_reset.is_public);
        let bootstrap = admin_user_module
            .actions()
            .iter()
            .find(|action| action.name.as_str() == "bootstrap")
            .unwrap_or_else(|| panic!("应存在 admin.user.bootstrap"));
        assert_eq!(bootstrap.route.path.as_str(), "/api/v1/admin/bootstrap");
        assert_eq!(bootstrap.success_status, 201);
        assert!(!bootstrap.is_public);
        assert!(bootstrap.permissions.is_empty());
        let admin_action = |name: &str| {
            admin_user_module
                .actions()
                .iter()
                .find(|action| action.name.as_str() == name)
                .unwrap_or_else(|| panic!("应存在 admin.user.{name}"))
        };
        assert_eq!(admin_action("list").permissions, ["admin.user:read"]);
        for name in ["add", "set_status", "set_admin"] {
            assert_eq!(admin_action(name).permissions, ["admin.user:write"]);
        }
        assert_eq!(admin_action("list").route.path, "/api/v1/admin/users");
        assert_eq!(admin_action("add").route.path, "/api/v1/admin/users");
        assert_eq!(
            admin_action("set_status").route.path,
            "/api/v1/admin/users/status"
        );
        assert_eq!(
            admin_action("set_admin").route.path,
            "/api/v1/admin/users/admin"
        );

        let org_user_module = app
            .runtime
            .catalog()
            .addons()
            .iter()
            .flat_map(|addon| &addon.modules)
            .find(|module| module.name.as_str() == "org.user")
            .unwrap_or_else(|| panic!("应存在 org.user 模块"));
        let action = |name: &str| {
            org_user_module
                .actions()
                .iter()
                .find(|action| action.name.as_str() == name)
                .unwrap_or_else(|| panic!("应存在 org.user.{name}"))
        };
        for name in ["get", "select", "table"] {
            assert_eq!(action(name).permissions, vec!["org.user:read"]);
        }
        assert_eq!(action("add").route.path, "/api/v1/org/users");
        assert_eq!(action("select").route.path, "/api/v1/org/users/query");
        assert_eq!(action("table").route.path, "/api/v1/org/users/schema");
        for name in ["add", "put", "del"] {
            assert_eq!(action(name).permissions, vec!["org.user:write"]);
        }
        let membership_table = app
            .runtime
            .table_definitions()
            .iter()
            .find(|definition| definition.name() == "org_user")
            .unwrap_or_else(|| panic!("应存在 org_user 表"));
        for field in ["name", "position", "email", "phone", "admin", "updated_at"] {
            assert!(
                membership_table.field(field).is_some(),
                "org_user 应包含基础账号字段 {field}"
            );
        }
        let view = org_user_module
            .views
            .iter()
            .find(|view| view.name.as_str() == "list")
            .unwrap_or_else(|| panic!("应存在 org.user.list View"));
        assert!(view.actions.contains(&yang_base::action!("org.user.add")));
        assert!(view.actions.contains(&yang_base::action!("org.user.put")));
        assert!(view.actions.contains(&yang_base::action!("org.user.del")));
        let delete = view
            .action_presentations
            .get(&yang_base::action!("org.user.del"))
            .unwrap_or_else(|| panic!("删除操作应声明展示语义"));
        assert_eq!(
            delete.placement,
            yang_base::definition::ActionPlacement::Row
        );
        assert_eq!(
            delete.interaction,
            yang_base::definition::ActionInteraction::Invoke
        );
        assert!(delete.confirmation.is_some());
    }
}

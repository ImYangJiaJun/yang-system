use crate::authorization::AuthorizationVersionCache;
use crate::config::SecuritySettings;
use crate::modules::{account, admin, org};
use anyhow::Context;
use std::sync::Arc;
use yang_base::definition::{AppBuilder, BuiltApp};
use yang_base::tools::Tools;

pub struct Application {
    pub runtime: BuiltApp,
}

pub fn build_app(
    tools: Arc<Tools>,
    security: Arc<SecuritySettings>,
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
    // 应用组合根只决定启用哪些 Addon；Addon 内部包含哪些 Module 由各领域自己维护。
    let runtime = AppBuilder::new()
        .addon(
            account::build_addon(
                security,
                Arc::new(account::CompositeGrantResolver::new(vec![
                    admin::grant_resolver(),
                    org::grant_resolver(),
                ])),
                authorization_cache,
            )
            .context("构建 account Addon 失败")?,
        )
        .addon(admin::build_addon().context("构建 admin Addon 失败")?)
        .addon(org::build_addon().context("构建 org Addon 失败")?)
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
                .build()
                .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}")),
        );
        let security = Arc::new(SecuritySettings {
            argon2_max_concurrency: 1,
            auth_rate_limit_window_seconds: 60,
            auth_rate_limit_ip_attempts: 30,
            auth_rate_limit_username_attempts: 10,
        });
        let app =
            build_app(tools, security).unwrap_or_else(|error| panic!("应用应构建成功: {error}"));
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

        assert_eq!(app.runtime.table_definitions().len(), 4);
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
        assert_eq!(operations.len(), 6);
        assert!(operations.contains(&(
            "account.user.register",
            "POST",
            "/api/v1/users/register",
            201,
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
        assert_eq!(app.runtime.compiled_views().len(), 4);

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

        let admin_user_module = app
            .runtime
            .catalog()
            .addons()
            .iter()
            .flat_map(|addon| &addon.modules)
            .find(|module| module.name.as_str() == "admin.user")
            .unwrap_or_else(|| panic!("应存在 admin.user 模块"));
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

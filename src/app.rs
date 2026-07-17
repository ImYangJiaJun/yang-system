use crate::config::SecuritySettings;
use crate::modules::org;
use crate::modules::user;
use anyhow::Context;
use std::sync::Arc;
use yang_base::definition::{AddonName, AddonSpec, AppBuilder, BuiltApp};
use yang_base::tools::Tools;

pub struct Application {
    pub runtime: BuiltApp,
}

pub fn build_app(
    tools: Arc<Tools>,
    security: Arc<SecuritySettings>,
) -> anyhow::Result<Application> {
    let user_module = user::build_module(security)?;
    let account = AddonSpec::new(AddonName::new("account").context("account Addon 名称无效")?)
        .module(user_module);
    let runtime = AppBuilder::new()
        .addon(account)
        .addon(org::OrgAddon)
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
            username_min_length: 3,
            username_max_length: 64,
            password_min_length: 10,
            password_max_length: 128,
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

        assert_eq!(app.runtime.table_definitions().len(), 3);
        let tables = app
            .runtime
            .table_definitions()
            .iter()
            .map(|definition| definition.name())
            .collect::<Vec<_>>();
        assert!(tables.contains(&"users"));
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
        assert_eq!(app.runtime.compiled_views().len(), 3);

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
    }
}

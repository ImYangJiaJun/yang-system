use crate::addon::{access, account};
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
    // access 提供授权存储与权限目录；账号域在 Token 签发时经 GrantResolver 合并直授权限。
    let permission_catalog = access::PermissionCatalogHandle::new();
    let access = access::build_addon(
        authorization_validator.clone(),
        step_up.clone(),
        permission_catalog.clone(),
    )
    .context("构建 access Addon 失败")?;
    let grant_resolvers: Vec<Arc<dyn account::GrantResolver>> = vec![access.grant_resolver()];
    let system_owner_claimer = account::no_system_owner_claimer();
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
        );
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
}

use crate::config::SecuritySettings;
use crate::modules::account;
use sqlx::MySqlPool;
use std::sync::Arc;
use yang_base::router::AppRouter;
use yang_base::BaseError;

pub fn build_app_router(
    pool: Arc<MySqlPool>,
    security: Arc<SecuritySettings>,
) -> Result<AppRouter, BaseError> {
    let modules = account::build_modules(pool, security)?;
    AppRouter::new()
        .register_module(modules.authentication)?
        .register_module(modules.account)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::mysql::MySqlPoolOptions;

    #[tokio::test]
    async fn catalog_is_the_single_route_and_schema_source() {
        let pool = MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"));
        let security = Arc::new(SecuritySettings {
            username_min_length: 3,
            username_max_length: 64,
            password_min_length: 10,
            password_max_length: 128,
        });
        let router = build_app_router(Arc::new(pool), security)
            .unwrap_or_else(|error| panic!("应用路由应构建成功: {error}"));
        let catalog = router
            .catalog()
            .unwrap_or_else(|error| panic!("API Catalog 应构建成功: {error}"));
        let operations: Vec<_> = catalog
            .modules
            .iter()
            .flat_map(|module| module.actions.iter())
            .map(|action| (action.route.operation_id.as_str(), action.is_public))
            .collect();

        assert_eq!(router.table_configs().len(), 1);
        assert_eq!(router.table_configs()[0].table_name, "accounts");
        assert!(operations.contains(&("accounts.register", true)));
        assert!(operations.contains(&("accounts.login", true)));
        assert!(operations.contains(&("accounts.refresh", true)));
        assert!(operations.contains(&("accounts.logout", true)));
        assert!(operations.contains(&("accounts.me", false)));
    }
}

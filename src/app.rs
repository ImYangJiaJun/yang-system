use crate::config::SecuritySettings;
use crate::modules::user;
use sqlx::MySqlPool;
use std::sync::Arc;
use yang_base::router::AppRouter;
use yang_base::BaseError;

pub fn build_app_router(
    pool: Arc<MySqlPool>,
    security: Arc<SecuritySettings>,
) -> Result<AppRouter, BaseError> {
    AppRouter::new().module(user::build_module(pool, security)?)
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
            .map(|action| {
                (
                    action.route.operation_id.as_str(),
                    action.route.method.as_str(),
                    action.route.path.as_str(),
                    action.route.success_status,
                    action.is_public,
                )
            })
            .collect();

        assert_eq!(router.table_definitions().len(), 1);
        assert_eq!(router.table_definitions()[0].name(), "users");
        assert_eq!(catalog.modules.len(), 1);
        assert_eq!(catalog.modules[0].name, "user");
        assert_eq!(operations.len(), 5);
        assert!(operations.contains(&(
            "users.register",
            "POST",
            "/api/v1/users/register",
            201,
            true,
        )));
        assert!(operations.contains(&("users.login", "POST", "/api/v1/users/login", 200, true,)));
        assert!(operations.contains(&(
            "users.refresh",
            "POST",
            "/api/v1/users/refresh",
            200,
            true,
        )));
        assert!(operations.contains(&("users.logout", "POST", "/api/v1/users/logout", 200, true,)));
        assert!(operations.contains(&("users.me", "GET", "/api/v1/users/me", 200, false,)));
    }
}

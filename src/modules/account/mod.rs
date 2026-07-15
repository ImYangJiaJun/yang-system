//! 账号与认证模块。

mod actions;
mod dto;
mod entity;
mod password;
mod repository;
mod service;

use self::actions::{AccountClaimsResolver, AccountVerifier, MeAction, RegisterAction};
use self::entity::account_table_config;
use self::repository::AccountRepository;
use self::service::AccountService;
use crate::config::SecuritySettings;
use sqlx::MySqlPool;
use std::sync::Arc;
use yang_base::action::auth::{LoginAction, LogoutAction, RefreshAction};
use yang_base::action::{TokenAuthMiddleware, User};
use yang_base::router::{ModuleRouter, RouteDescriptor};
use yang_base::token::TokenClaims;
use yang_base::BaseError;

pub struct AccountModules {
    pub authentication: ModuleRouter,
    pub account: ModuleRouter,
}

pub fn build_modules(
    pool: Arc<MySqlPool>,
    security: Arc<SecuritySettings>,
) -> Result<AccountModules, BaseError> {
    let table = account_table_config()?;
    let repository = AccountRepository::new(pool, Arc::clone(&table));
    let service = Arc::new(AccountService::new(repository, security));

    let authentication = ModuleRouter::new("account_auth", "账号认证")
        .register_action(RegisterAction::new(Arc::clone(&service)))?
        .register_action(LoginAction::new(AccountVerifier::new(Arc::clone(&service))))?
        .register_action(RefreshAction::new(AccountClaimsResolver::new(Arc::clone(
            &service,
        ))))?
        .register_action(LogoutAction::new())?
        .register_route(
            "register",
            route(
                "POST",
                "/api/v1/accounts/register",
                "accounts.register",
                201,
            )?,
        )?
        .register_route(
            "login",
            route("POST", "/api/v1/accounts/login", "accounts.login", 200)?,
        )?
        .register_route(
            "refresh",
            route("POST", "/api/v1/accounts/refresh", "accounts.refresh", 200)?,
        )?
        .register_route(
            "logout",
            route("POST", "/api/v1/accounts/logout", "accounts.logout", 200)?,
        )?;

    let account = ModuleRouter::new("account", "账号")
        .with_table_config(table)
        .middleware(TokenAuthMiddleware::new(user_from_claims))
        .register_action(MeAction::new(service))?
        .register_route(
            "me",
            route("GET", "/api/v1/accounts/me", "accounts.me", 200)?,
        )?;

    Ok(AccountModules {
        authentication,
        account,
    })
}

fn route(
    method: &str,
    path: &str,
    operation_id: &str,
    success_status: u16,
) -> Result<RouteDescriptor, BaseError> {
    RouteDescriptor::new(method, path, operation_id)?
        .with_success_status(success_status)?
        .with_tags(vec!["accounts".to_string()])
}

fn user_from_claims(claims: &TokenClaims) -> User {
    let id = claims.sub.parse::<i64>().unwrap_or_default();
    let username = claims
        .custom
        .get("username")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&claims.sub)
        .to_string();
    let roles = claims
        .custom
        .get("roles")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string);
    User::new(id, username).with_roles(roles)
}

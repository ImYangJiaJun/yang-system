//! 权限 Addon（addon 层）：权限目录与授权存储基础设施（决策 D1/D3/D4）。
//!
//! 两层结构：`grants/` 是 module 层（表与授权管理 Action）；
//! `domain/` 是 addon 层共享机制，账号域 Token 签发所需的直授权限
//! 经 [`AuthzGrantResolver`] 单一出口提供。

pub(crate) mod domain;
mod grants;

use crate::addon::account::GrantResolver;
use crate::authorization::{AuthorizationVersionValidator, StepUpServices};
use std::sync::Arc;
use yang_base::definition::AddonSpec;
use yang_base::BaseError;

pub(crate) use domain::permission_catalog::{project_permissions, PermissionCatalogHandle};
pub(crate) use domain::resolver::AuthzGrantResolver;

/// access Addon 的装配产物：Addon 定义 + 账号域授权快照扩展端口。
pub(crate) struct AccessAddon {
    spec: AddonSpec,
    grant_resolver: Arc<AuthzGrantResolver>,
}

impl AccessAddon {
    /// 账号域在 Token 签发时合并直授权限的解析器。
    pub(crate) fn grant_resolver(&self) -> Arc<dyn GrantResolver> {
        Arc::clone(&self.grant_resolver) as Arc<dyn GrantResolver>
    }

    /// 取出 Addon 定义交给 AppBuilder。
    pub(crate) fn into_spec(self) -> AddonSpec {
        self.spec
    }
}

/// 构建权限 Addon。
///
/// Addon 边界负责声明产品能力及其 Module；应用层不应直接拼装 `access.grants`。
pub(crate) fn build_addon(
    authorization_validator: AuthorizationVersionValidator,
    step_up: Option<StepUpServices>,
    permission_catalog: PermissionCatalogHandle,
) -> Result<AccessAddon, BaseError> {
    let (module, access) =
        grants::build_module(authorization_validator, step_up, permission_catalog)?;
    Ok(AccessAddon {
        spec: AddonSpec::new(yang_base::addon!("access")).module(module),
        grant_resolver: Arc::new(AuthzGrantResolver::new(access)),
    })
}

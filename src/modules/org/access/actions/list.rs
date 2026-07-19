//! 当前用户可访问的租户列表。

use super::super::service::{TenantPage, TenantService};
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{Int, ModuleSpec};
use yang_base::{Action, BaseError};

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) TenantListInput {
        #[param(source = query)]
        page: Int::new().title("页码"),
        #[param(source = query)]
        limit: Int::new().title("每页数量"),
    }
}

#[derive(Action)]
#[action(
    name = "list",
    display_name = "我的企业",
    description = "在选择租户前返回当前用户可访问的企业",
    method = "GET",
    path = "/api/v1/tenants"
)]
struct TenantListAction {
    service: Arc<TenantService>,
}

#[async_trait]
impl ActionHandler for TenantListAction {
    type Input = TenantListInput;
    type Output = TenantPage;

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        self.service.list(&ctx, input.page, input.limit).await
    }
}

pub(super) fn register(
    module: ModuleSpec,
    service: Arc<TenantService>,
) -> Result<ModuleSpec, BaseError> {
    Ok(module.native_action(TenantListAction { service }))
}

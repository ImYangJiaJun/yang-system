//! 创建企业并建立初始成员关系。

use super::super::service::{TenantService, TenantSummary};
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{ModuleSpec, Str};
use yang_base::{Action, BaseError};

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) TenantCreateInput {
        name: Str::new().title("企业名称").require(true).min_length(1).max_length(100),
        code: Str::new().title("企业编号").require(true).min_length(2).max_length(32),
    }
}

#[derive(Action)]
#[action(
    name = "create",
    display_name = "创建企业",
    description = "原子创建企业与当前用户的初始成员关系",
    method = "POST",
    path = "/api/v1/tenants",
    success_status = 201
)]
struct TenantCreateAction {
    service: Arc<TenantService>,
}

#[async_trait]
impl ActionHandler for TenantCreateAction {
    type Input = TenantCreateInput;
    type Output = TenantSummary;

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        self.service.create(&ctx, &input.name, &input.code).await
    }
}

pub(super) fn register(
    module: ModuleSpec,
    service: Arc<TenantService>,
) -> Result<ModuleSpec, BaseError> {
    Ok(module.native_action(TenantCreateAction { service }))
}

//! 一次性初始化首个平台超级管理员。

use super::super::service::{AdminService, BootstrapResult};
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{ModuleSpec, Str};
use yang_base::{Action, BaseError};

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) BootstrapInput {
        name: Str::new().title("姓名").require(true).min_length(1).max_length(50),
        position: Str::new().title("职务").max_length(50),
    }
}

#[derive(Action)]
#[action(
    name = "bootstrap",
    display_name = "初始化平台管理员",
    description = "由已登录用户一次性创建首个平台超级管理员，成功后需要刷新 Token",
    method = "POST",
    path = "/api/v1/admin/bootstrap",
    success_status = 201
)]
struct BootstrapAction {
    service: Arc<AdminService>,
}

#[async_trait]
impl ActionHandler for BootstrapAction {
    type Input = BootstrapInput;
    type Output = BootstrapResult;

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        self.service
            .bootstrap(&ctx, &input.name, input.position.as_deref())
            .await
    }
}

pub(super) fn register(module: ModuleSpec, service: Arc<AdminService>) -> ModuleSpec {
    module.native_action(BootstrapAction { service })
}

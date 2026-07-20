//! 启用或停用平台账号。

use super::super::model::AdminAccountView;
use super::super::service::AdminService;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{Int, ModuleSpec, Radio};
use yang_base::{Action, BaseError};

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) SetStatusInput {
        id: Int::new().title("平台账号 ID").require(true),
        status: Radio::<String>::new()
            .title("状态")
            .require(true)
            .options([("active", "启用"), ("disabled", "停用")]),
    }
}

#[derive(Action)]
#[action(
    name = "set_status",
    display_name = "设置平台账号状态",
    description = "启用或停用平台账号，并保护最后一个启用中的超级管理员",
    method = "PUT",
    path = "/api/v1/admin/users/status",
    permissions("admin.user:write")
)]
struct SetStatusAction {
    service: Arc<AdminService>,
}

#[async_trait]
impl ActionHandler for SetStatusAction {
    type Input = SetStatusInput;
    type Output = AdminAccountView;

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        self.service.set_status(&ctx, input.id, &input.status).await
    }
}

pub(super) fn register(module: ModuleSpec, service: Arc<AdminService>) -> ModuleSpec {
    module.native_action(SetStatusAction { service })
}

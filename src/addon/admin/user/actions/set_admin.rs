//! 设置平台超级管理员身份。

use super::super::model::AdminAccountView;
use super::super::service::AdminService;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{Int, ModuleSpec, Switch};
use yang_base::{Action, BaseError};

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) SetAdminInput {
        id: Int::new().title("平台账号 ID").require(true),
        admin: Switch::new().title("超级管理员").require(true),
    }
}

#[derive(Action)]
#[action(
    name = "set_admin",
    display_name = "设置超级管理员",
    description = "授予或撤销超级管理员身份，并保护最后一个启用中的超级管理员",
    method = "PUT",
    path = "/api/v1/admin/users/admin",
    permissions("admin.user:write")
)]
struct SetAdminAction {
    service: Arc<AdminService>,
}

#[async_trait]
impl ActionHandler for SetAdminAction {
    type Input = SetAdminInput;
    type Output = AdminAccountView;

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        self.service.set_admin(&ctx, input.id, input.admin).await
    }
}

pub(super) fn register(module: ModuleSpec, service: Arc<AdminService>) -> ModuleSpec {
    module.native_action(SetAdminAction { service })
}

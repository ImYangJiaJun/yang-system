//! 将现有基础用户绑定为平台账号。

use super::super::model::AdminAccountView;
use super::super::service::AdminService;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{Int, ModuleSpec, Str, Switch};
use yang_base::{Action, BaseError};

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) AdminAddInput {
        user_user: Int::new().title("用户 ID").require(true),
        name: Str::new().title("姓名").require(true).min_length(1).max_length(50),
        position: Str::new().title("职务").max_length(50),
        admin: Switch::new().title("超级管理员"),
    }
}

#[derive(Action)]
#[action(
    name = "add",
    display_name = "添加平台账号",
    description = "将现有启用用户绑定为平台账号",
    method = "POST",
    path = "/api/v1/admin/users",
    permissions("admin.user:write"),
    success_status = 201
)]
struct AdminAddAction {
    service: Arc<AdminService>,
}

#[async_trait]
impl ActionHandler for AdminAddAction {
    type Input = AdminAddInput;
    type Output = AdminAccountView;

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        self.service
            .add(
                &ctx,
                input.user_user,
                &input.name,
                input.position.as_deref(),
                input.admin.unwrap_or(false),
            )
            .await
    }
}

pub(super) fn register(module: ModuleSpec, service: Arc<AdminService>) -> ModuleSpec {
    module.native_action(AdminAddAction { service })
}

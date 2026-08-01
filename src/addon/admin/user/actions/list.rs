//! 分页查询平台账号。

use super::super::model::AdminAccountPage;
use super::super::service::AdminService;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{Int, ModuleSpec, Str};
use yang_base::{Action, BaseError};

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) AdminListInput {
        #[param(source = query)]
        page: Int::new().title("页码"),
        #[param(source = query)]
        limit: Int::new().title("每页数量"),
        #[param(source = query)]
        search: Str::new().title("搜索词").max_length(100),
    }
}

#[derive(Action)]
#[action(
    name = "list",
    display_name = "平台账号列表",
    description = "分页查询平台账号及其基础用户身份",
    method = "GET",
    path = "/api/v1/admin/users",
    permissions("admin.user:read")
)]
struct AdminListAction {
    service: Arc<AdminService>,
}

#[async_trait]
impl ActionHandler for AdminListAction {
    type Input = AdminListInput;
    type Output = AdminAccountPage;

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        self.service
            .list(&ctx, input.page, input.limit, input.search.as_deref())
            .await
    }
}

pub(super) fn register(module: ModuleSpec, service: Arc<AdminService>) -> ModuleSpec {
    module.native_action(AdminListAction { service })
}

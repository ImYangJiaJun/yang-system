//! 分页查询平台账号。

use super::super::domain::{AdminAccountPage, AdminService};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{Int, Str};
use yang_base::BaseError;

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

pub(super) async fn handle(
    ctx: ActionContext,
    input: AdminListInput,
    service: Arc<AdminService>,
) -> Result<AdminAccountPage, BaseError> {
    service
        .list(&ctx, input.page, input.limit, input.search.as_deref())
        .await
}

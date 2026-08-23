//! 设置平台超级管理员身份。

use super::super::domain::{AdminAccountView, AdminService};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{Int, Switch};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) SetAdminInput {
        id: Int::new().title("平台账号 ID").require(true),
        admin: Switch::new().title("超级管理员").require(true),
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: SetAdminInput,
    service: Arc<AdminService>,
) -> Result<AdminAccountView, BaseError> {
    service.set_admin(&ctx, input.id, input.admin).await
}

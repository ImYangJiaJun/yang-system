//! 启用或停用平台账号。

use super::super::domain::{AdminAccountView, AdminService};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{Int, Radio};
use yang_base::BaseError;

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

pub(super) async fn handle(
    ctx: ActionContext,
    input: SetStatusInput,
    service: Arc<AdminService>,
) -> Result<AdminAccountView, BaseError> {
    service.set_status(&ctx, input.id, &input.status).await
}

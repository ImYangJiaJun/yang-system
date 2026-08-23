//! 将现有基础用户绑定为平台账号。

use super::super::domain::{AdminAccountView, AdminService};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{Int, Str, Switch};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) AdminAddInput {
        user_user: Int::new().title("用户 ID").require(true),
        name: Str::new().title("姓名").require(true).min_length(1).max_length(50),
        position: Str::new().title("职务").max_length(50),
        admin: Switch::new().title("超级管理员"),
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: AdminAddInput,
    service: Arc<AdminService>,
) -> Result<AdminAccountView, BaseError> {
    service
        .add(
            &ctx,
            input.user_user,
            &input.name,
            input.position.as_deref(),
            input.admin.unwrap_or(false),
        )
        .await
}

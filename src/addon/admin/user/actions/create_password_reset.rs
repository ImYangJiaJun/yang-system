//! 管理员创建一次性密码重置凭证；不生成或返回临时密码。

use super::super::domain::{AdminService, PasswordResetCreated};
use std::sync::Arc;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::Int;
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) CreatePasswordResetInput {
        user_id: Int::new().title("目标用户 ID").require(true),
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: CreatePasswordResetInput,
    service: Arc<AdminService>,
) -> Result<ApiResponse, BaseError> {
    let created = service.create_password_reset(&ctx, input.user_id).await?;
    no_store_response(created)
}

fn no_store_response(created: PasswordResetCreated) -> Result<ApiResponse, BaseError> {
    ApiResponse::success(created, "密码重置凭证已创建，仅显示一次")?
        .with_header("cache-control", "no-store")?
        .with_header("pragma", "no-cache")
}

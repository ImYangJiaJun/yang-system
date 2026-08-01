//! 管理员创建一次性密码重置凭证；不生成或返回临时密码。

use super::super::service::{AdminService, PasswordResetCreated};
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::{Action as ActionHandler, ActionContext, ApiResponse};
use yang_base::definition::{Int, ModuleSpec};
use yang_base::{Action, BaseError};

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) CreatePasswordResetInput {
        user_id: Int::new().title("目标用户 ID").require(true),
    }
}

#[derive(Action)]
#[action(
    name = "create_password_reset",
    display_name = "创建密码重置凭证",
    description = "为目标用户创建短期单次消费凭证，响应只返回一次原始凭证",
    method = "POST",
    path = "/api/v1/admin/users/password-reset",
    permissions("admin.user:write"),
    success_status = 201
)]
struct CreatePasswordResetAction {
    service: Arc<AdminService>,
}

#[async_trait]
impl ActionHandler for CreatePasswordResetAction {
    type Input = CreatePasswordResetInput;
    type Output = ApiResponse;

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let created = self
            .service
            .create_password_reset(&ctx, input.user_id)
            .await?;
        no_store_response(created)
    }
}

fn no_store_response(created: PasswordResetCreated) -> Result<ApiResponse, BaseError> {
    ApiResponse::success(created, "密码重置凭证已创建，仅显示一次")?
        .with_header("cache-control", "no-store")?
        .with_header("pragma", "no-cache")
}

pub(super) fn register(module: ModuleSpec, service: Arc<AdminService>) -> ModuleSpec {
    module.native_action(CreatePasswordResetAction { service })
}

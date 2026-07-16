use super::{UserService, UserView};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;
use yang_base::action::{ActionContext, TypedHandler};
use yang_base::router::Api;
use yang_base::{Action, BaseError};

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}

#[derive(Action)]
#[action(
    name = "me",
    display_name = "当前用户",
    description = "读取当前已认证用户"
)]
struct MeAction {
    service: Arc<UserService>,
}

#[async_trait]
impl TypedHandler for MeAction {
    type Input = EmptyInput;
    type Output = UserView;

    async fn handle(
        &self,
        ctx: ActionContext,
        _input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let id = ctx
            .authenticated_user()
            .ok_or_else(|| BaseError::Unauthorized("需要登录".to_string()))?
            .id;
        self.service.view_by_id(id).await
    }
}

pub(super) fn api(service: Arc<UserService>) -> Api {
    Api::get("/api/v1/users/me", MeAction { service })
        .operation_id("users.me")
        .tag("users")
}

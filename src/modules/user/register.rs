use super::{UserService, UserView};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;
use yang_base::action::{ActionContext, TypedHandler};
use yang_base::router::Api;
use yang_base::{Action, BaseError};

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RegisterInput {
    username: String,
    password: String,
}

impl std::fmt::Debug for RegisterInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisterInput")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

#[derive(Action)]
#[action(
    name = "register",
    display_name = "注册用户",
    description = "创建一个新用户",
    public
)]
struct RegisterAction {
    service: Arc<UserService>,
}

#[async_trait]
impl TypedHandler for RegisterAction {
    type Input = RegisterInput;
    type Output = UserView;

    async fn handle(
        &self,
        _ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        self.service
            .register(&input.username, &input.password)
            .await
    }
}

pub(super) fn api(service: Arc<UserService>) -> Api {
    Api::post("/api/v1/users/register", RegisterAction { service })
        .operation_id("users.register")
        .created()
        .tag("users")
}

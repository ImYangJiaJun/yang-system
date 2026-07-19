use super::super::schema::UserView;
use super::super::service::UserService;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;
use yang_base::action::{ActionContext, TypedHandler};
use yang_base::definition::{ActionName, ActionSpec, HttpMethod, ModuleSpec, RouteSpec};
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

impl UserService {
    async fn view_by_id(&self, ctx: &ActionContext, id: i64) -> Result<UserView, BaseError> {
        let user = self
            .find_by_id(ctx, id)
            .await?
            .ok_or_else(|| BaseError::UserNotFound(id.to_string()))?;
        self.ensure_active(&user)?;
        UserView::try_from(&user)
    }
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
        self.service.view_by_id(&ctx, id).await
    }
}

pub(super) fn register(
    module: ModuleSpec,
    service: Arc<UserService>,
) -> Result<ModuleSpec, BaseError> {
    let name = ActionName::new("me").map_err(|error| BaseError::ConfigError(error.to_string()))?;
    let spec = ActionSpec::new(
        name,
        RouteSpec::new(HttpMethod::Get, "/api/v1/users/me", "users.me"),
    )
    .display_name("当前用户")
    .description("读取当前已认证用户")
    .tag("users");
    Ok(module.action(spec, MeAction { service }))
}

#[cfg(test)]
mod tests {
    use super::super::super::schema::{
        CREATED_AT, PASSWORD_HASH, STATUS, UPDATED_AT, USERNAME, USER_ID,
    };
    use super::*;
    use yang_base::table::Record;

    #[test]
    fn user_view_does_not_serialize_password_hash_from_record() {
        let record = Record::new()
            .set(USER_ID, 7)
            .set(USERNAME, "alice")
            .set(PASSWORD_HASH, "secret-hash")
            .set(STATUS, "active")
            .set(CREATED_AT, 10)
            .set(UPDATED_AT, 11);

        let view = UserView::try_from(&record)
            .unwrap_or_else(|error| panic!("完整记录应转换为用户视图: {error}"));
        let value = serde_json::to_value(view)
            .unwrap_or_else(|error| panic!("用户视图应可序列化: {error}"));

        assert_eq!(value.get(USERNAME), Some(&serde_json::json!("alice")));
        assert!(value.get(PASSWORD_HASH).is_none());
    }

    #[test]
    fn user_view_rejects_incomplete_or_invalid_record() {
        let incomplete = Record::new().set(USER_ID, 7);
        assert!(UserView::try_from(&incomplete).is_err());

        let invalid = Record::new()
            .set(USER_ID, "not-an-integer")
            .set(USERNAME, "alice")
            .set(STATUS, "active")
            .set(CREATED_AT, 10)
            .set(UPDATED_AT, 11);
        assert!(UserView::try_from(&invalid).is_err());
    }
}

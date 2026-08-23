//! 读取当前已认证用户。

use super::super::domain::schema::UserView;
use super::super::domain::service::UserService;
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{ParamInput, Params};
use yang_base::BaseError;

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct EmptyInput {}

impl ParamInput for EmptyInput {
    fn params() -> Params {
        Params::new()
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    _input: EmptyInput,
    service: Arc<UserService>,
) -> Result<UserView, BaseError> {
    let id = ctx
        .authenticated_user()
        .ok_or_else(|| BaseError::Unauthorized("需要登录".to_string()))?
        .id;
    service.view_by_id(&ctx, id).await
}

#[cfg(test)]
mod tests {
    use super::super::super::domain::schema::{
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

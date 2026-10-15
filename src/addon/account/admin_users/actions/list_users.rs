//! 管理端分页列出用户（需权限，路线图 D-3）。

use crate::addon::account::user::table::UserView;
use crate::addon::account::Account;
use schemars::JsonSchema;
use serde::Serialize;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, Int, ModuleSpec};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) ListUsersInput {
        #[param(source = query)]
        page: Int::new()
            .title("页码")
            .require(false)
            .default(1_i64),
        #[param(source = query)]
        page_size: Int::new()
            .title("每页条数")
            .require(false)
            .default(20_i64),
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(super) struct UsersPage {
    pub(crate) users: Vec<UserView>,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: ListUsersInput,
    account: Arc<Account>,
) -> Result<UsersPage, BaseError> {
    let page = usize::try_from(input.page.unwrap_or(1))
        .map_err(|_| BaseError::ParamInvalid("page".to_string(), "页码无效".to_string()))?;
    let page_size = usize::try_from(input.page_size.unwrap_or(20)).map_err(|_| {
        BaseError::ParamInvalid("page_size".to_string(), "每页条数无效".to_string())
    })?;
    if page_size > 100 {
        return Err(BaseError::ParamInvalid(
            "page_size".to_string(),
            "每页条数不能超过 100".to_string(),
        ));
    }
    let records = account.users().list_page(&ctx, page, page_size).await?;
    let users = records
        .iter()
        .map(UserView::try_from)
        .collect::<Result<Vec<_>, BaseError>>()?;

    Ok(UsersPage { users })
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("list_users"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&account))
        })
        .route(HttpMethod::Get, "/api/v1/users")
        .display_name("用户列表")
        .description("管理端分页列出用户（邮箱字段遵循 system 角色可见性）")
        .permissions(["account.users.read"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_has_bounded_pagination_only() {
        let params = <ListUsersInput as ParamInput>::params();
        let names = params
            .as_slice()
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["page", "page_size"]);
        // GET 无请求体：分页参数必须声明为 query 来源，否则 `?page=N`
        // 会被静默忽略（回归守护）。
        assert!(params
            .as_slice()
            .iter()
            .all(|param| param.source == yang_base::definition::ParamSource::Query));
        let injected = serde_json::from_value::<ListUsersInput>(serde_json::json!({
            "user_id": 99
        }));
        assert!(injected.is_err());
    }

    #[test]
    fn user_view_reuses_me_projection_contract() {
        // UserView 与 me 共用投影：不包含密码/版本字段。
        let record = yang_base::table::Record::new()
            .set(crate::addon::account::user::table::USER_ID, 7)
            .set(crate::addon::account::user::table::USERNAME, "alice")
            .set(
                crate::addon::account::user::table::EMAIL,
                "alice@example.com",
            )
            .set(crate::addon::account::user::table::EMAIL_VERIFIED_AT, 1000)
            .set(crate::addon::account::user::table::STATUS, "active")
            .set(crate::addon::account::user::table::CREATED_AT, 10)
            .set(crate::addon::account::user::table::UPDATED_AT, 11);
        let view = UserView::try_from(&record)
            .unwrap_or_else(|error| panic!("完整记录应转换为用户视图: {error}"));
        let value = serde_json::to_value(view)
            .unwrap_or_else(|error| panic!("用户视图应可序列化: {error}"));
        assert!(value.get("password_hash").is_none());
        assert!(value.get("authz_version").is_none());
    }
}

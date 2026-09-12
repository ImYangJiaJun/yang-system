//! 查询当前用户登录安全事件（分页、仅本人、按时间倒序，路线图 C-2b）。

use crate::addon::account::domain::login_event::LoginEventView;
use crate::addon::account::Account;
use schemars::JsonSchema;
use serde::Serialize;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, Int, ModuleSpec};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) SecurityEventsInput {
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

/// 分页的安全事件列表。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(super) struct SecurityEventsPage {
    pub(crate) events: Vec<LoginEventView>,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: SecurityEventsInput,
    account: Arc<Account>,
) -> Result<SecurityEventsPage, BaseError> {
    let user_id = ctx
        .authenticated_user()
        .ok_or_else(|| BaseError::Unauthorized("需要登录".to_string()))?
        .id;
    // 只允许查询本人事件；分页上限由输入约束保证。
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
    let events = account
        .login_events()
        .list_for_user(&ctx, user_id, page, page_size)
        .await?;
    Ok(SecurityEventsPage { events })
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("security_events"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(HttpMethod::Get, "/api/v1/users/security-events")
        .display_name("安全事件")
        .description("查询当前用户的登录安全事件（分页、仅本人）")
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_has_bounded_pagination() {
        let params = <SecurityEventsInput as ParamInput>::params();
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
        let injected = serde_json::from_value::<SecurityEventsInput>(serde_json::json!({
            "user_id": 99
        }));
        assert!(injected.is_err());
    }
}

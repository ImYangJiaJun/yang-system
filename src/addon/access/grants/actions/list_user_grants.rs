//! 查询目标用户的全部直授权限（审计视图：含过期行，带 expired 派生标记）。

use crate::addon::access::domain::context::Access;
use crate::addon::access::domain::repository::{current_unix_timestamp, is_expired, GrantRecord};
use schemars::JsonSchema;
use serde::Serialize;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, Key, ModuleSpec};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) ListUserGrantsInput {
        #[param(source = path)]
        user_id: Key::new()
            .title("目标用户")
            .require(true),
    }
}

/// 单条直授权限的对外视图（审计视图，过期行也展示）。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct GrantView {
    id: i64,
    permission: String,
    granted_by: i64,
    occurred_at: i64,
    /// Unix 秒；`None` = 永久有效。
    expires_at: Option<i64>,
    /// 派生标记：是否已过期（`expires_at <= now`）。过期后权限在解析侧失效，
    /// 行仍保留做审计；前端据此展示「已过期」并允许重新授予（走续期）。
    expired: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ListUserGrantsResult {
    user_id: i64,
    grants: Vec<GrantView>,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: ListUserGrantsInput,
    access: Arc<Access>,
) -> Result<ListUserGrantsResult, BaseError> {
    if input.user_id <= 0 {
        return Err(BaseError::ParamInvalid(
            "user_id".to_string(),
            "目标用户必须是正整数".to_string(),
        ));
    }
    // 目标用户必须存在；停用用户的授权事实仍允许查询（审计需要）。
    if access
        .authorization()
        .find_authorization_version(ctx.tools().mysql()?.pool(), input.user_id)
        .await?
        .is_none()
    {
        return Err(BaseError::UserNotFound(input.user_id.to_string()));
    }

    let mut transaction = ctx
        .tools()
        .mysql()?
        .read_only_transaction()
        .await
        .map_err(BaseError::from)?;
    // 审计视图读取全部行（含过期行）：与解析侧（list_by_user_in_tx 的 SQL 过滤）
    // 相反，这里要连过期行一起展示，expired 派生标记由展示层按同一时钟计算。
    let result = access
        .grants()
        .list_all_by_user_in_tx(&ctx, &mut transaction, input.user_id)
        .await;
    let records = match result {
        Ok(records) => {
            transaction.commit().await.map_err(BaseError::from)?;
            records
        }
        Err(error) => {
            if let Err(rollback_error) = transaction.rollback().await {
                tracing::error!(error = %rollback_error, "查询用户授权事务回滚失败");
            }
            return Err(error);
        }
    };

    let now = current_unix_timestamp()?;
    Ok(ListUserGrantsResult {
        user_id: input.user_id,
        grants: records
            .into_iter()
            .map(|record| grant_view(record, now))
            .collect(),
    })
}

/// 把一条直授事实投影为对外视图（含 `expired` 派生标记，前端展示用）。
fn grant_view(record: GrantRecord, now: i64) -> GrantView {
    GrantView {
        id: record.id,
        permission: record.permission,
        granted_by: record.granted_by,
        occurred_at: record.occurred_at,
        expires_at: record.expires_at,
        expired: is_expired(record.expires_at, now),
    }
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("list_user_grants"),
            move |ctx, input| handle(ctx, input, Arc::clone(&access)),
        )
        .route(HttpMethod::Get, "/api/v1/access/users/{user_id}/grants")
        .display_name("用户授权列表")
        .description("查询目标用户的全部直授权限")
        .permissions(["access.grants.read"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::{ParamInput, ParamSource};

    #[test]
    fn user_id_comes_from_the_path_not_the_body() {
        let params = ListUserGrantsInput::params();
        let user_id = params
            .as_slice()
            .iter()
            .find(|param| param.name.as_str() == "user_id")
            .unwrap_or_else(|| panic!("应声明 user_id 参数"));
        assert_eq!(user_id.source, ParamSource::Path);
        assert!(user_id.required);

        let injected = serde_json::from_value::<ListUserGrantsInput>(serde_json::json!({
            "user_id": 7,
            "permission": "access.grants.write"
        }));
        assert!(injected.is_err(), "客户端不能注入过滤字段");
    }

    #[test]
    fn view_flags_past_future_and_permanent_grants() {
        let now = 1_700_000_000;
        let record = |expires_at: Option<i64>| GrantRecord {
            id: 1,
            user_id: 7,
            permission: "access.grants.read".to_string(),
            granted_by: 9,
            occurred_at: 1_690_000_000,
            expires_at,
        };

        let expired = grant_view(record(Some(now - 1)), now);
        assert!(expired.expired, "过去时刻必须标记为已过期");
        assert_eq!(expired.expires_at, Some(now - 1));

        let active = grant_view(record(Some(now + 1)), now);
        assert!(!active.expired, "未来时刻未过期");
        assert_eq!(active.expires_at, Some(now + 1));

        let permanent = grant_view(record(None), now);
        assert!(!permanent.expired, "NULL=永久，永不过期");
        assert_eq!(permanent.expires_at, None);
    }
}

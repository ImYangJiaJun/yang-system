//! 按 session_id 撤销单个会话（需登录 + Step-up，路线图 C-1d）。

use crate::addon::account::Account;
use crate::audit;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) RevokeSessionInput {
        session_id: Str::new()
            .title("会话 ID")
            .require(true)
            .max_length(64),
    }
}

/// jti 黑名单保留时长（秒）：保守覆盖 refresh token 完整有效期，
/// 被踢设备的旧 refresh 在窗口内轮换会被 `verify_token_checked` 拒绝。
const REVOKE_JTI_BLACKLIST_TTL_SECONDS: u64 = 7 * 24 * 3600;

pub(super) async fn handle(
    ctx: ActionContext,
    input: RevokeSessionInput,
    account: Arc<Account>,
) -> Result<ApiResponse, BaseError> {
    let user_id = ctx
        .authenticated_user()
        .ok_or_else(|| BaseError::Unauthorized("需要登录".to_string()))?
        .id;
    // 目标会话必须属于当前用户（横向越权防护）。
    let Some((owner_id, current_jti)) = account
        .sessions()
        .current_jti(&ctx, &input.session_id)
        .await?
    else {
        return Err(BaseError::Unauthorized("会话不存在或已撤销".to_string()));
    };
    if owner_id != user_id {
        return Err(BaseError::Unauthorized("无权撤销该会话".to_string()));
    }

    let now = current_unix_timestamp()?;
    // 行标记撤销（幂等）。
    let affected = account
        .sessions()
        .revoke(&ctx, &input.session_id, now)
        .await?;
    if affected != 1 {
        return Err(BaseError::Unauthorized("会话不存在或已撤销".to_string()));
    }
    // jti 黑名单：使被踢设备的旧 refresh 立即失效（不递增凭据版本，
    // 不影响该用户其他会话）。
    ctx.tools()
        .token()?
        .revoke_by_jti_with_ttl(&current_jti, REVOKE_JTI_BLACKLIST_TTL_SECONDS)
        .await?;

    let event = audit::succeeded_event(
        &ctx,
        None,
        Some(audit::entity("user", user_id)?),
        audit::entity("user_session", input.session_id.clone())?,
        None,
        Some(audit::summary([
            ("session_id", json!(input.session_id)),
            ("revoked", json!(true)),
        ])?),
    )?;
    audit::append_independent(ctx.tools().mysql()?.pool(), &event).await?;

    ApiResponse::success(
        json!({ "session_revoked": true }),
        "该设备已退出登录",
    )
}

fn current_unix_timestamp() -> Result<i64, BaseError> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| BaseError::ConfigError("系统时间早于 Unix epoch".to_string()))?
        .as_secs();
    i64::try_from(seconds).map_err(|_| BaseError::ConfigError("系统时间超出 i64 范围".to_string()))
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("revoke_session"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(HttpMethod::Post, "/api/v1/users/sessions/revoke")
        .display_name("撤销会话")
        .description("按 session_id 撤销单个登录设备")
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_requires_session_id() {
        let params = <RevokeSessionInput as ParamInput>::params();
        let names = params
            .as_slice()
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["session_id"]);
        assert!(params.as_slice().iter().all(|param| param.required));
        let injected = serde_json::from_value::<RevokeSessionInput>(serde_json::json!({
            "session_id": "s-1",
            "user_id": 99
        }));
        assert!(injected.is_err());
    }
}

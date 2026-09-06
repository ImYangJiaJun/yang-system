//! 轮换 Refresh Token 并签发新 Token 对。

use crate::addon::account::Account;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::auth::{
    BrowserSession, RefreshAction, RefreshClaimsResolver, RefreshInput, TokenPairClaims,
};
use yang_base::action::{ActionContext, ApiResponse, TypedHandler};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::token::{TokenClaims, TokenType};
use yang_base::BaseError;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct BrowserRefreshInput {}

impl ParamInput for BrowserRefreshInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 把账号授权快照接到框架内置 `RefreshAction` 的声明解析端口。
#[derive(Clone)]
struct UserClaimsResolver {
    account: Arc<Account>,
}

#[async_trait]
impl RefreshClaimsResolver for UserClaimsResolver {
    async fn resolve(
        &self,
        ctx: &ActionContext,
        subject: &str,
    ) -> Result<serde_json::Value, BaseError> {
        Ok(self.account.claims_for_subject(ctx, subject).await?.access)
    }

    async fn resolve_pair(
        &self,
        ctx: &ActionContext,
        subject: &str,
    ) -> Result<TokenPairClaims, BaseError> {
        self.account.claims_for_subject(ctx, subject).await
    }

    async fn resolve_pair_from_claims(
        &self,
        ctx: &ActionContext,
        old_claims: &TokenClaims,
    ) -> Result<TokenPairClaims, BaseError> {
        self.account.claims_for_refresh(ctx, old_claims).await
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    _input: BrowserRefreshInput,
    account: Arc<Account>,
) -> Result<ApiResponse, BaseError> {
    let secure = BrowserSession::validate_same_origin(&ctx.request)?;
    let refresh_token = Account::browser_session().refresh_token(&ctx.request)?;
    let touch_ctx = ctx.clone();
    let tokens = RefreshAction::new(UserClaimsResolver {
        account: Arc::clone(&account),
    })
    .handle(ctx, RefreshInput { refresh_token })
    .await?;
    // refresh 热路径：节流更新会话行（last_seen_at 60s 窗口内不重复写，
    // jti 随轮换更新）；失败只记日志，不阻塞 refresh。
    if let Err(error) = touch_session_on_refresh(&touch_ctx, &account, &tokens.access_token).await {
        tracing::warn!(error = %error, "refresh 会话行更新失败");
    }
    Account::browser_session().token_response(tokens.access_token, tokens.refresh_token, secure)
}

/// 从新 access token 提取 session_id/jti 并节流更新 `user_session` 行。
async fn touch_session_on_refresh(
    ctx: &ActionContext,
    account: &Account,
    access_token: &str,
) -> Result<(), BaseError> {
    let claims = ctx.tools().token()?.verify_token(access_token)?;
    if claims.token_type != TokenType::Access {
        return Ok(());
    }
    let Some(session_id) = claims
        .custom
        .get("session_id")
        .and_then(|value| value.as_str())
    else {
        return Ok(()); // 老 Token 无 session_id：按无会话记录降级
    };
    let now = current_unix_timestamp()?;
    account
        .sessions()
        .touch_on_refresh(ctx, session_id, &claims.jti, now)
        .await
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
        .action_fn(yang_base::action_name!("refresh"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&account))
        })
        .route(HttpMethod::Post, "/api/v1/users/refresh")
        .display_name("刷新 Token")
        .description("轮换 Refresh Token 并签发新 Token 对")
        .public()
        .register()
}

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
    let old_claims = ctx.tools().token()?.verify_token(&refresh_token)?;
    let touch_ctx = ctx.clone();
    let tokens = RefreshAction::new(UserClaimsResolver {
        account: Arc::clone(&account),
    })
    .handle(ctx, RefreshInput { refresh_token })
    .await?;
    persist_session_rotation(
        &touch_ctx,
        &account,
        &old_claims,
        &tokens.access_token,
        &tokens.refresh_token,
    )
    .await?;
    Account::browser_session().token_response(tokens.access_token, tokens.refresh_token, secure)
}

/// 持久化成功前不暴露新 Token 对；未知提交结果要求重新登录。
async fn persist_session_rotation(
    ctx: &ActionContext,
    account: &Account,
    old_claims: &TokenClaims,
    access_token: &str,
    refresh_token: &str,
) -> Result<(), BaseError> {
    let claims = ctx.tools().token()?.verify_token(access_token)?;
    if claims.token_type != TokenType::Access {
        return Err(BaseError::TokenRevoked);
    }
    let Some(session_id) = claims
        .custom
        .get("session_id")
        .and_then(|value| value.as_str())
    else {
        return Err(BaseError::TokenRevoked);
    };
    // 记录轮换后的 refresh jti，供后续逐台撤销拉黑（否则撤销只命中 access jti）。
    let refresh_jti = ctx.tools().token()?.verify_token(refresh_token)?.jti;
    let user_id = old_claims
        .sub
        .parse::<i64>()
        .map_err(|_| BaseError::TokenRevoked)?;
    let now = current_unix_timestamp()?;
    account
        .sessions()
        .rotate_refresh(
            ctx,
            user_id,
            session_id,
            &old_claims.jti,
            (&claims.jti, &refresh_jti),
            now,
        )
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

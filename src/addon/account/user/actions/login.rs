//! 校验账号密码并签发 Token。

use crate::addon::account::domain::policy::normalize_username;
use crate::addon::account::Account;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::auth::{
    AuthOperation, BrowserSession, CredentialVerifier, LoginAction, LoginInput, VerifiedSubject,
};
use yang_base::action::{ActionContext, ApiResponse, TypedHandler};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct BrowserLoginInput {
    /// 用户名、邮箱或其他登录标识。
    username: String,
    /// 登录凭据。
    password: String,
    /// 可选业务扩展字段。
    #[serde(default)]
    extra: serde_json::Value,
}

impl ParamInput for BrowserLoginInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 把账号凭据校验接到框架内置 `LoginAction` 的端口。
#[derive(Clone)]
struct UserCredentialVerifier {
    account: Arc<Account>,
}

#[async_trait]
impl CredentialVerifier for UserCredentialVerifier {
    async fn verify(
        &self,
        ctx: &ActionContext,
        input: &LoginInput,
    ) -> Result<VerifiedSubject, BaseError> {
        let username = normalize_username(&input.username)?;
        self.account
            .rate_limiter()
            .check(ctx, AuthOperation::Login, &username)
            .await?;
        let user = self
            .account
            .users()
            .find_credentials_by_username(ctx, &username)
            .await?;
        // 等时校验：用户不存在时也必须执行一次完整的 Argon2 校验。
        // 若 miss 分支跳过哈希直接返回错误，「用户不存在」与「密码错误」的响应时间
        // 会相差一次 Argon2 运算（几十到几百毫秒），攻击者可据此枚举用户名是否存在。
        // 因此把 Option<密码哈希> 交给框架等时端口：None 时对内置 dummy 哈希走同一条
        // 校验代码路径，两条分支耗时分布一致，且对外返回同一个 InvalidPassword 错误。
        let password_matches = self
            .account
            .passwords()
            .verify_or_dummy(
                &input.password,
                user.as_ref().map(|user| user.password_hash.as_str()),
            )
            .await?;
        if !password_matches {
            return Err(BaseError::InvalidPassword);
        }
        // verify_or_dummy 对 None 恒返回 false，能走到这里说明用户一定存在。
        let user = user.ok_or(BaseError::InvalidPassword)?;
        Account::ensure_active(user.status)?;
        let claims = self.account.claims_for(ctx, user.id).await?;
        Ok(VerifiedSubject::new(user.id.to_string()).with_token_pair_claims(claims))
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: BrowserLoginInput,
    account: Arc<Account>,
) -> Result<ApiResponse, BaseError> {
    let secure = BrowserSession::validate_same_origin(&ctx.request)?;
    let tokens = LoginAction::new(UserCredentialVerifier { account })
        .handle(
            ctx,
            LoginInput {
                username: input.username,
                password: input.password,
                extra: input.extra,
            },
        )
        .await?;
    Account::browser_session().token_response(tokens.access_token, tokens.refresh_token, secure)
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("login"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&account))
        })
        .route(HttpMethod::Post, "/api/v1/users/login")
        .display_name("登录")
        .description("校验账号密码并签发 Token")
        .public()
        .register()
}

//! 账号模块上下文 `Account`：全部共享机制的唯一出口。
//!
//! 业务用例流程内联在各 Action 文件的 `handle` 中；Action 只从 `Account`
//! 获取能力——资源访问器、事务收尾、版本原语、授权快照与会话收敛都是
//! 它的方法或关联函数，不再有多层自由函数和 re-export 墙。

use super::claims;
use super::repository::UserRepository;
use super::status::UserStatus;
use crate::addon::account::domain::authz_version::{
    disable_locked_user_and_increment_versions, increment_locked_credential_versions,
    lock_user_credential, LockedUserCredential,
};
use crate::addon::account::domain::grants::{AuthorizationGrants, GrantResolver};
use crate::addon::account::domain::password_reset::{
    consume_in_tx, find_target_user, invalid_reset_token, lock_in_tx, LockedPasswordReset,
    PasswordResetReference,
};
use crate::addon::account::domain::system_owner::{OwnerClaimOutcome, SystemOwnerClaimer};
use crate::addon::account::user::table::{UserView, STATUS};
use crate::audit;
use crate::config::SecuritySettings;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::auth::{AuthRateLimiter, BrowserSession, PasswordEngine, TokenPairClaims};
use yang_base::action::{ActionContext, StepUpManager};
use yang_base::token::TokenClaims;
use yang_base::BaseError;
use yang_db::Transaction;

/// 浏览器刷新会话 Cookie 名称（Host-only、HttpOnly、SameSite=Strict）。
const REFRESH_COOKIE_NAME: &str = "yang_refresh";
/// 刷新会话 Cookie 的 Path 作用域。
const REFRESH_COOKIE_PATH: &str = "/api/v1/users";

/// 账号模块上下文：聚合共享资源，并以方法承载跨用例机制。
pub(crate) struct Account {
    users: Arc<UserRepository>,
    passwords: Arc<PasswordEngine>,
    rate_limiter: Arc<AuthRateLimiter>,
    grant_resolver: Arc<dyn GrantResolver>,
    system_owner_claimer: Arc<dyn SystemOwnerClaimer>,
    step_up_manager: Option<Arc<StepUpManager>>,
    issue_refresh_credential_version: bool,
}

impl Account {
    /// 由安全配置派生密码引擎与限流器，装配处只提供有信息量的部分。
    pub(crate) fn new(
        users: UserRepository,
        security: &SecuritySettings,
        grant_resolver: Arc<dyn GrantResolver>,
        system_owner_claimer: Arc<dyn SystemOwnerClaimer>,
        step_up_manager: Option<Arc<StepUpManager>>,
    ) -> Result<Self, BaseError> {
        Ok(Self {
            users: Arc::new(users),
            passwords: Arc::new(PasswordEngine::new(security.argon2_max_concurrency)?),
            rate_limiter: Arc::new(AuthRateLimiter::new(security.rate_limit_config())),
            grant_resolver,
            system_owner_claimer,
            step_up_manager,
            issue_refresh_credential_version: security.issue_refresh_credential_version,
        })
    }

    // ---- 资源访问器 ----

    pub(crate) fn users(&self) -> &UserRepository {
        &self.users
    }

    pub(crate) fn passwords(&self) -> &PasswordEngine {
        &self.passwords
    }

    pub(crate) fn rate_limiter(&self) -> &AuthRateLimiter {
        &self.rate_limiter
    }

    /// 浏览器会话 Cookie 能力（无状态，按需构造）。
    pub(crate) fn browser_session() -> BrowserSession {
        BrowserSession::new(REFRESH_COOKIE_NAME, REFRESH_COOKIE_PATH)
    }

    /// 凭据变更类能力（改密/重置/停用/全量撤销）的发布开关。
    pub(crate) fn credential_mutations_enabled(&self) -> bool {
        self.issue_refresh_credential_version
    }

    /// 组合根配置的 Step-up manager；未配置时 step_up_complete 不注册。
    pub(crate) fn step_up_manager(&self) -> Option<Arc<StepUpManager>> {
        self.step_up_manager.as_ref().map(Arc::clone)
    }

    /// 在注册事务中竞争唯一最终管理员哨兵（当前骨架为不声明的默认实现）。
    pub(crate) async fn claim_system_owner(
        &self,
        transaction: &mut Transaction,
        user_id: i64,
        username: &str,
    ) -> Result<OwnerClaimOutcome, BaseError> {
        self.system_owner_claimer
            .claim(transaction, user_id, username)
            .await
    }

    // ---- 跨用例共享机制 ----

    /// 按 ID 读取启用用户的展示视图（register 与 me 两个用例共享）。
    pub(crate) async fn view_by_id(
        &self,
        ctx: &ActionContext,
        id: i64,
    ) -> Result<UserView, BaseError> {
        let user = self
            .users
            .find_by_id(ctx, id)
            .await?
            .ok_or_else(|| BaseError::UserNotFound(id.to_string()))?;
        let status = UserStatus::from_storage(&user.require::<String>(STATUS)?)?;
        Self::ensure_active(status)?;
        UserView::try_from(&user)
    }

    /// 提交或回滚一个业务事务，回滚失败只记录日志不覆盖原错误。
    pub(crate) async fn finish_transaction<T>(
        transaction: Transaction,
        result: Result<T, BaseError>,
    ) -> Result<T, BaseError> {
        match result {
            Ok(value) => {
                transaction.commit().await.map_err(BaseError::from)?;
                Ok(value)
            }
            Err(error) => {
                if let Err(rollback_error) = transaction.rollback().await {
                    tracing::error!(error = %rollback_error, "账号用例事务回滚失败");
                }
                Err(error)
            }
        }
    }

    /// 用户必须处于启用状态。
    pub(crate) fn ensure_active(status: UserStatus) -> Result<(), BaseError> {
        if !status.is_active() {
            return Err(BaseError::Unauthorized("用户已停用".to_string()));
        }
        Ok(())
    }

    /// 持锁读取用户凭据与两个安全版本（锁在事务连接上以 FOR UPDATE 执行）。
    pub(crate) async fn lock_credential_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
    ) -> Result<LockedUserCredential, BaseError> {
        lock_user_credential(ctx.tools().mysql()?.pool(), transaction, user_id).await
    }

    /// 在持有的用户行锁内递增凭据与授权版本，并追加授权 Outbox。
    pub(crate) async fn increment_versions_in_tx(
        transaction: &mut Transaction,
        locked: &LockedUserCredential,
    ) -> Result<(i64, i64), BaseError> {
        increment_locked_credential_versions(transaction, locked).await
    }

    /// 在持有的用户行锁内停用账号并递增两个安全版本。
    pub(crate) async fn disable_locked_in_tx(
        transaction: &mut Transaction,
        locked: &LockedUserCredential,
    ) -> Result<(i64, i64), BaseError> {
        disable_locked_user_and_increment_versions(transaction, locked).await
    }

    /// 按凭证摘要定位目标用户。
    pub(crate) async fn find_reset_target(
        &self,
        ctx: &ActionContext,
        reference: &PasswordResetReference,
    ) -> Result<Option<i64>, BaseError> {
        find_target_user(ctx.tools().mysql()?.pool(), reference).await
    }

    /// 在事务内锁定密码重置凭证行。
    pub(crate) async fn lock_reset_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        reference: &PasswordResetReference,
    ) -> Result<LockedPasswordReset, BaseError> {
        lock_in_tx(ctx.tools().mysql()?.pool(), transaction, reference).await
    }

    /// 在事务内消费密码重置凭证。
    pub(crate) async fn consume_reset_in_tx(
        transaction: &mut Transaction,
        locked: &LockedPasswordReset,
    ) -> Result<(), BaseError> {
        consume_in_tx(transaction, locked).await
    }

    /// 密码重置凭证无效或已过期的统一错误。
    pub(crate) fn invalid_reset_token() -> BaseError {
        invalid_reset_token()
    }

    /// 持久撤销后尽力把 Redis 水位线即时收敛；失败时补失败审计并返回 false，
    /// 由授权 Outbox Worker 兜底收敛（logout 与 disable_self 两个用例共享）。
    pub(crate) async fn converge_revocation(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        action: &'static str,
        target_kind: &'static str,
    ) -> Result<bool, BaseError> {
        match ctx
            .tools()
            .token()?
            .revoke_by_subject(&user_id.to_string())
            .await
        {
            Ok(()) => Ok(true),
            Err(error) => {
                let audit_event = audit::AuditEvent::new(
                    audit::AuditEventContext::new(
                        audit::AuditActor::user(user_id).map_err(invalid_audit_event)?,
                        None,
                        ctx.request_id(),
                    )
                    .map_err(invalid_audit_event)?,
                    action,
                    Some(audit::entity("user", user_id)?),
                    audit::entity(target_kind, user_id)?,
                    audit::AuditResult::Failed,
                    None,
                    Some(audit::summary([
                        ("error_code", json!(error.code_str())),
                        ("outcome_code", json!("redis_convergence_pending")),
                    ])?),
                )
                .map_err(invalid_audit_event)?;
                if let Err(audit_error) =
                    audit::append_independent(ctx.tools().mysql()?.pool(), &audit_event).await
                {
                    tracing::error!(
                        error_code = error.code_str(),
                        audit_error_code = audit_error.code_str(),
                        user_id,
                        "账号会话 Redis 收敛与失败审计均未完成"
                    );
                }
                Ok(false)
            }
        }
    }

    // ---- 授权快照与 Token 声明（login 与 refresh 共享）----

    /// 按用户 ID 组装 Token 对声明（登录签发时使用）。
    pub(crate) async fn claims_for(
        &self,
        ctx: &ActionContext,
        user_id: i64,
    ) -> Result<TokenPairClaims, BaseError> {
        let snapshot = self.authorization_snapshot(ctx, user_id).await?;
        self.claims_from_snapshot(&snapshot)
    }

    /// 按 Token subject 组装声明（刷新时按 subject 解析时使用）。
    pub(crate) async fn claims_for_subject(
        &self,
        ctx: &ActionContext,
        subject: &str,
    ) -> Result<TokenPairClaims, BaseError> {
        let user_id = subject
            .parse::<i64>()
            .map_err(|_| BaseError::Unauthorized("Token subject 无效".to_string()))?;
        self.claims_for(ctx, user_id).await
    }

    /// 按旧 Refresh Token 声明组装新声明，并校验凭据版本未失效。
    pub(crate) async fn claims_for_refresh(
        &self,
        ctx: &ActionContext,
        old_claims: &TokenClaims,
    ) -> Result<TokenPairClaims, BaseError> {
        let user_id = old_claims
            .sub
            .parse::<i64>()
            .map_err(|_| BaseError::Unauthorized("Token subject 无效".to_string()))?;
        let snapshot = self.authorization_snapshot(ctx, user_id).await?;
        claims::validate_refresh_credential_version(old_claims, snapshot.credential_version)?;
        self.claims_from_snapshot(&snapshot)
    }

    fn claims_from_snapshot(
        &self,
        snapshot: &AuthorizationSnapshot,
    ) -> Result<TokenPairClaims, BaseError> {
        claims::claims_for_user(
            &snapshot.username,
            snapshot.authz_version,
            snapshot.credential_version,
            self.issue_refresh_credential_version,
            &snapshot.grants,
        )
    }

    /// 在只读事务内组装授权快照：用户状态、两个安全版本与外围域授权扩展。
    async fn authorization_snapshot(
        &self,
        ctx: &ActionContext,
        user_id: i64,
    ) -> Result<AuthorizationSnapshot, BaseError> {
        let mut transaction = ctx
            .tools()
            .mysql()?
            .read_only_transaction()
            .await
            .map_err(BaseError::from)?;
        let snapshot_result: Result<AuthorizationSnapshot, BaseError> = async {
            let state = self
                .users
                .find_authorization_state_in_tx(ctx, &mut transaction, user_id)
                .await?
                .ok_or_else(|| BaseError::UserNotFound(user_id.to_string()))?;
            Self::ensure_active(state.status)?;
            if state.authz_version < 1 {
                return Err(BaseError::Unauthorized("用户授权版本无效".to_string()));
            }
            if state.credential_version < 0 {
                return Err(BaseError::Unauthorized("用户凭据版本无效".to_string()));
            }
            let grants = AuthorizationGrants::user().extend(
                self.grant_resolver
                    .resolve(ctx, user_id, &mut transaction)
                    .await?,
            );
            Ok(AuthorizationSnapshot {
                username: state.username,
                authz_version: state.authz_version,
                credential_version: state.credential_version,
                grants,
            })
        }
        .await;

        match snapshot_result {
            Ok(snapshot) => {
                transaction.commit().await.map_err(BaseError::from)?;
                Ok(snapshot)
            }
            Err(error) => {
                if let Err(rollback_error) = transaction.rollback().await {
                    tracing::error!(
                        "授权快照失败后回滚事务也失败: user_id={}, error={}",
                        user_id,
                        rollback_error
                    );
                }
                Err(error)
            }
        }
    }
}

/// 登录/刷新共享的授权快照。
struct AuthorizationSnapshot {
    username: String,
    authz_version: i64,
    credential_version: i64,
    grants: AuthorizationGrants,
}

/// 账号审计事件构建失败的统一错误形态。
fn invalid_audit_event(error: anyhow::Error) -> BaseError {
    BaseError::ConfigError(format!("构建账号生命周期审计事件失败: {error}"))
}

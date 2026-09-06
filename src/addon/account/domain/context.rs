//! 账号模块上下文 `Account`：全部共享机制的唯一出口。
//!
//! 业务用例流程内联在各 Action 文件的 `handle` 中；Action 只从 `Account`
//! 获取能力——资源访问器、事务收尾、版本原语、授权快照与会话收敛都是
//! 它的方法或关联函数，不再有多层自由函数和 re-export 墙。

use super::claims;
use super::login_event::LoginEventRepository;
use super::repository::UserRepository;
use super::session::SessionRepository;
use super::status::UserStatus;
use crate::addon::account::domain::authz_version::{
    activate_locked_user_and_increment_versions, anonymize_locked_user_and_increment_versions,
    disable_locked_user_and_increment_versions, increment_locked_credential_versions,
    lock_user_credential, LockedUserCredential,
};
use crate::addon::account::domain::grants::{AuthorizationGrants, GrantResolver};
use crate::addon::account::domain::password_reset::{
    consume_in_tx, find_target_user, insert_issued, insert_issued_by, invalid_reset_token,
    invalidate_all_for_user_in_tx, lock_in_tx, IssuedPasswordReset, LockedPasswordReset,
    PasswordResetReference,
};
use crate::addon::account::domain::system_owner::{OwnerClaimOutcome, SystemOwnerClaimer};
use crate::addon::account::user::table::{UserView, STATUS};
use crate::audit;
use crate::config::{SecuritySettings, TotpSettings};
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
    sessions: Arc<SessionRepository>,
    login_events: Arc<LoginEventRepository>,
    passwords: Arc<PasswordEngine>,
    rate_limiter: Arc<AuthRateLimiter>,
    grant_resolver: Arc<dyn GrantResolver>,
    system_owner_claimer: Arc<dyn SystemOwnerClaimer>,
    step_up_manager: Option<Arc<StepUpManager>>,
    issue_refresh_credential_version: bool,
    password_reset_ttl_seconds: u64,
    totp_settings: Option<TotpSettings>,
}

impl Account {
    /// 由安全配置派生密码引擎与限流器，装配处只提供有信息量的部分。
    pub(crate) fn new(
        users: UserRepository,
        sessions: SessionRepository,
        login_events: LoginEventRepository,
        security: &SecuritySettings,
        grant_resolver: Arc<dyn GrantResolver>,
        system_owner_claimer: Arc<dyn SystemOwnerClaimer>,
        step_up_manager: Option<Arc<StepUpManager>>,
    ) -> Result<Self, BaseError> {
        Ok(Self {
            users: Arc::new(users),
            sessions: Arc::new(sessions),
            login_events: Arc::new(login_events),
            passwords: Arc::new(PasswordEngine::new(security.argon2_max_concurrency)?),
            rate_limiter: Arc::new(AuthRateLimiter::new(security.rate_limit_config())),
            grant_resolver,
            system_owner_claimer,
            step_up_manager,
            issue_refresh_credential_version: security.issue_refresh_credential_version,
            password_reset_ttl_seconds: security.password_reset_ttl_seconds,
            totp_settings: security.totp.clone(),
        })
    }

    // ---- 资源访问器 ----

    pub(crate) fn users(&self) -> &UserRepository {
        &self.users
    }

    pub(crate) fn sessions(&self) -> &SessionRepository {
        &self.sessions
    }

    pub(crate) fn login_events(&self) -> &LoginEventRepository {
        &self.login_events
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

    /// TOTP 配置域；未配置时 MFA Action 不注册。
    pub(crate) fn totp_settings(&self) -> Option<&TotpSettings> {
        self.totp_settings.as_ref()
    }

    /// 在注册事务中竞争唯一最终管理员哨兵（当前骨架为不声明的默认实现）。
    /// 从当前请求 access token 的 claims 提取会话标识（无 token/老格式返回 None）。
    pub(crate) fn session_id_from_request(&self, ctx: &ActionContext) -> Option<String> {
        let token = ctx.request.token()?;
        let claims = ctx.tools().token().ok()?.verify_token(token).ok()?;
        claims
            .custom
            .get("session_id")
            .and_then(|value| value.as_str())
            .map(str::to_string)
    }

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

    /// 校验账号第二因子（E-1c/E-1d 验收）：TOTP 码优先，失败后试一次性
    /// 恢复码（命中则独立事务单次消费）。任何失败返回 `InvalidPassword`，
    /// 与密码错误同响应（防枚举）。
    ///
    /// 恢复码消费是独立事务：与登录/Step-up 的签发路径无共享写，单次
    /// 消费语义由事务内「摘要移除 + 回写」保证。
    pub(crate) async fn verify_second_factor(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        state: &crate::addon::account::domain::repository::TotpStateRecord,
        secret: &str,
        code: &str,
    ) -> Result<(), BaseError> {
        let verifier = yang_base::action::auth::TotpLiteVerifier::default();
        if yang_base::action::auth::TotpVerifier::verify(&verifier, secret, code)
            .await
            .is_ok()
        {
            return Ok(());
        }
        // TOTP 码失败 → 尝试一次性恢复码（单次消费）。
        if crate::addon::account::domain::mfa::recovery_digest_matches(
            state.totp_recovery_digest.as_deref().unwrap_or("[]"),
            code,
        ) {
            let mut transaction = ctx.tools().mysql()?.transaction().await?;
            let consumed = self
                .users()
                .consume_recovery_code_in_tx(ctx, &mut transaction, user_id, code)
                .await?;
            if consumed {
                transaction.commit().await.map_err(BaseError::from)?;
                return Ok(());
            }
            let _ = transaction.rollback().await;
        }
        Err(BaseError::InvalidPassword)
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

    /// 在持有的用户行锁内匿名化删除账号并递增两个安全版本（路线图 E-2b）。
    pub(crate) async fn anonymize_locked_in_tx(
        transaction: &mut Transaction,
        locked: &LockedUserCredential,
        deleted_username: &str,
    ) -> Result<(i64, i64), BaseError> {
        anonymize_locked_user_and_increment_versions(transaction, locked, deleted_username).await
    }

    /// 在持有的用户行锁内启用账号并递增两个安全版本（管理动作 D-1）。
    pub(crate) async fn activate_locked_in_tx(
        transaction: &mut Transaction,
        locked: &LockedUserCredential,
    ) -> Result<(i64, i64), BaseError> {
        activate_locked_user_and_increment_versions(transaction, locked).await
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

    /// 在事务内作废某用户的全部未消费重置凭证（匿名化删除前置清理）。
    pub(crate) async fn invalidate_resets_in_tx(
        transaction: &mut Transaction,
        user_id: i64,
    ) -> Result<(), BaseError> {
        invalidate_all_for_user_in_tx(transaction, user_id).await
    }

    /// 自助密码重置凭证的有效期（秒）。
    pub(crate) fn password_reset_ttl_seconds(&self) -> u64 {
        self.password_reset_ttl_seconds
    }

    /// 为启用用户签发一条自助密码重置凭证并入库（只存摘要与指纹）。
    pub(crate) async fn issue_password_reset(
        &self,
        ctx: &ActionContext,
        user_id: i64,
    ) -> Result<IssuedPasswordReset, BaseError> {
        let issued = IssuedPasswordReset::generate()?;
        insert_issued(
            ctx.tools().mysql()?.pool(),
            user_id,
            &issued,
            self.password_reset_ttl_seconds,
        )
        .await?;
        Ok(issued)
    }

    /// 签发密码重置凭证并记录操作者（管理签发路线图 D-2）。
    pub(crate) async fn issue_password_reset_by(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        requested_by_user: Option<i64>,
    ) -> Result<IssuedPasswordReset, BaseError> {
        let issued = IssuedPasswordReset::generate()?;
        insert_issued_by(
            ctx.tools().mysql()?.pool(),
            user_id,
            &issued,
            self.password_reset_ttl_seconds,
            requested_by_user,
        )
        .await?;
        Ok(issued)
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
    ///
    /// 每次登录生成新的 `session_id`（跨 Refresh 轮换稳定，`user_session` 表
    /// 以它为键）；换绑/改密/停用等凭据变更递增 credential_version 使会话失效。
    pub(crate) async fn claims_for(
        &self,
        ctx: &ActionContext,
        user_id: i64,
    ) -> Result<TokenPairClaims, BaseError> {
        let snapshot = self.authorization_snapshot(ctx, user_id).await?;
        let session_id = new_session_id()?;
        self.claims_from_snapshot(&snapshot, Some(&session_id))
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
    ///
    /// `session_id` 从旧 claims 继承（老 Token 无该字段时按无会话记录降级，
    /// 不拒绝既有会话），保证「踢出某设备」在轮换后仍能定位同一会话行。
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
        let inherited_session_id = claims::session_id_from_claims(old_claims)?;
        self.claims_from_snapshot(&snapshot, inherited_session_id.as_deref())
    }

    fn claims_from_snapshot(
        &self,
        snapshot: &AuthorizationSnapshot,
        session_id: Option<&str>,
    ) -> Result<TokenPairClaims, BaseError> {
        claims::claims_for_user(
            &snapshot.username,
            snapshot.authz_version,
            snapshot.credential_version,
            self.issue_refresh_credential_version,
            &snapshot.grants,
            session_id,
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

/// 生成跨 Refresh 轮换稳定的会话标识（UUID v4，`user_session` 表主键）。
fn new_session_id() -> Result<String, BaseError> {
    Ok(uuid::Uuid::new_v4().to_string())
}

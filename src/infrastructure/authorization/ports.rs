//! 授权失效公共端口：读回源与事务内 writer。
//!
//! 分层约定：infrastructure 只定义端口抽象，不依赖任何业务 Addon；
//! 端口由 account 域实现（`users.authz_version` 的 SQL 仍只存在于账号域
//! 单一 writer 文件），组合根（`app.rs`）装配一次后分发给 Token 校验器
//! 与需要使授权失效的业务 Addon。

use async_trait::async_trait;
use sqlx::MySqlPool;
use std::sync::Arc;
use yang_base::BaseError;
use yang_db::Transaction;

/// 授权版本回源快照（与账号域的存储表示解耦）。
#[derive(Debug, Clone)]
pub struct AuthorizationVersionSnapshot {
    status: &'static str,
    active: bool,
    version: i64,
}

impl AuthorizationVersionSnapshot {
    /// 构造回源快照；仅供端口实现方（账号域）与测试使用。
    pub(crate) fn new(status: &'static str, active: bool, version: i64) -> Self {
        Self {
            status,
            active,
            version,
        }
    }

    /// 存储层状态标签，仅用于日志与审计。
    pub fn status(&self) -> &'static str {
        self.status
    }

    /// 用户是否处于启用状态。
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// 当前授权版本。
    pub fn version(&self) -> i64 {
        self.version
    }
}

/// 授权版本读取端口：Token 校验回源与管理查询共享的事实源。
#[async_trait]
pub trait AuthorizationVersionSource: Send + Sync {
    /// 从最终事实源（MySQL）读取用户授权版本快照；用户不存在返回 `None`。
    async fn find_authorization_version(
        &self,
        pool: &MySqlPool,
        user_id: i64,
    ) -> Result<Option<AuthorizationVersionSnapshot>, BaseError>;
}

/// 已在调用方事务中锁定的授权失效句柄。
///
/// 不透明令牌：业务 Addon 只能读取启用状态，并把句柄交回 writer 端口递增；
/// 构造与版本字段只对 crate 内端口实现方可见。
pub struct LockedAuthorization {
    user_id: i64,
    active: bool,
    authz_version: i64,
}

impl LockedAuthorization {
    /// 构造锁句柄；仅供端口实现方（账号域）与测试使用。
    pub(crate) fn new(user_id: i64, active: bool, authz_version: i64) -> Self {
        Self {
            user_id,
            active,
            authz_version,
        }
    }

    /// 锁定时观察到的用户是否处于启用状态。
    pub fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn user_id(&self) -> i64 {
        self.user_id
    }

    pub(crate) fn authz_version(&self) -> i64 {
        self.authz_version
    }
}

/// 授权失效 writer 端口：使某用户的 Access Token 失效。
///
/// 业务 Addon 因自身授权事实变化（如权限授予/撤销）使用户 Token 失效时，
/// 必须在同一事务中先 [`lock_authorization_version`](Self::lock_authorization_version)
/// 持有用户行锁、写完自身事实后再
/// [`increment_locked_authorization_version`](Self::increment_locked_authorization_version)
/// 单调递增版本并追加授权 Outbox；凭据版本不变，Refresh 会话保持有效。
#[async_trait]
pub trait AuthorizationVersionWriter: Send + Sync {
    /// 在调用方事务中锁定目标用户的授权版本（FOR UPDATE）。
    async fn lock_authorization_version(
        &self,
        pool: &MySqlPool,
        transaction: &mut Transaction,
        user_id: i64,
    ) -> Result<LockedAuthorization, BaseError>;

    /// 在持有的用户行锁内单调递增授权版本，并写入授权 Outbox。
    async fn increment_locked_authorization_version(
        &self,
        transaction: &mut Transaction,
        locked: &LockedAuthorization,
    ) -> Result<i64, BaseError>;
}

/// 授权失效公共端口句柄：读回源 + 事务内 writer。
///
/// 组合根装配一次，按需分发给校验器（读）与业务 Addon（读+写）；
/// `Clone` 只克隆内部 `Arc`，所有克隆共享同一实现。
#[derive(Clone)]
pub struct AuthorizationPort {
    source: Arc<dyn AuthorizationVersionSource>,
    writer: Arc<dyn AuthorizationVersionWriter>,
}

impl AuthorizationPort {
    pub fn new(
        source: Arc<dyn AuthorizationVersionSource>,
        writer: Arc<dyn AuthorizationVersionWriter>,
    ) -> Self {
        Self { source, writer }
    }

    /// 读取端口（Token 校验器回源使用）。
    pub fn source(&self) -> Arc<dyn AuthorizationVersionSource> {
        Arc::clone(&self.source)
    }

    /// 读取用户授权版本快照（管理查询等业务 Addon 使用）。
    pub async fn find_authorization_version(
        &self,
        pool: &MySqlPool,
        user_id: i64,
    ) -> Result<Option<AuthorizationVersionSnapshot>, BaseError> {
        self.source.find_authorization_version(pool, user_id).await
    }

    /// 在调用方事务中锁定目标用户的授权版本（FOR UPDATE）。
    pub async fn lock_authorization_version(
        &self,
        pool: &MySqlPool,
        transaction: &mut Transaction,
        user_id: i64,
    ) -> Result<LockedAuthorization, BaseError> {
        self.writer
            .lock_authorization_version(pool, transaction, user_id)
            .await
    }

    /// 在持有的用户行锁内单调递增授权版本，并写入授权 Outbox。
    pub async fn increment_locked_authorization_version(
        &self,
        transaction: &mut Transaction,
        locked: &LockedAuthorization,
    ) -> Result<i64, BaseError> {
        self.writer
            .increment_locked_authorization_version(transaction, locked)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::mysql::MySqlPoolOptions;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_pool() -> MySqlPool {
        MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"))
    }

    #[test]
    fn snapshot_and_locked_handle_expose_only_their_contracts() {
        let snapshot = AuthorizationVersionSnapshot::new("active", true, 7);
        assert!(snapshot.is_active());
        assert_eq!(snapshot.status(), "active");
        assert_eq!(snapshot.version(), 7);

        let locked = LockedAuthorization::new(42, false, 3);
        assert!(!locked.is_active());
        assert_eq!(locked.user_id(), 42);
        assert_eq!(locked.authz_version(), 3);
    }

    struct FakeSource {
        calls: AtomicUsize,
        snapshot: Option<AuthorizationVersionSnapshot>,
    }

    #[async_trait]
    impl AuthorizationVersionSource for FakeSource {
        async fn find_authorization_version(
            &self,
            _pool: &MySqlPool,
            _user_id: i64,
        ) -> Result<Option<AuthorizationVersionSnapshot>, BaseError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.snapshot.clone())
        }
    }

    #[tokio::test]
    async fn port_facade_delegates_reads_to_the_injected_source() {
        let source = Arc::new(FakeSource {
            calls: AtomicUsize::new(0),
            snapshot: Some(AuthorizationVersionSnapshot::new("disabled", false, 9)),
        });
        let port = AuthorizationPort::new(
            source.clone() as Arc<dyn AuthorizationVersionSource>,
            Arc::new(FakeWriter),
        );

        let snapshot = port
            .find_authorization_version(&test_pool(), 7)
            .await
            .unwrap_or_else(|error| panic!("端口读取应成功: {error}"))
            .unwrap_or_else(|| panic!("应返回快照"));
        assert!(!snapshot.is_active());
        assert_eq!(snapshot.version(), 9);
        assert_eq!(
            source.calls.load(Ordering::SeqCst),
            1,
            "读取必须经过注入的端口实现"
        );

        // source() 暴露的读端口必须与门面共享同一实现。
        let direct = port.source();
        direct
            .find_authorization_version(&test_pool(), 7)
            .await
            .unwrap_or_else(|error| panic!("端口读取应成功: {error}"));
        assert_eq!(source.calls.load(Ordering::SeqCst), 2);

        let empty_source = Arc::new(FakeSource {
            calls: AtomicUsize::new(0),
            snapshot: None,
        });
        let missing = empty_source
            .find_authorization_version(&test_pool(), 8)
            .await
            .unwrap_or_else(|error| panic!("端口读取应成功: {error}"));
        assert!(missing.is_none(), "用户不存在必须返回 None");
    }

    struct FakeWriter;

    #[async_trait]
    impl AuthorizationVersionWriter for FakeWriter {
        async fn lock_authorization_version(
            &self,
            _pool: &MySqlPool,
            _transaction: &mut Transaction,
            _user_id: i64,
        ) -> Result<LockedAuthorization, BaseError> {
            Err(BaseError::Unauthorized(
                "fake writer 不提供事务".to_string(),
            ))
        }

        async fn increment_locked_authorization_version(
            &self,
            _transaction: &mut Transaction,
            _locked: &LockedAuthorization,
        ) -> Result<i64, BaseError> {
            Err(BaseError::Unauthorized(
                "fake writer 不提供事务".to_string(),
            ))
        }
    }
}

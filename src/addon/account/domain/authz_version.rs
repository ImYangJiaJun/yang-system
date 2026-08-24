//! 授权事实 writer 共享的用户版本锁与递增原语。
//! authorization-writer: account-security-version

use sqlx::MySqlPool;
use yang_base::BaseError;
use yang_db::{field, table, CompareOp, QueryBuilder, SqlExpr, Transaction};

use crate::addon::account::UserStatus;

/// 已在当前事务中锁定的用户授权状态。
#[cfg(any(feature = "admin", feature = "org"))]
pub(crate) struct LockedUserAuthorization {
    user_id: i64,
    // 只有 admin 的最终管理员保护会复核锁定时的用户状态。
    #[cfg_attr(not(feature = "admin"), allow(dead_code))]
    status: UserStatus,
    authz_version: i64,
}

/// 已在当前事务中锁定的用户凭据与版本状态。
///
/// 密码摘要只允许在此窄边界内用于并发复核，不提供读取接口。
pub(crate) struct LockedUserCredential {
    user_id: i64,
    status: UserStatus,
    password_hash: String,
    authz_version: i64,
    credential_version: i64,
}

impl LockedUserCredential {
    /// 返回锁定时观察到的用户状态。
    pub(crate) fn status(&self) -> UserStatus {
        self.status
    }

    /// 比较事务外观察到的摘要，防止并发改密覆盖。
    pub(crate) fn password_hash_matches(&self, observed: &str) -> bool {
        self.password_hash == observed
    }
}

#[cfg(any(feature = "admin", feature = "org"))]
impl LockedUserAuthorization {
    /// 返回锁定的用户 ID。
    pub(crate) fn user_id(&self) -> i64 {
        self.user_id
    }

    /// 返回锁定时观察到的用户状态。
    #[cfg(feature = "admin")]
    pub(crate) fn status(&self) -> UserStatus {
        self.status
    }
}

/// 读取 Token 校验所需的最小授权事实。
pub(crate) async fn find_authorization_version(
    pool: &MySqlPool,
    user_id: i64,
) -> Result<Option<(UserStatus, i64)>, BaseError> {
    let row: Option<(String, i64)> = QueryBuilder::from_pool(pool, table!("users"))
        .field(field!("status"))
        .field(field!("authz_version"))
        .where_and(field!("id"), CompareOp::Eq, user_id)
        .find()
        .await
        .map_err(BaseError::from)?;
    row.map(|(status, version)| Ok((UserStatus::from_storage(&status)?, version)))
        .transpose()
}

/// 锁定用户行，并读取授权 writer 所需的最小状态。
///
/// `pool` 只用于构建查询；语句仍在 `transaction` 的连接上以 `FOR UPDATE` 执行。
#[cfg(any(feature = "admin", feature = "org"))]
pub(crate) async fn lock_user_authorization(
    pool: &MySqlPool,
    transaction: &mut Transaction,
    user_id: i64,
) -> Result<LockedUserAuthorization, BaseError> {
    let (status, authz_version) = transaction
        .select_for_update::<(String, i64)>(
            QueryBuilder::from_pool(pool, table!("users"))
                .field(field!("status"))
                .field(field!("authz_version"))
                .where_and(field!("id"), CompareOp::Eq, user_id),
        )
        .await
        .map_err(BaseError::from)?
        .into_iter()
        .next()
        .ok_or_else(|| BaseError::UserNotFound(user_id.to_string()))?;
    if authz_version < 1 {
        return Err(BaseError::Unauthorized(
            "用户授权版本必须是正整数".to_string(),
        ));
    }
    Ok(LockedUserAuthorization {
        user_id,
        status: UserStatus::from_storage(&status)?,
        authz_version,
    })
}

/// 一次性锁定改密需要的摘要和两个版本，避免非锁定快照读取旧摘要。
pub(crate) async fn lock_user_credential(
    pool: &MySqlPool,
    transaction: &mut Transaction,
    user_id: i64,
) -> Result<LockedUserCredential, BaseError> {
    let (status, password_hash, authz_version, credential_version) = transaction
        .select_for_update::<(String, String, i64, i64)>(
            QueryBuilder::from_pool(pool, table!("users"))
                .field(field!("status"))
                .field(field!("password_hash"))
                .field(field!("authz_version"))
                .field(field!("credential_version"))
                .where_and(field!("id"), CompareOp::Eq, user_id),
        )
        .await
        .map_err(BaseError::from)?
        .into_iter()
        .next()
        .ok_or_else(|| BaseError::UserNotFound(user_id.to_string()))?;
    if authz_version < 1 || credential_version < 0 {
        return Err(BaseError::Unauthorized("用户安全版本无效".to_string()));
    }
    Ok(LockedUserCredential {
        user_id,
        status: UserStatus::from_storage(&status)?,
        password_hash,
        authz_version,
        credential_version,
    })
}

/// 按稳定用户 ID 顺序去重并锁定授权状态，避免多用户 writer 形成反向锁序。
#[cfg(feature = "org")]
pub(crate) async fn lock_user_authorizations(
    pool: &MySqlPool,
    transaction: &mut Transaction,
    user_ids: impl IntoIterator<Item = i64>,
) -> Result<Vec<LockedUserAuthorization>, BaseError> {
    let user_ids = stable_user_ids(user_ids);
    let mut locked = Vec::with_capacity(user_ids.len());
    for user_id in user_ids {
        locked.push(lock_user_authorization(pool, transaction, user_id).await?);
    }
    Ok(locked)
}

/// 在持有用户行锁的同一事务中递增版本、写入 Outbox，并返回新版本。
#[cfg(any(feature = "admin", feature = "org"))]
pub(crate) async fn increment_locked_authz_version(
    transaction: &mut Transaction,
    locked: &LockedUserAuthorization,
) -> Result<i64, BaseError> {
    let next = next_authz_version(locked.authz_version)?;
    let affected = transaction
        .table(table!("users"))
        .where_and(field!("id"), CompareOp::Eq, locked.user_id)
        .where_and(field!("authz_version"), CompareOp::Eq, locked.authz_version)
        .update(&serde_json::json!({ "authz_version": next }))
        .await?;
    if affected != 1 {
        return Err(BaseError::from(yang_db::DbError::TransactionError(
            format!("用户 {} 授权版本在持锁事务内发生意外变化", locked.user_id),
        )));
    }
    append_authorization_outbox(transaction, locked.user_id, next).await?;
    Ok(next)
}

/// 在改密事务的同一把用户行锁下递增凭据与授权版本，并写入授权 Outbox。
pub(crate) async fn increment_locked_credential_versions(
    transaction: &mut Transaction,
    locked: &LockedUserCredential,
) -> Result<(i64, i64), BaseError> {
    let next_authz = next_authz_version(locked.authz_version)?;
    let next_credential = next_credential_version(locked.credential_version)?;
    let affected = transaction
        .table(table!("users"))
        .where_and(field!("id"), CompareOp::Eq, locked.user_id)
        .where_and(field!("authz_version"), CompareOp::Eq, locked.authz_version)
        .where_and(
            field!("credential_version"),
            CompareOp::Eq,
            locked.credential_version,
        )
        .update(&serde_json::json!({
            "authz_version": next_authz,
            "credential_version": next_credential,
        }))
        .await?;
    if affected != 1 {
        return Err(BaseError::from(yang_db::DbError::TransactionError(
            format!("用户 {} 安全版本在持锁事务内发生意外变化", locked.user_id),
        )));
    }
    append_authorization_outbox(transaction, locked.user_id, next_authz).await?;
    Ok((next_authz, next_credential))
}

/// 在账号停用事务中同时写入状态、两个安全版本与授权 Outbox。
pub(crate) async fn disable_locked_user_and_increment_versions(
    transaction: &mut Transaction,
    locked: &LockedUserCredential,
) -> Result<(i64, i64), BaseError> {
    let next_authz = next_authz_version(locked.authz_version)?;
    let next_credential = next_credential_version(locked.credential_version)?;
    let affected = transaction
        .table(table!("users"))
        .where_and(field!("id"), CompareOp::Eq, locked.user_id)
        .where_and(field!("status"), CompareOp::Eq, locked.status.as_str())
        .where_and(field!("authz_version"), CompareOp::Eq, locked.authz_version)
        .where_and(
            field!("credential_version"),
            CompareOp::Eq,
            locked.credential_version,
        )
        .update(&serde_json::json!({
            "status": UserStatus::Disabled.as_str(),
            "authz_version": next_authz,
            "credential_version": next_credential,
        }))
        .await?;
    if affected != 1 {
        return Err(BaseError::from(yang_db::DbError::TransactionError(
            format!("用户 {} 停用事实在持锁事务内发生意外变化", locked.user_id),
        )));
    }
    append_authorization_outbox(transaction, locked.user_id, next_authz).await?;
    Ok((next_authz, next_credential))
}

async fn append_authorization_outbox(
    transaction: &mut Transaction,
    user_id: i64,
    authz_version: i64,
) -> Result<(), BaseError> {
    transaction
        .table(table!("authorization_outbox"))
        .set_expr(field!("available_at"), SqlExpr::unix_timestamp())
        .set_expr(field!("created_at"), SqlExpr::unix_timestamp())
        .insert(&serde_json::json!({
            "user_id": user_id,
            "authz_version": authz_version,
            "state": "pending",
            "attempts": 0,
        }))
        .await?;
    Ok(())
}

/// 按已锁定顺序递增所有受影响用户，同一用户在调用前已经去重。
#[cfg(feature = "org")]
pub(crate) async fn increment_locked_authz_versions(
    transaction: &mut Transaction,
    locked: &[LockedUserAuthorization],
) -> Result<Vec<(i64, i64)>, BaseError> {
    let mut versions = Vec::with_capacity(locked.len());
    for user in locked {
        let version = increment_locked_authz_version(transaction, user).await?;
        versions.push((user.user_id(), version));
    }
    Ok(versions)
}

#[cfg(feature = "org")]
fn stable_user_ids(user_ids: impl IntoIterator<Item = i64>) -> Vec<i64> {
    let mut user_ids = user_ids.into_iter().collect::<Vec<_>>();
    user_ids.sort_unstable();
    user_ids.dedup();
    user_ids
}

fn next_authz_version(current: i64) -> Result<i64, BaseError> {
    current
        .checked_add(1)
        .filter(|next| *next > 1)
        .ok_or_else(|| {
            BaseError::from(yang_db::DbError::TransactionError(
                "用户授权版本已耗尽或无效".to_string(),
            ))
        })
}

fn next_credential_version(current: i64) -> Result<i64, BaseError> {
    current
        .checked_add(1)
        .filter(|next| *next > 0)
        .ok_or_else(|| {
            BaseError::from(yang_db::DbError::TransactionError(
                "用户凭据版本已耗尽或无效".to_string(),
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_version_is_positive_monotonic_and_overflow_safe() {
        assert_eq!(
            next_authz_version(1).unwrap_or_else(|error| panic!("版本 1 应可递增: {error}")),
            2
        );
        assert!(next_authz_version(0).is_err());
        assert!(next_authz_version(-1).is_err());
        assert!(next_authz_version(i64::MAX).is_err());
    }

    #[test]
    fn credential_version_starts_at_zero_and_is_monotonic_and_overflow_safe() {
        assert_eq!(
            next_credential_version(0)
                .unwrap_or_else(|error| panic!("凭据版本 0 应可递增: {error}")),
            1
        );
        assert!(next_credential_version(-1).is_err());
        assert!(next_credential_version(i64::MAX).is_err());
    }

    #[test]
    fn locked_credential_detects_a_concurrent_digest_change() {
        let locked = LockedUserCredential {
            user_id: 7,
            status: UserStatus::Active,
            password_hash: "new-digest".to_string(),
            authz_version: 2,
            credential_version: 1,
        };

        assert!(locked.password_hash_matches("new-digest"));
        assert!(!locked.password_hash_matches("observed-before-lock"));
    }

    #[cfg(feature = "org")]
    #[test]
    fn authorization_users_are_locked_once_in_stable_order() {
        assert_eq!(stable_user_ids([9, 3, 9, 5, 3]), [3, 5, 9]);
    }
}

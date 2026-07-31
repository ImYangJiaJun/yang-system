//! 授权事实 writer 共享的用户版本锁与递增原语。
//! raw-sql-boundary: domain-service account-authz-version
//! authorization-writer: account-security-version

use sqlx::MySqlPool;
use yang_base::BaseError;
use yang_db::Transaction;

use super::UserStatus;

/// 已在当前事务中锁定的用户授权状态。
pub(crate) struct LockedUserAuthorization {
    user_id: i64,
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

impl LockedUserAuthorization {
    /// 返回锁定的用户 ID。
    pub(crate) fn user_id(&self) -> i64 {
        self.user_id
    }

    /// 返回锁定时观察到的用户状态。
    pub(crate) fn status(&self) -> UserStatus {
        self.status
    }
}

/// 读取 Token 校验所需的最小授权事实。
pub(crate) async fn find_authorization_version(
    pool: &MySqlPool,
    user_id: i64,
) -> Result<Option<(UserStatus, i64)>, BaseError> {
    let row: Option<(String, i64)> = sqlx::query_as(
        "SELECT status, authz_version \
         FROM users \
         WHERE id = ? \
         LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(yang_db::DbError::from)
    .map_err(BaseError::from)?;
    row.map(|(status, version)| Ok((UserStatus::from_storage(&status)?, version)))
        .transpose()
}

/// 锁定用户行，并读取授权 writer 所需的最小状态。
pub(crate) async fn lock_user_authorization(
    transaction: &mut Transaction,
    user_id: i64,
) -> Result<LockedUserAuthorization, BaseError> {
    let (status, authz_version) = sqlx::query_as::<_, (String, i64)>(
        "SELECT status, authz_version FROM users WHERE id = ? FOR UPDATE",
    )
    .bind(user_id)
    .fetch_optional(executor(transaction)?)
    .await
    .map_err(yang_db::DbError::from)?
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
    transaction: &mut Transaction,
    user_id: i64,
) -> Result<LockedUserCredential, BaseError> {
    let (status, password_hash, authz_version, credential_version) =
        sqlx::query_as::<_, (String, String, i64, i64)>(
            "SELECT status, password_hash, authz_version, credential_version \
             FROM users WHERE id = ? FOR UPDATE",
        )
        .bind(user_id)
        .fetch_optional(executor(transaction)?)
        .await
        .map_err(yang_db::DbError::from)?
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
pub(crate) async fn lock_user_authorizations(
    transaction: &mut Transaction,
    user_ids: impl IntoIterator<Item = i64>,
) -> Result<Vec<LockedUserAuthorization>, BaseError> {
    let user_ids = stable_user_ids(user_ids);
    let mut locked = Vec::with_capacity(user_ids.len());
    for user_id in user_ids {
        locked.push(lock_user_authorization(transaction, user_id).await?);
    }
    Ok(locked)
}

/// 在持有用户行锁的同一事务中递增版本、写入 Outbox，并返回新版本。
pub(crate) async fn increment_locked_authz_version(
    transaction: &mut Transaction,
    locked: &LockedUserAuthorization,
) -> Result<i64, BaseError> {
    let next = next_authz_version(locked.authz_version)?;
    let result =
        sqlx::query("UPDATE users SET authz_version = ? WHERE id = ? AND authz_version = ?")
            .bind(next)
            .bind(locked.user_id)
            .bind(locked.authz_version)
            .execute(executor(transaction)?)
            .await
            .map_err(yang_db::DbError::from)?;
    if result.rows_affected() != 1 {
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
    let result = sqlx::query(
        "UPDATE users SET authz_version = ?, credential_version = ? \
         WHERE id = ? AND authz_version = ? AND credential_version = ?",
    )
    .bind(next_authz)
    .bind(next_credential)
    .bind(locked.user_id)
    .bind(locked.authz_version)
    .bind(locked.credential_version)
    .execute(executor(transaction)?)
    .await
    .map_err(yang_db::DbError::from)?;
    if result.rows_affected() != 1 {
        return Err(BaseError::from(yang_db::DbError::TransactionError(
            format!("用户 {} 安全版本在持锁事务内发生意外变化", locked.user_id),
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
    sqlx::query(
        "INSERT INTO authorization_outbox \
         (user_id, authz_version, state, attempts, available_at, created_at) \
         VALUES (?, ?, 'pending', 0, UNIX_TIMESTAMP(), UNIX_TIMESTAMP())",
    )
    .bind(user_id)
    .bind(authz_version)
    .execute(executor(transaction)?)
    .await
    .map_err(yang_db::DbError::from)?;
    Ok(())
}

/// 按已锁定顺序递增所有受影响用户，同一用户在调用前已经去重。
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

fn executor(transaction: &mut Transaction) -> Result<&mut sqlx::MySqlConnection, BaseError> {
    transaction.executor().ok_or_else(|| {
        BaseError::from(yang_db::DbError::TransactionError(
            "授权 writer 事务已结束".to_string(),
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

    #[test]
    fn authorization_users_are_locked_once_in_stable_order() {
        assert_eq!(stable_user_ids([9, 3, 9, 5, 3]), [3, 5, 9]);
    }
}

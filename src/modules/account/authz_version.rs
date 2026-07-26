//! 授权事实 writer 共享的用户版本锁与递增原语。

use yang_base::BaseError;
use yang_db::Transaction;

/// 已在当前事务中锁定的用户授权状态。
pub(crate) struct LockedUserAuthorization {
    user_id: i64,
    status: String,
    authz_version: i64,
}

impl LockedUserAuthorization {
    /// 返回锁定的用户 ID。
    pub(crate) fn user_id(&self) -> i64 {
        self.user_id
    }

    /// 返回锁定时观察到的用户状态。
    pub(crate) fn status(&self) -> &str {
        &self.status
    }
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
        status,
        authz_version,
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

/// 在持有用户行锁的同一事务中递增版本，并返回新版本。
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
    Ok(next)
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
    fn authorization_users_are_locked_once_in_stable_order() {
        assert_eq!(stable_user_ids([9, 3, 9, 5, 3]), [3, 5, 9]);
    }
}

//! 密码重置凭证的生成、摘要与持久化仓储边界。
//! raw-sql-boundary: domain-repository password-reset-token

use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use std::fmt::Write;
use yang_base::BaseError;
use yang_db::Transaction;

const RAW_TOKEN_BYTES: usize = 32;
const RAW_TOKEN_CHARS: usize = RAW_TOKEN_BYTES * 2;
const FINGERPRINT_CHARS: usize = 16;

pub(crate) struct GeneratedPasswordReset {
    raw_token: String,
    reference: PasswordResetReference,
}

impl GeneratedPasswordReset {
    pub(crate) fn generate() -> Result<Self, BaseError> {
        let mut random = [0_u8; RAW_TOKEN_BYTES];
        OsRng
            .try_fill_bytes(&mut random)
            .map_err(|_| BaseError::Unknown("密码重置凭证随机源不可用".to_string()))?;
        let raw_token = encode_hex(&random)?;
        let reference = PasswordResetReference::from_bytes(&random)?;
        Ok(Self {
            raw_token,
            reference,
        })
    }

    pub(crate) fn raw_token(&self) -> &str {
        &self.raw_token
    }

    pub(crate) fn reference(&self) -> &PasswordResetReference {
        &self.reference
    }
}

pub(crate) struct PasswordResetReference {
    digest: String,
    fingerprint: String,
}

impl PasswordResetReference {
    pub(crate) fn parse(raw_token: &str) -> Result<Self, BaseError> {
        let bytes = decode_hex(raw_token).ok_or_else(invalid_reset_token)?;
        Self::from_bytes(&bytes)
    }

    fn from_bytes(bytes: &[u8; RAW_TOKEN_BYTES]) -> Result<Self, BaseError> {
        let digest = encode_hex(&Sha256::digest(bytes))?;
        let fingerprint = digest
            .get(..FINGERPRINT_CHARS)
            .ok_or_else(|| BaseError::Unknown("密码重置凭证指纹生成失败".to_string()))?
            .to_string();
        Ok(Self {
            digest,
            fingerprint,
        })
    }

    pub(crate) fn attempt_fingerprint(raw_token: &str) -> Result<String, BaseError> {
        let digest = encode_hex(&Sha256::digest(raw_token.as_bytes()))?;
        digest
            .get(..FINGERPRINT_CHARS)
            .map(str::to_owned)
            .ok_or_else(|| BaseError::Unknown("密码重置尝试指纹生成失败".to_string()))
    }

    pub(crate) fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

pub(crate) struct LockedPasswordReset {
    id: i64,
    user_id: i64,
    expires_at: i64,
    consumed_at: Option<i64>,
    invalidated_at: Option<i64>,
    database_now: i64,
}

impl LockedPasswordReset {
    pub(crate) fn user_id(&self) -> i64 {
        self.user_id
    }

    pub(crate) fn is_usable(&self) -> bool {
        self.consumed_at.is_none()
            && self.invalidated_at.is_none()
            && self.expires_at > self.database_now
    }
}

pub(crate) async fn create_in_tx(
    transaction: &mut Transaction,
    target_user_id: i64,
    requested_by_user_id: i64,
    reset: &GeneratedPasswordReset,
    ttl_seconds: u64,
) -> Result<(), BaseError> {
    let ttl_seconds = i64::try_from(ttl_seconds)
        .map_err(|_| BaseError::ConfigError("密码重置 TTL 超出 MySQL 范围".to_string()))?;
    sqlx::query(
        "UPDATE password_reset_token SET invalidated_at = UNIX_TIMESTAMP() \
         WHERE user_user = ? AND consumed_at IS NULL AND invalidated_at IS NULL",
    )
    .bind(target_user_id)
    .execute(executor(transaction)?)
    .await
    .map_err(yang_db::DbError::from)?;
    sqlx::query(
        "INSERT INTO password_reset_token \
         (token_digest, token_fingerprint, user_user, requested_by_user, expires_at, created_at) \
         VALUES (?, ?, ?, ?, UNIX_TIMESTAMP() + ?, UNIX_TIMESTAMP())",
    )
    .bind(&reset.reference.digest)
    .bind(reset.reference.fingerprint())
    .bind(target_user_id)
    .bind(requested_by_user_id)
    .bind(ttl_seconds)
    .execute(executor(transaction)?)
    .await
    .map_err(yang_db::DbError::from)?;
    Ok(())
}

pub(crate) async fn find_target_user(
    pool: &sqlx::MySqlPool,
    reference: &PasswordResetReference,
) -> Result<Option<i64>, BaseError> {
    sqlx::query_scalar("SELECT user_user FROM password_reset_token WHERE token_digest = ? LIMIT 1")
        .bind(&reference.digest)
        .fetch_optional(pool)
        .await
        .map_err(yang_db::DbError::from)
        .map_err(BaseError::from)
}

pub(crate) async fn lock_in_tx(
    transaction: &mut Transaction,
    reference: &PasswordResetReference,
) -> Result<LockedPasswordReset, BaseError> {
    sqlx::query_as::<_, (i64, i64, i64, Option<i64>, Option<i64>, i64)>(
        "SELECT id, user_user, expires_at, consumed_at, invalidated_at, UNIX_TIMESTAMP() \
         FROM password_reset_token WHERE token_digest = ? FOR UPDATE",
    )
    .bind(&reference.digest)
    .fetch_optional(executor(transaction)?)
    .await
    .map_err(yang_db::DbError::from)?
    .map(
        |(id, user_id, expires_at, consumed_at, invalidated_at, database_now)| {
            LockedPasswordReset {
                id,
                user_id,
                expires_at,
                consumed_at,
                invalidated_at,
                database_now,
            }
        },
    )
    .ok_or_else(invalid_reset_token)
}

pub(crate) async fn consume_in_tx(
    transaction: &mut Transaction,
    locked: &LockedPasswordReset,
) -> Result<(), BaseError> {
    if !locked.is_usable() {
        return Err(invalid_reset_token());
    }
    let consumed = sqlx::query(
        "UPDATE password_reset_token SET consumed_at = UNIX_TIMESTAMP() \
         WHERE id = ? AND consumed_at IS NULL AND invalidated_at IS NULL \
           AND expires_at > UNIX_TIMESTAMP()",
    )
    .bind(locked.id)
    .execute(executor(transaction)?)
    .await
    .map_err(yang_db::DbError::from)?;
    if consumed.rows_affected() != 1 {
        return Err(invalid_reset_token());
    }
    sqlx::query(
        "UPDATE password_reset_token SET invalidated_at = UNIX_TIMESTAMP() \
         WHERE user_user = ? AND id <> ? AND consumed_at IS NULL AND invalidated_at IS NULL",
    )
    .bind(locked.user_id)
    .bind(locked.id)
    .execute(executor(transaction)?)
    .await
    .map_err(yang_db::DbError::from)?;
    Ok(())
}

pub(crate) fn invalid_reset_token() -> BaseError {
    BaseError::Unauthorized("密码重置凭证无效或已过期".to_string())
}

fn executor(transaction: &mut Transaction) -> Result<&mut sqlx::MySqlConnection, BaseError> {
    transaction.executor().ok_or_else(|| {
        BaseError::from(yang_db::DbError::TransactionError(
            "密码重置事务已结束".to_string(),
        ))
    })
}

fn encode_hex(bytes: &[u8]) -> Result<String, BaseError> {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}")
            .map_err(|_| BaseError::Unknown("密码重置凭证编码失败".to_string()))?;
    }
    Ok(encoded)
}

fn decode_hex(value: &str) -> Option<[u8; RAW_TOKEN_BYTES]> {
    if value.len() != RAW_TOKEN_CHARS || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut decoded = [0_u8; RAW_TOKEN_BYTES];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let text = std::str::from_utf8(pair).ok()?;
        decoded[index] = u8::from_str_radix(text, 16).ok()?;
    }
    Some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_token_is_high_entropy_shaped_and_database_value_is_only_a_digest() {
        let reset = GeneratedPasswordReset::generate()
            .unwrap_or_else(|error| panic!("应生成重置凭证: {error}"));
        assert_eq!(reset.raw_token().len(), RAW_TOKEN_CHARS);
        assert!(reset
            .raw_token()
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')));
        assert_ne!(reset.raw_token(), reset.reference.digest);
        assert_eq!(reset.reference.fingerprint().len(), FINGERPRINT_CHARS);
        let reparsed = PasswordResetReference::parse(reset.raw_token())
            .unwrap_or_else(|error| panic!("生成的凭证应可解析: {error}"));
        assert_eq!(reparsed.digest, reset.reference.digest);
        assert_eq!(reparsed.fingerprint, reset.reference.fingerprint);
    }

    #[test]
    fn malformed_and_expired_or_consumed_tokens_fail_closed() {
        for malformed in ["", "abcd", &"g".repeat(RAW_TOKEN_CHARS)] {
            assert!(PasswordResetReference::parse(malformed).is_err());
        }
        let usable = LockedPasswordReset {
            id: 1,
            user_id: 7,
            expires_at: 101,
            consumed_at: None,
            invalidated_at: None,
            database_now: 100,
        };
        assert!(usable.is_usable());
        assert!(!LockedPasswordReset {
            expires_at: 100,
            ..usable
        }
        .is_usable());
    }
}

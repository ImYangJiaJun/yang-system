use argon2::password_hash::{Error as PasswordHashError, PasswordHash, SaltString};
use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use rand_core::OsRng;
use yang_base::BaseError;

pub(super) fn hash(password: &str) -> Result<String, BaseError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|value| value.to_string())
        .map_err(|_| BaseError::Unknown("密码哈希失败".to_string()))
}

pub(super) fn verify(password: &str, encoded: &str) -> Result<bool, BaseError> {
    let parsed = PasswordHash::new(encoded)
        .map_err(|_| BaseError::Unknown("数据库中的密码哈希格式无效".to_string()))?;
    match Argon2::default().verify_password(password.as_bytes(), &parsed) {
        Ok(()) => Ok(true),
        Err(PasswordHashError::Password) => Ok(false),
        Err(_) => Err(BaseError::Unknown("密码校验失败".to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_hash_round_trip() {
        let encoded = hash("correct-horse-battery-staple")
            .unwrap_or_else(|error| panic!("密码应成功哈希: {error}"));
        assert!(verify("correct-horse-battery-staple", &encoded)
            .unwrap_or_else(|error| panic!("密码应成功校验: {error}")));
        assert!(!verify("wrong-password", &encoded)
            .unwrap_or_else(|error| panic!("错误密码应得到 false: {error}")));
        assert!(!encoded.contains("correct-horse-battery-staple"));
    }
}

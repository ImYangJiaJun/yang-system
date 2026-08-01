//! 密码哈希与校验的受控执行边界。

use argon2::password_hash::{Error as PasswordHashError, PasswordHash, SaltString};
use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use rand_core::OsRng;
use std::sync::Arc;
use tokio::sync::Semaphore;
use yang_base::BaseError;

#[derive(Clone)]
pub(super) struct PasswordEngine {
    permits: Arc<Semaphore>,
}

impl PasswordEngine {
    pub(super) fn new(max_concurrency: usize) -> Result<Self, BaseError> {
        if max_concurrency == 0 {
            return Err(BaseError::ConfigError(
                "security.argon2_max_concurrency 必须大于 0".to_string(),
            ));
        }
        Ok(Self {
            permits: Arc::new(Semaphore::new(max_concurrency)),
        })
    }

    pub(super) async fn hash(&self, password: &str) -> Result<String, BaseError> {
        let password = password.to_owned();
        self.run_blocking(move || {
            let salt = SaltString::generate(&mut OsRng);
            Argon2::default()
                .hash_password(password.as_bytes(), &salt)
                .map(|value| value.to_string())
                .map_err(|_| BaseError::Unknown("密码哈希失败".to_string()))
        })
        .await
    }

    pub(super) async fn verify(&self, password: &str, encoded: &str) -> Result<bool, BaseError> {
        let password = password.to_owned();
        let encoded = encoded.to_owned();
        self.run_blocking(move || {
            let parsed = PasswordHash::new(&encoded)
                .map_err(|_| BaseError::Unknown("数据库中的密码哈希格式无效".to_string()))?;
            match Argon2::default().verify_password(password.as_bytes(), &parsed) {
                Ok(()) => Ok(true),
                Err(PasswordHashError::Password) => Ok(false),
                Err(_) => Err(BaseError::Unknown("密码校验失败".to_string())),
            }
        })
        .await
    }

    async fn run_blocking<T>(
        &self,
        operation: impl FnOnce() -> Result<T, BaseError> + Send + 'static,
    ) -> Result<T, BaseError>
    where
        T: Send + 'static,
    {
        let permit = Arc::clone(&self.permits)
            .acquire_owned()
            .await
            .map_err(|_| BaseError::Unknown("密码执行器已关闭".to_string()))?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation()
        })
        .await
        .map_err(|error| BaseError::Unknown(format!("密码任务执行失败: {error}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn password_hash_round_trip() {
        let engine =
            PasswordEngine::new(1).unwrap_or_else(|error| panic!("密码执行器应构建成功: {error}"));
        let encoded = engine
            .hash("correct-horse-battery-staple")
            .await
            .unwrap_or_else(|error| panic!("密码应成功哈希: {error}"));

        assert!(engine
            .verify("correct-horse-battery-staple", &encoded)
            .await
            .unwrap_or_else(|error| panic!("密码应成功校验: {error}")));
        assert!(!engine
            .verify("wrong-password", &encoded)
            .await
            .unwrap_or_else(|error| panic!("错误密码应得到 false: {error}")));
        assert!(!encoded.contains("correct-horse-battery-staple"));
    }

    #[test]
    fn zero_concurrency_is_rejected() {
        assert!(matches!(
            PasswordEngine::new(0),
            Err(BaseError::ConfigError(message)) if message.contains("argon2_max_concurrency")
        ));
    }
}

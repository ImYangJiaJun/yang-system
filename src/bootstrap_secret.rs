//! 平台初始化一次性凭证的不可逆配置值对象。

use anyhow::{ensure, Context};
use argon2::password_hash::{PasswordHash, SaltString};
use argon2::{Argon2, PasswordHasher};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Deserializer};
use std::fmt::Write as _;

const ARGON2_VERSION: u32 = 19;
const MIN_MEMORY_KIB: u32 = 19_456;
const MAX_MEMORY_KIB: u32 = 262_144;
const MIN_ITERATIONS: u32 = 2;
const MAX_ITERATIONS: u32 = 10;
const MIN_PARALLELISM: u32 = 1;
const MAX_PARALLELISM: u32 = 4;
const MIN_SALT_BYTES: usize = 16;
const MIN_OUTPUT_BYTES: usize = 32;
const MAX_ENCODED_LENGTH: usize = 512;
const GENERATED_SECRET_BYTES: usize = 32;

/// 只持有 Argon2id PHC 摘要，不持有运维生成的原始 bootstrap secret。
#[derive(Clone, PartialEq, Eq)]
pub struct BootstrapSecretDigest(String);

impl BootstrapSecretDigest {
    /// 解析并校验用于一次性高权限初始化的摘要强度与资源边界。
    pub fn parse(encoded: impl Into<String>) -> anyhow::Result<Self> {
        let encoded = encoded.into();
        ensure!(
            !encoded.is_empty() && encoded.len() <= MAX_ENCODED_LENGTH,
            "bootstrap.secret_digest 长度无效"
        );
        let parsed = PasswordHash::new(&encoded)
            .map_err(|_| anyhow::anyhow!("bootstrap.secret_digest 不是合法 PHC 摘要"))?;
        ensure!(
            parsed.algorithm.as_str() == "argon2id",
            "bootstrap.secret_digest 必须使用 argon2id"
        );
        ensure!(
            parsed.version == Some(ARGON2_VERSION),
            "bootstrap.secret_digest 必须使用 Argon2 v={ARGON2_VERSION}"
        );
        ensure!(
            parsed.params.iter().count() == 3
                && parsed
                    .params
                    .iter()
                    .all(|(name, _)| matches!(name.as_str(), "m" | "t" | "p")),
            "bootstrap.secret_digest 只能包含 m、t、p 参数"
        );

        let memory = required_decimal(&parsed, "m")?;
        let iterations = required_decimal(&parsed, "t")?;
        let parallelism = required_decimal(&parsed, "p")?;
        ensure!(
            (MIN_MEMORY_KIB..=MAX_MEMORY_KIB).contains(&memory),
            "bootstrap.secret_digest 内存成本必须在 {MIN_MEMORY_KIB}..={MAX_MEMORY_KIB} KiB"
        );
        ensure!(
            (MIN_ITERATIONS..=MAX_ITERATIONS).contains(&iterations),
            "bootstrap.secret_digest 迭代次数必须在 {MIN_ITERATIONS}..={MAX_ITERATIONS}"
        );
        ensure!(
            (MIN_PARALLELISM..=MAX_PARALLELISM).contains(&parallelism),
            "bootstrap.secret_digest 并行度必须在 {MIN_PARALLELISM}..={MAX_PARALLELISM}"
        );

        let salt = parsed.salt.context("bootstrap.secret_digest 缺少 salt")?;
        let mut salt_buffer = [0_u8; 64];
        let salt = salt
            .decode_b64(&mut salt_buffer)
            .map_err(|_| anyhow::anyhow!("bootstrap.secret_digest salt 编码无效"))?;
        ensure!(
            salt.len() >= MIN_SALT_BYTES,
            "bootstrap.secret_digest salt 至少需要 {MIN_SALT_BYTES} 字节"
        );
        let output = parsed
            .hash
            .context("bootstrap.secret_digest 缺少摘要输出")?;
        ensure!(
            output.len() >= MIN_OUTPUT_BYTES,
            "bootstrap.secret_digest 输出至少需要 {MIN_OUTPUT_BYTES} 字节"
        );

        Ok(Self(encoded))
    }

    /// 返回用于配置持久化或后续常量时间校验的 PHC 摘要。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for BootstrapSecretDigest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("BootstrapSecretDigest([REDACTED])")
    }
}

impl<'de> Deserialize<'de> for BootstrapSecretDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        Self::parse(encoded).map_err(serde::de::Error::custom)
    }
}

/// 本地/运维工具一次性生成的原始 secret 与其不可逆摘要。
///
/// 应只展示原始 secret 一次，并只把 `digest` 写入应用配置。
pub struct GeneratedBootstrapSecret {
    secret: String,
    digest: BootstrapSecretDigest,
}

impl GeneratedBootstrapSecret {
    pub fn secret(&self) -> &str {
        &self.secret
    }

    pub fn digest(&self) -> &BootstrapSecretDigest {
        &self.digest
    }
}

impl std::fmt::Debug for GeneratedBootstrapSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("GeneratedBootstrapSecret([REDACTED])")
    }
}

/// 生成 256 bit 随机一次性 secret 及使用当前强度下限编码的 Argon2id 摘要。
pub fn generate_bootstrap_secret() -> anyhow::Result<GeneratedBootstrapSecret> {
    let mut random = [0_u8; GENERATED_SECRET_BYTES];
    OsRng.fill_bytes(&mut random);
    let mut secret = String::with_capacity(GENERATED_SECRET_BYTES * 2);
    for byte in random {
        write!(&mut secret, "{byte:02x}").context("编码 bootstrap secret 失败")?;
    }
    let salt = SaltString::generate(&mut OsRng);
    let encoded = Argon2::default()
        .hash_password(secret.as_bytes(), &salt)
        .map_err(|_| anyhow::anyhow!("生成 bootstrap secret 摘要失败"))?
        .to_string();
    let digest = BootstrapSecretDigest::parse(encoded)?;
    Ok(GeneratedBootstrapSecret { secret, digest })
}

fn required_decimal(hash: &PasswordHash<'_>, name: &str) -> anyhow::Result<u32> {
    hash.params
        .get_decimal(name)
        .with_context(|| format!("bootstrap.secret_digest 缺少或错误的 {name} 参数"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_secret_is_high_entropy_shaped_and_never_debugged() {
        let generated = generate_bootstrap_secret()
            .unwrap_or_else(|error| panic!("bootstrap secret 应生成成功: {error:#}"));
        assert_eq!(generated.secret().len(), GENERATED_SECRET_BYTES * 2);
        assert!(generated
            .secret()
            .bytes()
            .all(|value| value.is_ascii_hexdigit()));
        assert!(generated.digest().as_str().starts_with("$argon2id$"));
        let debug = format!("{generated:?}");
        assert!(!debug.contains(generated.secret()));
        assert!(!debug.contains(generated.digest().as_str()));
    }
}

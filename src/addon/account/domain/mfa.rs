//! TOTP 第二因子的领域机制（路线图 E-1b/E-1c）。
//!
//! 只承载三类共享机制：
//! - AEAD 加密/解密 `users.totp_secret`（独立密钥域 `security.totp.aead_key`，
//!   启动校验保证不与 token/step-up/验证码密钥复用）；
//! - TOTP 共享密钥与 `otpauth://` URI 的生成（RFC 6238 算法本体由
//!   `yang_base::action::auth::TotpLiteVerifier` 提供，此处只做密钥与标签）；
//! - 一次性恢复码的生成与摘要校验（摘要入库、单次消费由 Action 层保证）。
//!
//! 校验器统一走框架端口 [`TotpVerifier`]，不在此重复实现算法。

use crate::config::TotpSettings;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use yang_base::BaseError;

/// AEAD 随机 nonce 长度（AES-GCM 标准 12 字节）。
const NONCE_LEN: usize = 12;
/// TOTP 共享密钥的 Base32 长度（160 bit -> 32 字符）。
const TOTP_SECRET_BASE32_LEN: usize = 32;
/// 恢复码安全随机长度（16 字节 → 4 组 4 字符）。
const RECOVERY_CODE_BYTES: usize = 16;
/// 每次激活签发的恢复码数量。
pub(crate) const RECOVERY_CODE_COUNT: usize = 8;

/// 由 `security.totp` 配置构造的 AEAD 加密器。
///
/// 密钥取配置原文的 SHA-256 摘要（32 字节），避免对 hex/base64/原文的
/// 形态猜测；摘要化后配置值本身不直接成为密钥材料，但**配置必须保持
/// 独立**——密钥域隔离的保证在配置校验层。
pub(crate) struct TotpSecretCipher {
    cipher: Aes256Gcm,
}

impl TotpSecretCipher {
    pub(crate) fn new(settings: &TotpSettings) -> Result<Self, BaseError> {
        let key = Sha256::digest(settings.aead_key.as_bytes());
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| {
            BaseError::ConfigError("security.totp.aead_key 无法构造 AES-256-GCM".to_string())
        })?;
        Ok(Self { cipher })
    }

    /// 加密 TOTP secret；输出 `nonce || ciphertext`（Base64），便于单列存储。
    pub(crate) fn encrypt(&self, plaintext: &[u8]) -> Result<String, BaseError> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext)
            .map_err(|_| BaseError::ConfigError("TOTP secret 加密失败".to_string()))?;
        let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ciphertext);
        Ok(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            blob,
        ))
    }

    /// 解密 `encrypt` 的输出；失败按配置损坏处理。
    pub(crate) fn decrypt(&self, stored: &str) -> Result<Vec<u8>, BaseError> {
        let blob = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, stored)
            .map_err(|_| BaseError::ConfigError("TOTP secret 存储损坏（非 Base64）".to_string()))?;
        if blob.len() < NONCE_LEN + 1 {
            return Err(BaseError::ConfigError(
                "TOTP secret 存储损坏（长度不足）".to_string(),
            ));
        }
        let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);
        let plaintext = self
            .cipher
            .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
            .map_err(|_| {
                BaseError::ConfigError("TOTP secret 解密失败（密钥域不匹配？）".to_string())
            })?;
        Ok(plaintext)
    }
}

/// 生成 RFC 4648 Base32 编码的 160-bit TOTP 共享密钥（不带填充）。
pub(crate) fn generate_totp_secret() -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut bytes = [0u8; 20];
    OsRng.fill_bytes(&mut bytes);
    // 20 字节 → 160 bit → 32 个 Base32 字符；u64 缓冲足够承载任意 5-bit 分组。
    let mut out = String::with_capacity(TOTP_SECRET_BASE32_LEN);
    let mut buffer: u64 = 0;
    let mut bits: u32 = 0;
    for byte in bytes {
        buffer = (buffer << 8) | byte as u64;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 0x1F) as usize] as char);
        }
    }
    // 最后不足 8 位的余数用 0 填充（Base32 标准 padding 语义）。
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 0x1F) as usize] as char);
    }
    debug_assert_eq!(out.len(), TOTP_SECRET_BASE32_LEN);
    out
}

/// 构造 `otpauth://totp/<issuer>:<account>?secret=...&issuer=...&algorithm=SHA256&digits=...&period=30`。
pub(crate) fn otpauth_uri(issuer: &str, account: &str, secret: &str, digits: u32) -> String {
    let issuer_enc: String = issuer
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let account_enc: String = account
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!(
        "otpauth://totp/{issuer_enc}:{account_enc}?secret={secret}&issuer={issuer_enc}&algorithm=SHA256&digits={digits}&period=30"
    )
}

/// 生成一组一次性恢复码（明文），并返回（明文组、各组的 SHA-256 摘要组）。
///
/// 摘要入库；明文只在激活响应中回显一次。消费时按摘要匹配并单次作废
/// （Action 层保证）。
pub(crate) fn generate_recovery_codes() -> (Vec<String>, Vec<String>) {
    let mut plains = Vec::with_capacity(RECOVERY_CODE_COUNT);
    let mut digests = Vec::with_capacity(RECOVERY_CODE_COUNT);
    for _ in 0..RECOVERY_CODE_COUNT {
        let mut bytes = [0u8; RECOVERY_CODE_BYTES];
        OsRng.fill_bytes(&mut bytes);
        // 16 字节 → 32 hex 字符 → 4 组 8 字符，用连字符分组提升可读性。
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let mut grouped = String::with_capacity(35);
        for (i, chunk) in hex.as_bytes().chunks(8).enumerate() {
            if i > 0 {
                grouped.push('-');
            }
            grouped.push_str(std::str::from_utf8(chunk).unwrap_or(""));
        }
        plains.push(grouped.clone());
        digests.push(hex_digest(grouped.as_bytes()));
    }
    (plains, digests)
}

/// 对恢复码做 SHA-256 摘要（hex），用于与库内摘要比对。
pub(crate) fn hex_digest(input: &[u8]) -> String {
    let digest = Sha256::digest(input);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// 判断一次性恢复码是否命中摘要 JSON 数组（`["hex", ...]`）。
/// 命中即视为一次有效消费候选；**单次消费**（移除）由调用方在事务内保证。
pub(crate) fn recovery_digest_matches(digests_json: &str, code: &str) -> bool {
    let Ok(digests) = serde_json::from_str::<Vec<String>>(digests_json) else {
        return false;
    };
    let digest = hex_digest(code.as_bytes());
    digests.iter().any(|candidate| candidate == &digest)
}

/// 从摘要 JSON 数组中移除命中项；无命中时原样返回。
pub(crate) fn remove_recovery_digest(digests_json: &str, code: &str) -> Option<String> {
    let mut digests: Vec<String> = serde_json::from_str(digests_json).ok()?;
    let digest = hex_digest(code.as_bytes());
    let before = digests.len();
    digests.retain(|candidate| candidate != &digest);
    if digests.len() == before {
        return None;
    }
    serde_json::to_string(&digests).ok()
}

#[cfg(test)]
mod __tests__ {
    use super::*;

    fn test_settings() -> TotpSettings {
        TotpSettings {
            aead_key: "test-aead-key-0123456789abcdef-0123456789".to_string(),
            digits: 6,
        }
    }

    #[test]
    fn cipher_round_trip() -> anyhow::Result<()> {
        let cipher = TotpSecretCipher::new(&test_settings())?;
        let secret = generate_totp_secret();
        let stored = cipher.encrypt(secret.as_bytes())?;
        let decrypted = cipher.decrypt(&stored)?;
        assert_eq!(String::from_utf8(decrypted)?, secret);
        Ok(())
    }

    #[test]
    fn cipher_rejects_wrong_key_domain() -> anyhow::Result<()> {
        let cipher = TotpSecretCipher::new(&test_settings())?;
        let stored = cipher.encrypt(b"secret-material")?;
        let wrong = TotpSecretCipher::new(&TotpSettings {
            aead_key: "another-key-0123456789abcdef-0123456789".to_string(),
            digits: 6,
        })?;
        assert!(wrong.decrypt(&stored).is_err());
        Ok(())
    }

    #[test]
    fn secret_is_valid_base32_and_length() {
        let secret = generate_totp_secret();
        assert_eq!(secret.len(), 32, "实际长度 {}", secret.len());
        assert!(
            secret
                .bytes()
                .all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b)),
            "含非法 Base32 字符: {secret}"
        );
    }

    #[test]
    fn otpauth_uri_shape() {
        let uri = otpauth_uri("yang-system", "alice@example.com", "ABC234", 6);
        assert!(
            uri.starts_with("otpauth://totp/yang-system:alice@example.com?"),
            "实际 URI: {uri}"
        );
        assert!(uri.contains("secret=ABC234"));
        assert!(uri.contains("algorithm=SHA256&digits=6&period=30"));
        // 标签中的非法字符被替换
        let uri = otpauth_uri("yang system", "alice email", "ABC234", 6);
        assert!(uri.contains("yang_system:alice_email"));
    }

    fn plausible(code: &str) -> bool {
        let mut hex_chars = 0;
        let mut groups = 0;
        for (i, part) in code.split('-').enumerate() {
            if i > 0 {
                groups += 1;
            }
            if part.len() != 8 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
                return false;
            }
            hex_chars += 8;
        }
        groups == 3 && hex_chars == 32
    }

    #[test]
    fn recovery_codes_round_trip() {
        let (plains, digests) = generate_recovery_codes();
        assert_eq!(plains.len(), RECOVERY_CODE_COUNT);
        assert_eq!(digests.len(), RECOVERY_CODE_COUNT);
        for (plain, digest) in plains.iter().zip(digests.iter()) {
            assert!(plausible(plain));
            assert_eq!(*digest, hex_digest(plain.as_bytes()));
        }
    }
}

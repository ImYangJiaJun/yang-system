//! 飞书外部选项接口的 AES-256-CBC 加解密。
//!
//! 与飞书官方文档的 Go 参考实现保持一致：密钥取配置原文的 SHA-256 摘要；
//! IV 为 16 字节随机值并**前置**拼接到密文；填充为 PKCS#7（长度已对齐时仍追加整块）；
//! 输出为 `base64(IV ‖ 密文)`。
//!
//! # 只提供加密方向
//!
//! 飞书来文是明文，本服务从不解密外部输入，因此不存在 padding oracle 暴露面。
//! CBC 本身不提供完整性保护，这是飞书规范的缺口——自行追加 HMAC 会导致飞书无法解密，
//! 故不加。
//!

use aes::cipher::block_padding::Pkcs7;
use aes::cipher::{BlockEncryptMut, KeyIvInit};
// 只在测试用的解密辅助函数里需要；生产路径不解密
#[cfg(test)]
use aes::cipher::BlockDecryptMut;
use base64::Engine;
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use yang_base::BaseError;

/// AES 块大小（字节）。
const BLOCK_SIZE: usize = 16;

/// base64 STANDARD 编码器。
const BASE64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
#[cfg(test)]
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

/// 由配置原文派生 256 位密钥。
///
/// 取 SHA-256 摘要而非直接使用原文，避免对配置值的形态（hex / base64 / UTF-8）做猜测。
pub(crate) fn derive_key(raw: &str) -> [u8; 32] {
    let digest = Sha256::digest(raw.as_bytes());
    let mut key = [0u8; 32];
    key.copy_from_slice(&digest);
    key
}

/// 加密字节串，返回 `base64(IV ‖ 密文)`。
///
/// 填充为 PKCS#7：**长度已对齐时仍追加整块 16 字节**，与飞书 Go 参考实现的
/// `standardizeDataEn`（`appendingLen = 16 - len % 16`，对齐时为 16）一致。
///
/// # 错误
///
/// 填充空间不足时返回 [`BaseError::ConfigError`]。
pub(crate) fn encrypt_bytes(plaintext: &[u8], key: &[u8; 32]) -> Result<String, BaseError> {
    let mut iv = [0u8; BLOCK_SIZE];
    OsRng.fill_bytes(&mut iv);

    // 预留一个整块：PKCS#7 在明文已对齐时还要再追加整块
    let mut buffer = vec![0u8; plaintext.len() + BLOCK_SIZE];
    buffer[..plaintext.len()].copy_from_slice(plaintext);

    let cipher = Aes256CbcEnc::new(key.into(), (&iv).into());
    let ciphertext = cipher
        .encrypt_padded_mut::<Pkcs7>(&mut buffer, plaintext.len())
        .map_err(|_| BaseError::ConfigError("AES-CBC 加密填充失败".to_string()))?;

    let mut output = Vec::with_capacity(BLOCK_SIZE + ciphertext.len());
    output.extend_from_slice(&iv);
    output.extend_from_slice(ciphertext);
    Ok(BASE64.encode(output))
}

/// 加密一个 JSON 值，返回 `base64(IV ‖ 密文)`。
///
/// 被加密的是 `data.result` 的**内容**（选项与国际化文案），不含外层 `result` 包装。
///
/// # 错误
///
/// 序列化失败或填充失败时返回 [`BaseError`]。
pub(crate) fn encrypt_json(value: &serde_json::Value, key: &[u8; 32]) -> Result<String, BaseError> {
    let plaintext = serde_json::to_vec(value)
        .map_err(|error| BaseError::JsonSerializeFailed(error.to_string()))?;
    encrypt_bytes(&plaintext, key)
}

/// 解密 CBC 密文但**不剥除填充**。
///
/// 仅供测试使用：用来验证产出的填充确实是 PKCS#7（剥掉填充后能拿回原文），
/// 生产路径从不解密外部输入，因此没有对应的生产函数。
#[cfg(test)]
pub(crate) fn decrypt_cbc(
    ciphertext: &[u8],
    iv: &[u8],
    key: &[u8; 32],
) -> Result<Vec<u8>, BaseError> {
    if ciphertext.is_empty() || ciphertext.len() % BLOCK_SIZE != 0 {
        return Err(BaseError::ConfigError(
            "AES-CBC 密文长度必须是块大小的整数倍".to_string(),
        ));
    }
    if iv.len() != BLOCK_SIZE {
        return Err(BaseError::ConfigError(
            "AES-CBC IV 长度必须是 16".to_string(),
        ));
    }
    let iv: [u8; BLOCK_SIZE] = iv
        .try_into()
        .map_err(|_| BaseError::ConfigError("AES-CBC IV 长度非法".to_string()))?;

    let mut buffer = ciphertext.to_vec();
    let cipher = Aes256CbcDec::new(key.into(), (&iv).into());
    // NoPadding：只解密，不剥填充，便于测试直接检查填充字节
    let decrypted = cipher
        .decrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut buffer)
        .map_err(|_| BaseError::ConfigError("AES-CBC 解密失败".to_string()))?;
    Ok(decrypted.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;

    /// 已知明文 `sha256("a-test-key")` 的字节形式。
    const EXPECTED_KEY_HEX: &str =
        "bd01d31bd0851d44aa6fcaae3317a20e450cc16929f33babc79222ad7ca9a952";

    fn expected_key() -> [u8; 32] {
        let mut key = [0u8; 32];
        for (index, chunk) in EXPECTED_KEY_HEX.as_bytes().chunks(2).enumerate() {
            let text = std::str::from_utf8(chunk)
                .unwrap_or_else(|error| panic!("hex 应是 UTF-8: {error}"));
            key[index] = u8::from_str_radix(text, 16)
                .unwrap_or_else(|error| panic!("应是合法 hex: {error}"));
        }
        key
    }

    fn decode(cipher_base64: &str) -> Vec<u8> {
        STANDARD
            .decode(cipher_base64)
            .unwrap_or_else(|error| panic!("应是合法 base64: {error}"))
    }

    #[test]
    fn key_derivation_is_sha256_of_raw_text() {
        // 密钥取配置原文的 SHA-256 摘要，避免对 hex/base64/原文的形态猜测
        assert_eq!(derive_key("a-test-key"), expected_key());
    }

    #[test]
    fn aligned_plaintext_gets_a_whole_extra_block() {
        // 飞书 Go 参考实现的 standardizeDataEn 在明文长度已对齐时仍追加整块 16 字节。
        // 这里通过可观测的密文长度把它钉死：16 字节明文 -> 16(IV) + 32(两个块)
        let cipher = encrypt_bytes(&[7u8; 16], &expected_key())
            .unwrap_or_else(|error| panic!("加密应成功: {error}"));
        assert_eq!(
            decode(&cipher).len(),
            16 + 32,
            "对齐输入必须多出一个整块，否则与飞书参考实现不一致"
        );
    }

    #[test]
    fn partial_plaintext_is_padded_to_the_next_block() {
        // 20 字节明文 -> 补到 32 字节 -> 16(IV) + 32
        let cipher = encrypt_bytes(&[7u8; 20], &expected_key())
            .unwrap_or_else(|error| panic!("加密应成功: {error}"));
        assert_eq!(decode(&cipher).len(), 16 + 32);
    }

    #[test]
    fn ciphertext_is_iv_prefixed_and_block_aligned() {
        let cipher = encrypt_bytes(b"hello feishu", &expected_key())
            .unwrap_or_else(|error| panic!("加密应成功: {error}"));
        let raw = decode(&cipher);
        assert!(raw.len() > 16, "至少含 IV 与一个密文块");
        assert_eq!((raw.len() - 16) % 16, 0, "去 IV 后必须是 16 的整数倍");
    }

    #[test]
    fn random_iv_makes_every_ciphertext_different() {
        // 每次使用新的随机 IV：这是 CBC 的安全要求，也证明 IV 不是固定值
        let key = expected_key();
        let first = encrypt_bytes(b"same plaintext", &key)
            .unwrap_or_else(|error| panic!("加密应成功: {error}"));
        let second = encrypt_bytes(b"same plaintext", &key)
            .unwrap_or_else(|error| panic!("加密应成功: {error}"));
        assert_ne!(first, second, "两次加密必须因随机 IV 而不同");
    }

    #[test]
    fn round_trip_recovers_plaintext_and_pkcs7_padding() {
        // 用同一密钥解密，验证填充确实是 PKCS#7（否则飞书解不开）
        for plaintext in [vec![0u8; 16], vec![1u8; 20], b"hello feishu".to_vec()] {
            let key = expected_key();
            let cipher = encrypt_bytes(&plaintext, &key)
                .unwrap_or_else(|error| panic!("加密应成功: {error}"));
            let raw = decode(&cipher);
            let (iv, body) = raw.split_at(16);
            let decrypted =
                decrypt_cbc(body, iv, &key).unwrap_or_else(|error| panic!("解密应成功: {error}"));
            let padding = *decrypted
                .last()
                .unwrap_or_else(|| panic!("解密结果不应为空")) as usize;
            assert!(
                (1..=16).contains(&padding),
                "PKCS#7 填充字节必须在 1..=16，实际 {padding}"
            );
            assert_eq!(decrypted.len() % 16, 0, "解密结果必须是整块");
            let recovered = &decrypted[..decrypted.len() - padding];
            assert_eq!(recovered, plaintext.as_slice());
        }
    }

    #[test]
    fn json_wrapper_matches_bytes_wrapper_shape() {
        // encrypt_json 只是把值序列化后再走 encrypt_bytes，形状必须一致
        let key = expected_key();
        let cipher = encrypt_json(&serde_json::json!({"result": "x"}), &key)
            .unwrap_or_else(|error| panic!("加密应成功: {error}"));
        let raw = decode(&cipher);
        assert!(raw.len() > 16);
        assert_eq!((raw.len() - 16) % 16, 0);
    }
}

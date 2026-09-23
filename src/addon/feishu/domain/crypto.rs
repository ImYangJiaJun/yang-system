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
use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use base64::Engine;
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use yang_base::BaseError;

use super::token::hash_token;

/// AES 块大小（字节）。
const BLOCK_SIZE: usize = 16;

/// base64 STANDARD 编码器。
const BASE64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
// 消费者（凭据回显端点）在后续批次接入。仓库既有先例：
// `domain/bitable.rs:29`、`domain/outbound.rs:30`。
#[allow(dead_code)]
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

/// 解密 `base64(IV ‖ 密文)`，剥除 PKCS#7 填充。
///
/// 与 [`encrypt_bytes`] 严格对称：IV 前置、PKCS#7。**只用于解本服务自己封存的凭据**，
/// 不用于解任何外部输入（飞书来文是明文）。
#[allow(dead_code)]
fn decrypt_bytes(encoded: &str, key: &[u8; 32]) -> Result<Vec<u8>, BaseError> {
    let raw = BASE64
        .decode(encoded)
        .map_err(|_| BaseError::ConfigError("凭据密文不是合法 base64".to_string()))?;
    // 至少要有一个 IV 块 + 一个密文块（PKCS#7 保证密文段非空且是整块）
    if raw.len() <= BLOCK_SIZE || (raw.len() - BLOCK_SIZE) % BLOCK_SIZE != 0 {
        return Err(BaseError::ConfigError("凭据密文长度非法".to_string()));
    }
    let (iv, ciphertext) = raw.split_at(BLOCK_SIZE);
    let iv: [u8; BLOCK_SIZE] = iv
        .try_into()
        .map_err(|_| BaseError::ConfigError("凭据密文 IV 长度非法".to_string()))?;

    let mut buffer = ciphertext.to_vec();
    let cipher = Aes256CbcDec::new(key.into(), (&iv).into());
    // Pkcs7：解密并校验/剥除填充。填充不对（含换钥匙）会在这里失败，
    // 而不是吐出一段垃圾明文——这是「换钥匙必须显式失败」的落实点。
    let plain = cipher
        .decrypt_padded_mut::<Pkcs7>(&mut buffer)
        .map_err(|_| BaseError::ConfigError("凭据密文解密失败".to_string()))?;
    Ok(plain.to_vec())
}

/// 封存一份凭据：返回 `(摘要, 密文)`。
///
/// 两份产物分工不同、都不能省：
/// - **摘要**供校验路径做比对，**校验时不需要解密**；
/// - **密文**只在「回显给运维复制」这一条路径上解密。
///
/// 解密面被收敛在一个端点里，这是刻意的。
pub(crate) fn seal(plaintext: &str, key: &[u8; 32]) -> Result<(String, String), BaseError> {
    Ok((
        hash_token(plaintext),
        encrypt_bytes(plaintext.as_bytes(), key)?,
    ))
}

/// 生成一份系统凭据。
///
/// 32 字节密码学随机源（`OsRng`）的 hex，共 64 字符。飞书对 Token/Key 的**格式不限**
/// （《关联外部选项》：「参数格式不限，与飞书审批中心表单设计中填写的 Token、Key
/// 一致即可」），所以取定长 hex 只为可读与可复制，不是为了满足什么校验。
pub(crate) fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 取出封存的凭据明文（低层，**不做完整性校验**）。
///
/// 调用方几乎总是该用 [`unseal_verified`]：CBC 没有完整性保护，本函数对
/// 「密文被篡改」「拿错了钥匙」「贴错了行」都**不保证**失败。
///
/// # 错误
///
/// 密文非法、填充不合法、明文不是合法 UTF-8 时返回 [`BaseError`]。
#[allow(dead_code)]
pub(crate) fn unseal(cipher: &str, key: &[u8; 32]) -> Result<String, BaseError> {
    let bytes = decrypt_bytes(cipher, key)?;
    String::from_utf8(bytes)
        .map_err(|_| BaseError::ConfigError("凭据明文不是合法 UTF-8".to_string()))
}

/// 解封并**用同一条记录上的摘要校验**。这是生产路径该用的那个。
///
/// # 为什么要摘要校验
///
/// CBC 只保密、不保证完整性：填充校验是概率性的（错钥匙下约 1/256 恰好合法），
/// 而且改 IV 只会污染第一段明文、填充仍然完好。把 `token_hash` 当校验和是随手可得
/// 的正解——它本来就在同一条记录上，且校验路径**不需要解密**。
///
/// 于是三类事故都会变成确定性失败：篡改密文、拿错钥匙、把 A 行的密文贴到 B 行。
#[allow(dead_code)]
pub(crate) fn unseal_verified(
    cipher: &str,
    expected_hash: &str,
    key: &[u8; 32],
) -> Result<String, BaseError> {
    let plaintext = unseal(cipher, key)?;
    if !super::token::verify_token(&plaintext, expected_hash) {
        return Err(BaseError::ConfigError(
            "凭据密文与摘要不符（密文被篡改、钥匙不对，或密文与摘要不同源）".to_string(),
        ));
    }
    Ok(plaintext)
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

    // ---- 凭据封存（seal / unseal）----

    fn key() -> [u8; 32] {
        derive_key("test-credential-wrapping-key")
    }

    fn sealed(plaintext: &str) -> (String, String) {
        seal(plaintext, &key()).unwrap_or_else(|error| panic!("应可封存: {error}"))
    }

    #[test]
    fn generated_tokens_are_64_hex_chars_and_do_not_repeat() {
        let first = generate_token();
        let second = generate_token();
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(first, second, "两次生成不得相同");
    }

    #[test]
    fn unseal_returns_the_same_plaintext_that_was_sealed() {
        // 「一直可以复制同一个值」是硬需求：回显必须是**解密出原值**，不是现场新生成
        let (hash, cipher) = sealed("s3cret-token");
        assert_eq!(
            unseal_verified(&cipher, &hash, &key()).unwrap_or_else(|e| panic!("应可解封: {e}")),
            "s3cret-token"
        );
        assert_eq!(hash.len(), 64, "摘要是 64 位 hex");
    }

    #[test]
    fn sealing_the_same_plaintext_twice_yields_a_different_ciphertext() {
        // IV 每次随机：同样的明文不能产生同样的密文，否则「这两条是同一份凭据」可被读出
        let (_, first) = sealed("t");
        let (_, second) = sealed("t");
        assert_ne!(first, second);
    }

    #[test]
    fn the_hash_is_stable_for_the_same_plaintext() {
        // 摘要用于校验，必须稳定——否则同一份凭据每轮都验不过
        assert_eq!(sealed("t").0, sealed("t").0);
    }

    #[test]
    fn unseal_verified_rejects_the_wrong_key_deterministically() {
        // CBC 没有完整性保护，单靠 PKCS#7 填充校验识别篡改是**概率性**的
        // （错钥匙下填充约 1/256 恰好合法）。摘要校验把它变成确定性的失败。
        let (hash, cipher) = sealed("t");
        let other = derive_key("a-different-key");
        assert!(
            unseal_verified(&cipher, &hash, &other).is_err(),
            "换钥匙必须失败"
        );
    }

    #[test]
    fn unseal_verified_rejects_a_tampered_ciphertext_deterministically() {
        let (hash, cipher) = sealed("t");
        let mut raw = BASE64.decode(&cipher).unwrap_or_default();
        // 改 IV：只污染第一段明文、填充仍然完好——单靠填充校验**发现不了**
        raw[0] ^= 0xFF;
        let tampered = BASE64.encode(raw);
        assert!(
            unseal_verified(&tampered, &hash, &key()).is_err(),
            "摘要校验必须挡住这种篡改"
        );
    }

    #[test]
    fn unseal_verified_rejects_a_cipher_from_another_record() {
        // 把 A 行的密文贴到 B 行：密文本身完全合法，只有摘要比对能发现
        let (_, cipher_a) = sealed("token-a");
        let (hash_b, _) = sealed("token-b");
        assert!(
            unseal_verified(&cipher_a, &hash_b, &key()).is_err(),
            "密文与摘要必须同源"
        );
    }

    #[test]
    fn unseal_rejects_a_malformed_ciphertext() {
        assert!(unseal("not-base64!!", &key()).is_err());
        assert!(unseal("", &key()).is_err());
        // 不足一个 IV 块
        assert!(unseal(&BASE64.encode([0u8; 8]), &key()).is_err());
        // 密文段不是块大小的整数倍
        assert!(unseal(&BASE64.encode(vec![0u8; BLOCK_SIZE + 7]), &key()).is_err());
    }

    #[test]
    fn multi_byte_plaintext_survives_a_round_trip() {
        // 凭据是随机字节的 hex/base64，但仍要能承载中文与 emoji 而不损坏
        let plaintext = "凭据-🔑-token";
        let (_, cipher) = seal(plaintext, &key()).unwrap_or_else(|e| panic!("应可封存: {e}"));
        assert_eq!(
            unseal(&cipher, &key()).unwrap_or_else(|e| panic!("应可解封: {e}")),
            plaintext
        );
    }

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

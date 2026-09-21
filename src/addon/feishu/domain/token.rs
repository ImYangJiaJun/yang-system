//! 数据源 Token 的哈希与常数时间比较。
//!
//! 数据源 Token 用于校验「请求是否来自配置了这个 Token 的那个数据源」，
//! **只以 SHA-256 摘要入库**，永不存明文；比较走常数时间，避免通过响应时间侧信道
//! 逐字节猜测。
//!

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// SHA-256 十六进制摘要的长度。
const HASH_HEX_LEN: usize = 64;

/// 计算 token 的 SHA-256 摘要，返回 64 字符小写 hex。
///
/// 数据库里存的是这个值而不是明文——本服务只需要**校验** token，永远不需要**出示**它，
/// 因此没有必要保留可还原的形态。
pub(crate) fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut hex = String::with_capacity(HASH_HEX_LEN);
    // 仓库既有惯例是手写 hex 编码表，不引入 hex crate
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        hex.push(HEX[(byte >> 4) as usize] as char);
        hex.push(HEX[(byte & 0x0f) as usize] as char);
    }
    hex
}

/// 常数时间校验 token 是否匹配库存摘要。
///
/// 库存摘要先做归一化（去首尾空白、转小写），再从 secret 文件读入的值也能直接用。
/// 归一化后不是合法的 64 位 hex 时一律返回 `false`（fail-closed），不 panic——
/// 一个被误写的库存值应当拒绝所有请求，而不是让服务崩溃或放行。
pub(crate) fn verify_token(presented: &str, stored_hash: &str) -> bool {
    let normalized = stored_hash.trim().to_ascii_lowercase();
    if normalized.len() != HASH_HEX_LEN || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return false;
    }
    let computed = hash_token(presented);
    // 用常数时间比较，避免通过响应时间逐字节推断摘要
    computed.as_bytes().ct_eq(normalized.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_lowercase_hex_sha256() {
        // sha256("abc") 的已知值，钉死编码形态（小写 hex，64 字符）
        assert_eq!(
            hash_token("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hash_is_stable_across_calls() {
        assert_eq!(hash_token("same"), hash_token("same"));
        assert_ne!(hash_token("a"), hash_token("b"));
    }

    #[test]
    fn verify_accepts_matching_token() {
        let stored = hash_token("a-source-token");
        assert!(verify_token("a-source-token", &stored));
    }

    #[test]
    fn verify_rejects_mismatched_token() {
        let stored = hash_token("a-source-token");
        assert!(!verify_token("another-token", &stored));
        assert!(!verify_token("", &stored));
    }

    #[test]
    fn verify_rejects_malformed_stored_hash() {
        // 库存值不是 64 位 hex 时一律拒绝，且不 panic
        assert!(!verify_token("a-source-token", ""));
        assert!(!verify_token("a-source-token", "not-a-hash"));
        assert!(!verify_token("a-source-token", &"z".repeat(64)));
        assert!(!verify_token("a-source-token", &"a".repeat(63)));
        assert!(!verify_token("a-source-token", &"a".repeat(65)));
    }

    #[test]
    fn verify_is_case_insensitive_on_stored_hash() {
        // 运维手工写入大写 hex 时应照常通过
        assert!(verify_token(
            "a-source-token",
            &hash_token("a-source-token").to_uppercase()
        ));
    }

    #[test]
    fn verify_tolerates_surrounding_whitespace_on_stored_hash() {
        // 从 secret 文件读入的值可能带结尾换行
        let padded = format!("  {}\n", hash_token("a-source-token"));
        assert!(verify_token("a-source-token", &padded));
    }
}

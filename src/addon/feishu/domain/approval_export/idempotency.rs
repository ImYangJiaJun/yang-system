//! 回写幂等键与行内容摘要。
//!
//! # 为什么 Token 必须确定性且是 UUIDv4
//!
//! - 飞书 `create_record` 的 `client_token` 必须是合法 UUIDv4，非法值返回
//!   `1254037`；
//! - 同一 token 重放返回同一 `record_id`，所以「网络失败后重试」不会建重复行；
//! - 目标行被手工删除后重放仍返回旧 id（记录实际不存在）——此时必须
//!   `generation + 1` 换一个新 token 才能真正重建。
//!
//! Token 由 (规则, 实例, 行键, 代次) 派生：同一次尝试永远同 token，强制重建
//! 换代次必然换 token。摘要用 SHA-256 前 16 字节手工格式化，与
//! `approval_uuid::derive_uuid` 同一条口径（`uuid` crate 未开 v5 feature）。

use std::collections::BTreeMap;

use serde_json::Value;
use sha2::{Digest, Sha256};

/// 派生目标记录的 `client_token`。
pub(crate) fn client_token(
    rule_id: i64,
    instance_code: &str,
    row_key: &str,
    generation: i64,
) -> String {
    // 分隔符不可省：坐标字符集重叠时，("ab","c") 与 ("a","bc") 会撞同一个键。
    let seed = format!("approval-export|{rule_id}|{instance_code}|{row_key}|{generation}");
    let digest = Sha256::digest(seed.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // 版本位标 4：飞书要求 UUIDv4 形态（随机性不参与幂等，确定性才是关键）。
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-\
         {:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

/// 行内容摘要：`sha256(row_key|fields)` 的 64 位十六进制。
///
/// `fields` 是 `BTreeMap`，序列化顺序稳定；同内容必然同摘要，供调用方判断
/// 「重试时配置/取值是否变过」。**不要**拿它做幂等键——那由 [`client_token`] 负责。
pub(crate) fn payload_hash(row_key: &str, fields: &BTreeMap<String, Value>) -> String {
    let canonical = serde_json::json!({
        "row_key": row_key,
        "fields": fields,
    });
    let text = serde_json::to_vec(&canonical).unwrap_or_default();
    Sha256::digest(&text)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_fields() -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("备注".to_string(), serde_json::json!("机票")),
            ("付款金额".to_string(), serde_json::json!(80.25)),
        ])
    }

    fn version_nibble(token: &str) -> char {
        token.as_bytes()[14] as char
    }

    #[test]
    fn token_is_a_canonical_uuid_v4() {
        let token = client_token(7, "INST-1", "row:0", 0);
        assert_eq!(token.len(), 36);
        assert_eq!(token.matches('-').count(), 4);
        let hyphen_positions: Vec<usize> = token
            .char_indices()
            .filter(|(_, character)| *character == '-')
            .map(|(index, _)| index)
            .collect();
        assert_eq!(hyphen_positions, vec![8, 13, 18, 23]);
        assert_eq!(version_nibble(&token), '4', "版本位必须是 4：{token}");
        assert!(
            matches!(token.as_bytes()[19] as char, '8' | '9' | 'a' | 'b'),
            "变体位必须是 10xx：{token}"
        );
    }

    #[test]
    fn token_is_stable_for_the_same_attempt() {
        assert_eq!(
            client_token(7, "INST-1", "row:0", 0),
            client_token(7, "INST-1", "row:0", 0)
        );
    }

    #[test]
    fn token_changes_when_any_coordinate_changes() {
        let base = client_token(7, "INST-1", "row:0", 0);
        assert_ne!(base, client_token(8, "INST-1", "row:0", 0));
        assert_ne!(base, client_token(7, "INST-2", "row:0", 0));
        assert_ne!(base, client_token(7, "INST-1", "row:1", 0));
        assert_ne!(
            base,
            client_token(7, "INST-1", "row:0", 1),
            "强制重建必须换新 token"
        );
    }

    #[test]
    fn payload_hash_is_stable_and_content_sensitive() {
        let first = payload_hash("row:0", &sample_fields());
        let second = payload_hash("row:0", &sample_fields());
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|character| character.is_ascii_hexdigit()));

        let mut changed = sample_fields();
        changed.insert("付款金额".to_string(), serde_json::json!(80.26));
        assert_ne!(first, payload_hash("row:0", &changed));
        assert_ne!(first, payload_hash("row:1", &sample_fields()));
    }
}

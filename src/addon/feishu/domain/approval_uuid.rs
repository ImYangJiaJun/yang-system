//! 审批实例的幂等键（`uuid`）派生。
//!
//! # 为什么不能只用 `record_id`
//!
//! `record_id` 只在**单个多维表格内**唯一，全局不一定唯一。两张表的 `record_id`
//! 撞车时，全新记录的首次创建就会吃 `60012`（uuid 冲突）并静默丢单——而
//! 「静默」是关键：冲突的语义是「实例已存在」，所以它会走进回捞路径，捞到
//! **另一条记录**的审批实例。
//!
//! # 为什么必须带 `approval_code`
//!
//! 使用方换审批定义后，同一行应能作为**新单子**重发（设计 §11 限制 4）。
//! 不带定义码的话，旧 uuid 已占用会让换定义后的重发永远撞冲突。
//!
//! # 为什么不用 UUIDv5
//!
//! `uuid` crate 在 `Cargo.toml` 里只开了 `v4` feature，用 v5 要加 feature（拉
//! `sha1`），会改 `Cargo.lock` 并触发 MSRV 冷缓存验证。这里用已有的 `sha2` 取
//! 摘要前 16 字节手工格式化——同样确定性、同样规范形态、同样是 SHA 系。
//!
//! **不要改成 MD5**：CI 有 `pnpm audit`。

#![allow(dead_code)] // 派生先落地并自带测试；消费者（派发编排）在后续任务接入。

use sha2::{Digest, Sha256};

/// 由多维表格坐标与审批定义派生稳定 `uuid`。
///
/// 输出是规范 UUID 形态（36 字符、带连字符），满足飞书对 `uuid` 的长度与格式约束
/// （`instance/create.md:43`：长度 `1~64` 字符、格式建议
/// `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX`）。
///
/// 版本位（`b[6]` 高四位）标 `8`：这是**自定义**派生，不属于 RFC 4122 定义的任何
/// 版本，用保留的 v8 比谎称 v4（随机）或 v5（SHA-1 命名空间）更诚实。变体位
/// （`b[8]` 高两位）标 `10`，符合 RFC 4122。
pub(crate) fn derive_uuid(
    base_token: &str,
    table_id: &str,
    approval_code: &str,
    record_id: &str,
) -> String {
    // 用 `|` 分隔而不是直接拼接：三个坐标的字符集可能重叠，不加分隔符会让
    // ("ab","c") 与 ("a","bc") 派生出同一个键。
    let name = format!("{base_token}|{table_id}|{approval_code}|{record_id}");
    let digest = Sha256::digest(name.as_bytes());

    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_is_canonical_form() {
        // 官方建议形态 XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX（instance/create.md:43），
        // 长度落在 1~64 的约束内。
        let value = derive_uuid("appbcbWCzen6", "tblsRc9GRRX", "4202AD96-9EC1", "recqwIwhc6");
        assert_eq!(value.len(), 36);
        assert_eq!(value.matches('-').count(), 4);
        assert!(
            value.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
            "只允许十六进制字符与连字符：{value}"
        );
        let hyphen_positions: Vec<usize> = value
            .char_indices()
            .filter(|(_, c)| *c == '-')
            .map(|(i, _)| i)
            .collect();
        assert_eq!(hyphen_positions, vec![8, 13, 18, 23], "连字符位置必须规范");
    }

    #[test]
    fn uuid_is_stable_across_calls() {
        // 幂等的根基：同输入必须同输出。不稳定的话每次重扫都建一个新审批单。
        let first = derive_uuid("b", "t", "c", "r");
        let second = derive_uuid("b", "t", "c", "r");
        assert_eq!(first, second);
    }

    #[test]
    fn uuid_differs_across_tables_for_same_record_id() {
        // record_id 只在单个多维表格内唯一。若不带表坐标，两张表的同 id 记录
        // 会撞 60012 并被回捞到**另一条记录**的实例。
        let from_table_one = derive_uuid("base1", "tbl1", "code", "rec_same");
        let from_table_two = derive_uuid("base2", "tbl2", "code", "rec_same");
        assert_ne!(from_table_one, from_table_two);
    }

    #[test]
    fn uuid_differs_when_only_base_or_table_differs() {
        // 两个坐标各自都要参与，不能只带一个。
        let base = derive_uuid("base1", "tbl1", "code", "rec");
        assert_ne!(base, derive_uuid("base2", "tbl1", "code", "rec"));
        assert_ne!(base, derive_uuid("base1", "tbl2", "code", "rec"));
    }

    #[test]
    fn uuid_differs_across_approval_codes() {
        // 换审批定义要能重发（设计 §11 限制 4）。
        let first = derive_uuid("b", "t", "code_a", "r");
        let second = derive_uuid("b", "t", "code_b", "r");
        assert_ne!(first, second);
    }

    #[test]
    fn separator_prevents_coordinate_bleed() {
        // 不加分隔符时 ("ab","c") 与 ("a","bc") 会派生出同一个键。
        assert_ne!(
            derive_uuid("ab", "c", "code", "rec"),
            derive_uuid("a", "bc", "code", "rec")
        );
    }

    #[test]
    fn version_and_variant_bits_are_set() {
        // 变体位（第 9 字节高两位）必须是 10，否则不是一个合法 UUID 形态。
        let value = derive_uuid("b", "t", "c", "r");
        let variant_nibble = value.as_bytes()[19] as char;
        assert!(
            matches!(variant_nibble, '8' | '9' | 'a' | 'b'),
            "变体位必须是 RFC 4122 的 10xx：{value}"
        );
    }
}

//! xlsx 解析：嗅探、表头、按列名取值。
//!
//! **只读，不写。** 本模块不碰数据库、不碰网络，是纯函数层，
//! 所以它能在单元测试里被完整覆盖（导入 Action 的那层不行，要真库）。

#![allow(dead_code)] // 嗅探先落地并自带测试；消费者（xlsx 导入 Action）在后续任务接入。

/// zip 魔数：xlsx 是 zip，`PK\x03\x04`。
///
/// `allowed_content_types` 只是**客户端自称**（框架 `media.rs` 明写它不能替代
/// 内容校验），所以真正的判定在这里。
pub(crate) fn sniff_is_zip(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_magic_is_accepted() {
        assert!(sniff_is_zip(b"PK\x03\x04rest"));
    }

    #[test]
    fn arbitrary_bytes_are_rejected() {
        assert!(!sniff_is_zip(b"not a zip at all"));
        assert!(!sniff_is_zip(b""));
        // 真实夹具：一段纯文本
        assert!(!sniff_is_zip(include_bytes!(
            "../../../../tests/fixtures/xlsx/not_a_zip.bin"
        )));
    }
}

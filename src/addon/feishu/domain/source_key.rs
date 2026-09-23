//! `source_key` 的合法形态——**唯一事实源**。
//!
//! 它进 URL 路径段（`/api/v1/feishu/approval/options/{source_key}`），
//! 因此限定为小写字母开头的 `[a-z0-9_]`：避免百分号编码、大小写歧义与路径穿越。
//!
//! 收敛到这里是因为它有两个消费者：字段级的建源 Action 与表级的建源 Action。
//! 两处各写一份的后果是漂移——一处放宽了，另一处还在按老规矩拒。

/// 数据源标识的合法形态。
///
/// - 首字节必须是小写 ASCII 字母（不能是数字或下划线开头）
/// - 其余字节只允许小写字母、数字、下划线
/// - 总长不超过 64 字节
pub(crate) fn valid_source_key(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && value.len() <= 64
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_keys_the_ledger_actually_uses() {
        // 目标台账已在用的两个（2026-09-23 实测）
        for value in ["payment_currency", "payment_fx_rate"] {
            assert!(valid_source_key(value), "{value} 应合法");
        }
    }

    #[test]
    fn rejects_uppercase_dashes_and_leading_non_letters() {
        for value in ["Bad-Key", "Payment", "1abc", "_abc", "a b", "a/b", "a?b"] {
            assert!(!valid_source_key(value), "{value:?} 应被拒");
        }
    }

    #[test]
    fn rejects_blank_and_overlong() {
        assert!(!valid_source_key(""));
        let long = "a".repeat(65);
        assert!(!valid_source_key(&long));
        let ok = "a".repeat(64);
        assert!(valid_source_key(&ok), "64 字节是上限之内");
    }

    #[test]
    fn rejects_non_ascii_that_would_need_percent_encoding() {
        assert!(!valid_source_key("币种"));
        assert!(!valid_source_key("a币"));
    }
}

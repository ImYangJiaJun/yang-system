//! 反向同步的语义值。
//!
//! 三侧（派发源单元格、审批回读表单、当前台账单元格）都必须投影到同一套
//! [`SyncValue`] 再比较；否则「选项文案 vs option_id」「日期毫秒 vs RFC3339」
//! 「附件 file_token vs 12h URL」会被误判成变化。

use sha2::{Digest, Sha256};

use super::super::approval_export::form::FixedNumber;

/// 已经上传到多维表、可直接写附件列的附件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedAttachment {
    pub(crate) file_token: String,
    pub(crate) name: String,
    pub(crate) size: Option<i64>,
}

/// 反向同步的语义值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SyncValue {
    Text(String),
    Number(FixedNumber),
    Bool(bool),
    /// 单选取 1 个、多选取 N 个；保留回读顺序用于写库。
    Labels(Vec<String>),
    PersonIds(Vec<String>),
    /// 绝对毫秒时间戳。
    Millis(i64),
    DateInterval {
        start: i64,
        end: i64,
    },
    /// 关联审批的实例 Code。
    ConnectCodes(Vec<String>),
    Attachments(Vec<PreparedAttachment>),
    Empty,
}

impl SyncValue {
    /// 把等价但形态不同的值归一：空串/空数组 → `Empty`。
    pub(crate) fn normalized(self) -> Self {
        match self {
            Self::Text(text) if text.trim().is_empty() => Self::Empty,
            Self::Labels(labels) if labels.is_empty() => Self::Empty,
            Self::PersonIds(ids) if ids.is_empty() => Self::Empty,
            Self::ConnectCodes(codes) if codes.is_empty() => Self::Empty,
            Self::Attachments(files) if files.is_empty() => Self::Empty,
            other => other,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        matches!(self.clone().normalized(), Self::Empty)
    }

    /// 稳定、可比较的规范化文本。
    ///
    /// - 标签/人员/关联 Code 排序去重，顺序不同不算变化；
    /// - 附件只按 `name + size` 比较，`file_token` 会随重新上传变化，不参与；
    /// - 数字走 `FixedNumber` 的十进制字面量，`100` 与 `100.0` 同形。
    pub(crate) fn canonical_key(&self) -> String {
        match self.clone().normalized() {
            Self::Empty => "empty".to_string(),
            Self::Text(text) => format!("text:{text}"),
            Self::Number(number) => format!("number:{}", number.to_json_value()),
            Self::Bool(value) => format!("bool:{value}"),
            Self::Labels(mut labels) => {
                labels.sort();
                labels.dedup();
                format!("labels:{}", labels.join("\u{1f}"))
            }
            Self::PersonIds(mut ids) => {
                ids.sort();
                ids.dedup();
                format!("person:{}", ids.join("\u{1f}"))
            }
            Self::Millis(millis) => format!("millis:{millis}"),
            Self::DateInterval { start, end } => format!("interval:{start}:{end}"),
            Self::ConnectCodes(mut codes) => {
                codes.sort();
                codes.dedup();
                format!("connect:{}", codes.join("\u{1f}"))
            }
            Self::Attachments(mut files) => {
                files.sort_by(|left, right| {
                    left.name
                        .cmp(&right.name)
                        .then(left.size.cmp(&right.size))
                        .then(left.file_token.cmp(&right.file_token))
                });
                let parts: Vec<String> = files
                    .iter()
                    .map(|file| {
                        format!(
                            "{}|{}",
                            file.name,
                            file.size.map_or("-".into(), |s| s.to_string())
                        )
                    })
                    .collect();
                format!("attachments:{}", parts.join("\u{1f}"))
            }
        }
    }

    /// SHA-256(canonical_key)；用于 `form_sync_hash` 与日志去重。
    pub(crate) fn hash(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.canonical_key().as_bytes());
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn number(text: &str) -> SyncValue {
        SyncValue::Number(FixedNumber::parse(text).unwrap_or_else(|| panic!("{text}")))
    }

    #[test]
    fn numbers_compare_by_decimal_value_not_literal() {
        assert_eq!(number("100").hash(), number("100.0").hash());
        assert_eq!(number("9.1330").hash(), number("9.133").hash());
        assert_ne!(number("100").hash(), number("100.01").hash());
    }

    #[test]
    fn labels_and_people_are_order_insensitive() {
        assert_eq!(
            SyncValue::Labels(vec!["A".into(), "B".into()]).hash(),
            SyncValue::Labels(vec!["B".into(), "A".into()]).hash()
        );
        assert_eq!(
            SyncValue::PersonIds(vec!["ou_b".into(), "ou_a".into()]).hash(),
            SyncValue::PersonIds(vec!["ou_a".into(), "ou_b".into()]).hash()
        );
        assert_ne!(
            SyncValue::Labels(vec!["A".into()]).hash(),
            SyncValue::Labels(vec!["A".into(), "B".into()]).hash()
        );
    }

    #[test]
    fn attachments_compare_by_name_and_size_not_token() {
        let left = SyncValue::Attachments(vec![
            PreparedAttachment {
                file_token: "t1".into(),
                name: "a.pdf".into(),
                size: Some(10),
            },
            PreparedAttachment {
                file_token: "t2".into(),
                name: "b.pdf".into(),
                size: None,
            },
        ]);
        let right = SyncValue::Attachments(vec![
            PreparedAttachment {
                file_token: "t3".into(),
                name: "b.pdf".into(),
                size: None,
            },
            PreparedAttachment {
                file_token: "t4".into(),
                name: "a.pdf".into(),
                size: Some(10),
            },
        ]);
        assert_eq!(left.hash(), right.hash(), "顺序与 token 不参与比较");
        let changed = SyncValue::Attachments(vec![PreparedAttachment {
            file_token: "t1".into(),
            name: "a.pdf".into(),
            size: Some(11),
        }]);
        assert_ne!(left.hash(), changed.hash(), "大小变化必须可检测");
    }

    #[test]
    fn empty_forms_are_normalized_but_zero_and_false_are_values() {
        assert_eq!(SyncValue::Text("  ".into()).hash(), SyncValue::Empty.hash());
        assert_eq!(
            SyncValue::Labels(Vec::new()).hash(),
            SyncValue::Empty.hash()
        );
        assert_ne!(SyncValue::Bool(false).hash(), SyncValue::Empty.hash());
        assert_ne!(number("0").hash(), SyncValue::Empty.hash());
        assert!(!SyncValue::Bool(false).is_empty());
    }

    #[test]
    fn hash_is_64_hex_chars() {
        let hash = SyncValue::Text("x".into()).hash();
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }
}

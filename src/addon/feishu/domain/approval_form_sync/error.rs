//! 反向同步的错误类型。
//!
//! **只允许携带 id / 字段名 / 结果码，禁止拼表单值**：这些错误会写进
//! `feishu_approval_task.form_sync_error` 与日志，而那张表/日志的读者范围远大于
//! 运维；金额、姓名、电话、附件名都可能在里面。

/// 反向同步失败的分类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SyncError {
    /// 表单里没有这个控件：定义漂移或条件隐藏，不写、不清空。
    MissingWidget { widget_id: String },
    /// 必填控件回读为空。
    EmptyRequired { widget_id: String },
    /// 值存在但形态不对。
    InvalidValue {
        widget_id: String,
        reason: &'static str,
    },
    /// 控件类型本系统不支持反向。
    UnsupportedWidget {
        widget_id: String,
        widget_type: String,
    },
    /// 目标列不可写或类型不匹配。
    UnsupportedTarget {
        field_id: String,
        field_type: Option<i32>,
    },
    /// 外部选项 ID/文案反查不到。
    OptionUnresolved { widget_id: String },
    /// 外部选项同名多 id / 固定选项多对一，无法唯一确定。
    OptionAmbiguous { widget_id: String },
    /// 附件超过单文件上限。
    AttachmentTooLarge { widget_id: String, size: i64 },
    /// 附件没有可用 URL 或下载失败。
    AttachmentUnavailable { widget_id: String },
    /// 目标列在多维表里找不到。
    FieldNotFound { field_id: String },
    /// 多个控件映射到同一目标列。
    DuplicateTarget { field_id: String },
    /// 聚合明细行数与成员数不一致。
    RowCountMismatch { expected: usize, actual: usize },
    /// 聚合行键缺失/重复/匹配不上。
    RowKeyUnresolved { detail_id: String },
    /// 读台账失败。
    BitableRead {
        record_id: String,
        reason: &'static str,
    },
    /// 写台账失败。
    BitableWrite {
        record_id: String,
        reason: &'static str,
    },
    /// 实例表单缺失。
    FormMissing,
    /// 实例表单不是合法 JSON。
    FormInvalid,
    /// 需要人工核对（配置漂移、结构不一致等）。
    Review { reason: &'static str },
}

impl SyncError {
    /// 稳定的结果码，供 metrics / 控制台筛选；不随文案变化。
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::MissingWidget { .. } => "missing_widget",
            Self::EmptyRequired { .. } => "empty_required",
            Self::InvalidValue { .. } => "invalid_value",
            Self::UnsupportedWidget { .. } => "unsupported_widget",
            Self::UnsupportedTarget { .. } => "unsupported_target",
            Self::OptionUnresolved { .. } => "option_unresolved",
            Self::OptionAmbiguous { .. } => "option_ambiguous",
            Self::AttachmentTooLarge { .. } => "attachment_too_large",
            Self::AttachmentUnavailable { .. } => "attachment_unavailable",
            Self::FieldNotFound { .. } => "field_not_found",
            Self::DuplicateTarget { .. } => "duplicate_target",
            Self::RowCountMismatch { .. } => "row_count_mismatch",
            Self::RowKeyUnresolved { .. } => "row_key_unresolved",
            Self::BitableRead { .. } => "bitable_read",
            Self::BitableWrite { .. } => "bitable_write",
            Self::FormMissing => "form_missing",
            Self::FormInvalid => "form_invalid",
            Self::Review { .. } => "review",
        }
    }
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingWidget { widget_id } => {
                write!(formatter, "表单里缺少控件「{widget_id}」，跳过该字段")
            }
            Self::EmptyRequired { widget_id } => {
                write!(formatter, "必填控件「{widget_id}」为空")
            }
            Self::InvalidValue { widget_id, reason } => {
                write!(formatter, "控件「{widget_id}」的值不可用：{reason}")
            }
            Self::UnsupportedWidget {
                widget_id,
                widget_type,
            } => write!(
                formatter,
                "控件「{widget_id}」（{widget_type}）不支持反向同步"
            ),
            Self::UnsupportedTarget {
                field_id,
                field_type,
            } => write!(
                formatter,
                "目标列「{field_id}」（type={field_type:?}）不可写或类型不匹配"
            ),
            Self::OptionUnresolved { widget_id } => {
                write!(formatter, "控件「{widget_id}」的选项无法反查")
            }
            Self::OptionAmbiguous { widget_id } => {
                write!(
                    formatter,
                    "控件「{widget_id}」的选项存在多个候选，需人工核对"
                )
            }
            Self::AttachmentTooLarge { widget_id, size } => write!(
                formatter,
                "控件「{widget_id}」的附件过大（{size} 字节），超过 20MB 上限"
            ),
            Self::AttachmentUnavailable { widget_id } => {
                write!(formatter, "控件「{widget_id}」的附件没有可下载地址")
            }
            Self::FieldNotFound { field_id } => {
                write!(formatter, "目标列「{field_id}」不存在")
            }
            Self::DuplicateTarget { field_id } => {
                write!(formatter, "多个控件映射到同一目标列「{field_id}」")
            }
            Self::RowCountMismatch { expected, actual } => write!(
                formatter,
                "聚合明细行数与成员数不一致（期望 {expected}，实际 {actual}）"
            ),
            Self::RowKeyUnresolved { detail_id } => write!(
                formatter,
                "聚合明细「{detail_id}」的行键缺失、重复或匹配不上成员"
            ),
            Self::BitableRead { record_id, reason } => {
                write!(formatter, "读取记录「{record_id}」失败：{reason}")
            }
            Self::BitableWrite { record_id, reason } => {
                write!(formatter, "写入记录「{record_id}」失败：{reason}")
            }
            Self::FormMissing => write!(formatter, "审批实例没有表单"),
            Self::FormInvalid => write!(formatter, "审批实例表单不是合法 JSON 数组"),
            Self::Review { reason } => write!(formatter, "需要人工核对：{reason}"),
        }
    }
}

impl std::error::Error for SyncError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_never_contains_values() {
        // 构造函数只收 id / 类型 / 固定原因，没有值参数；这条测试钉住文案
        // 不会因为后续改动把值拼进来。
        for error in [
            SyncError::InvalidValue {
                widget_id: "w1".into(),
                reason: "日期不是 RFC3339",
            },
            SyncError::BitableWrite {
                record_id: "rec1".into(),
                reason: "permission_denied",
            },
            SyncError::OptionAmbiguous {
                widget_id: "w1".into(),
            },
        ] {
            let text = error.to_string();
            assert!(!text.contains("张三"), "{text}");
            assert!(!text.contains("123.45"), "{text}");
            assert!(!text.contains("https://"), "{text}");
        }
    }

    #[test]
    fn codes_are_stable_and_unique() {
        let errors = [
            SyncError::MissingWidget {
                widget_id: "w".into(),
            },
            SyncError::EmptyRequired {
                widget_id: "w".into(),
            },
            SyncError::FormMissing,
            SyncError::FormInvalid,
            SyncError::Review { reason: "r" },
        ];
        let codes: Vec<_> = errors.iter().map(SyncError::code).collect();
        let unique: std::collections::BTreeSet<_> = codes.iter().collect();
        assert_eq!(codes.len(), unique.len(), "{codes:?}");
    }
}

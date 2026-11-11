//! `feishu_approval_comment_run` 表声明：审批评论回写的逐条运行台账。
//!
//! 一行 = 一条目标多维表格记录在一条规则下的评论状态。唯一键
//! `(rule_id, record_id)` 保证事件驱动与轮询兜底两条入口不会重复建账。
//!
//! # 状态机
//!
//! ```text
//! pending -> commenting -> commented -> done
//!                 |             |
//!                 v             v（状态列写失败，下一轮只补写状态）
//!               retry         commented
//!                 |
//!                 v（连续失败达上限 / Fatal）
//!               failed
//! ```
//!
//! `waiting` / `skipped` 表示本轮条件未满足；下一轮扫描会重新评估并唤醒。
//! `commented` 一旦落库就**绝不再发评论**：重启、重复事件、状态列写失败都
//! 只会进入「补写状态列」分支。

use yang_base::definition::{
    FieldName, FieldRef, Int, Key, Str, TableName, TableSpec, Text, Timestamp,
};
use yang_base::BaseError;

/// 单条评论最多尝试次数；达上限后置 `failed`。
pub(crate) const MAX_COMMENT_ATTEMPTS: i64 = 5;

/// 台账状态集合（状态机契约；运行期认领集合见 worker）。
#[allow(dead_code)]
pub(crate) const COMMENT_STATES: &[&str] = &[
    "pending",
    "commenting",
    "commented",
    "done",
    "retry",
    "failed",
    "skipped",
    "waiting",
];

/// 声明审批评论运行台账表。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    let table_name = yang_base::table!("feishu_approval_comment_run");
    Ok(TableSpec::new(table_name.clone())
        .title("飞书审批评论运行")
        .fields(yang_base::fields! {
            id => Key::new().title("ID").filterable(true).sortable(true),
            rule_id => Int::new()
                .title("所属规则")
                .require(true)
                .indexed(true)
                .filterable(true),
            // 实例编码在事件到达时未必能解析；解析不到时保持 NULL，由处理轮
            // 反查 `feishu_approval_export_run` 补齐。解析失败则直接 failed。
            instance_code => Str::new()
                .title("实例编码")
                .max_length(128)
                .indexed(true)
                .filterable(true)
                .sortable(true),
            record_id => Str::new()
                .title("目标记录 ID")
                .require(true)
                .max_length(128)
                .filterable(true)
                .sortable(true),
            // pending / commenting / commented / done / retry / failed / skipped / waiting
            state => Str::new()
                .title("状态")
                .require(true)
                .max_length(32)
                .default("pending")
                .filterable(true),
            // 支付字段内容的稳定指纹（sha256 十六进制）；用于诊断「条件相同但值变了」。
            payment_fingerprint => Str::new().title("支付内容指纹").max_length(64),
            comment_id => Str::new().title("评论 ID").max_length(128),
            attempts => Int::new()
                .title("尝试次数")
                .require(true)
                .default(0),
            next_attempt_at => Timestamp::new()
                .title("下次尝试时间")
                .filterable(true)
                .sortable(true),
            last_error => Text::new().title("最后错误"),
            created_at => Timestamp::new().created_at().title("创建时间"),
            updated_at => Timestamp::new().updated_at().title("更新时间").sortable(true),
        })
        .unique_named(
            "uk_feishu_approval_comment_run",
            [
                field_ref(&table_name, "rule_id")?,
                field_ref(&table_name, "record_id")?,
            ],
        ))
}

fn field_ref(table_name: &TableName, field: &str) -> Result<FieldRef, BaseError> {
    let field = FieldName::new(field).map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(FieldRef::new(table_name.clone(), field))
}

/// 事件/扫描入口的幂等唤醒是否应重置为 pending。
///
/// `commenting`（可能在飞行中）、`commented`（只需补状态）、`done`/`failed`
/// （终态）不重置；其中 `commented` 允许把 `next_attempt_at` 拉近，让状态列
/// 写失败尽快重试。
pub(crate) fn wake_state(state: &str) -> Option<&'static str> {
    match state.trim() {
        "pending" | "retry" | "waiting" | "skipped" => Some("pending"),
        "commented" => Some("commented"),
        _ => None,
    }
}

/// 是否是可被处理轮认领的状态。
#[allow(dead_code)]
pub(crate) fn is_claimable_state(state: &str) -> bool {
    matches!(state.trim(), "pending" | "retry" | "commented")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition() -> yang_base::table::TableDefinition {
        table_spec()
            .unwrap_or_else(|error| panic!("表声明应有效: {error}"))
            .table_definition()
            .unwrap_or_else(|error| panic!("应可编译为表定义: {error}"))
    }

    #[test]
    fn table_has_expected_name_and_unique_key_columns() {
        let definition = definition();
        assert_eq!(definition.name(), "feishu_approval_comment_run");
        assert_eq!(definition.primary_key(), "id");
        for name in ["rule_id", "record_id", "state"] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(field.is_required(), "{name} 必填（唯一键/状态机依赖）");
        }
        for name in ["rule_id", "record_id", "state"] {
            assert!(
                definition
                    .field(name)
                    .is_some_and(|field| field.is_filterable()),
                "{name} 必须可筛选"
            );
        }
    }

    #[test]
    fn state_defaults_to_pending_and_attempts_to_zero() {
        let definition = definition();
        assert_eq!(
            definition
                .field("state")
                .and_then(|field| field.default_value().cloned()),
            Some(serde_json::json!("pending"))
        );
        assert_eq!(
            definition
                .field("attempts")
                .and_then(|field| field.default_value().cloned()),
            Some(serde_json::json!(0))
        );
    }

    #[test]
    fn wake_transition_never_reopens_terminal_or_inflight_states() {
        for state in ["pending", "retry", "waiting", "skipped"] {
            assert_eq!(wake_state(state), Some("pending"), "{state}");
        }
        assert_eq!(wake_state("commented"), Some("commented"));
        for state in ["commenting", "done", "failed", "unknown"] {
            assert_eq!(wake_state(state), None, "{state}");
        }
        for state in ["pending", "retry", "commented"] {
            assert!(is_claimable_state(state), "{state}");
        }
        for state in ["commenting", "done", "failed", "skipped", "waiting"] {
            assert!(!is_claimable_state(state), "{state}");
        }
    }

    #[test]
    fn every_field_has_an_explicit_chinese_label() {
        let definition = definition();
        for field in definition.fields() {
            assert!(
                !field.label().is_empty(),
                "字段 {} 必须有展示名",
                field.name()
            );
            assert_ne!(
                field.label(),
                field.name(),
                "字段 {} 的展示名退化成了字段名",
                field.name()
            );
        }
    }
}

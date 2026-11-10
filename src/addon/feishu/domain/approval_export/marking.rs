//! 目标状态列的标记判定。
//!
//! # 语义
//!
//! 已写过的行（`write_status=written` 且 `target_record_id` 非空）在实例状态
//! 变化时写一次状态文案：
//!
//! - `reverted == true` 或状态 `REVERTED` → `reverted`（撤销）；
//! - `REJECTED` → `rejected`；`CANCELED` → `canceled`；`DELETED` → `deleted`；
//! - `PENDING` 且触发节点已不再通过（被退回）→ `rolled_back`；要求最终通过的
//!   规则（`final_approved`，或 `node_passed` + `require_final_approval`）回到
//!   `PENDING` 同样按已退回处理——已写行只可能来自一次最终通过；
//! - 其余 → `valid`（重新通过后恢复）。
//!
//! 文案默认「有效 / 已退回 / 已拒绝 / 已撤回 / 已撤销 / 已删除」，可由规则表的
//! `status_labels_json` 覆盖；文案相同则调用方跳过写目标表。

use serde_json::Value;

use super::super::approval::InstanceDetail;
use super::trigger::compute_node_passed;
use super::{ExportError, ExportRule, TriggerMode};

/// run 行的 `mark_status`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarkStatus {
    /// 未写过的行 / 未启用标记：什么都不做。
    None,
    Valid,
    RolledBack,
    Rejected,
    Canceled,
    Reverted,
    Deleted,
}

impl MarkStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Valid => "valid",
            Self::RolledBack => "rolled_back",
            Self::Rejected => "rejected",
            Self::Canceled => "canceled",
            Self::Reverted => "reverted",
            Self::Deleted => "deleted",
        }
    }
}

/// 一次标记判定的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MarkOutcome {
    pub(crate) status: MarkStatus,
    /// 要写进目标状态列的文案；`None` = 不写。
    pub(crate) label: Option<String>,
}

/// 状态列文案。默认值与 `status_labels_json` 的覆盖值合并。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusLabels {
    pub(crate) valid: String,
    pub(crate) rolled_back: String,
    pub(crate) rejected: String,
    pub(crate) canceled: String,
    pub(crate) reverted: String,
    pub(crate) deleted: String,
}

impl Default for StatusLabels {
    fn default() -> Self {
        Self {
            valid: "有效".to_string(),
            rolled_back: "已退回".to_string(),
            rejected: "已拒绝".to_string(),
            canceled: "已撤回".to_string(),
            reverted: "已撤销".to_string(),
            deleted: "已删除".to_string(),
        }
    }
}

impl StatusLabels {
    /// 解析用户覆盖。键既接受英文常量（`valid` / `rolled_back` …），也接受
    /// 表注释里的中文（有效/已退回/…）；未知键按配置错误拒绝，避免「改了但没生效」。
    pub(crate) fn parse(raw: Option<&str>) -> Result<Self, ExportError> {
        let mut labels = Self::default();
        let Some(raw) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
            return Ok(labels);
        };
        let value: Value = serde_json::from_str(raw)
            .map_err(|error| ExportError::Fatal(format!("状态文案 JSON 解析失败：{error}")))?;
        let object = value
            .as_object()
            .ok_or_else(|| ExportError::Fatal("状态文案必须是 JSON 对象".to_string()))?;
        for (key, value) in object {
            let text = value
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .ok_or_else(|| ExportError::Fatal(format!("状态文案「{key}」必须是非空字符串")))?;
            match key.as_str() {
                "valid" | "有效" => labels.valid = text.to_string(),
                "rolled_back" | "已退回" => labels.rolled_back = text.to_string(),
                "rejected" | "已拒绝" => labels.rejected = text.to_string(),
                "canceled" | "cancelled" | "已撤回" => labels.canceled = text.to_string(),
                "reverted" | "已撤销" => labels.reverted = text.to_string(),
                "deleted" | "已删除" => labels.deleted = text.to_string(),
                other => return Err(ExportError::Fatal(format!("未知的状态文案键「{other}」"))),
            }
        }
        Ok(labels)
    }

    pub(crate) fn label(&self, status: MarkStatus) -> Option<&str> {
        match status {
            MarkStatus::Valid => Some(self.valid.as_str()),
            MarkStatus::RolledBack => Some(self.rolled_back.as_str()),
            MarkStatus::Rejected => Some(self.rejected.as_str()),
            MarkStatus::Canceled => Some(self.canceled.as_str()),
            MarkStatus::Reverted => Some(self.reverted.as_str()),
            MarkStatus::Deleted => Some(self.deleted.as_str()),
            MarkStatus::None => None,
        }
    }
}

/// 计算目标状态列的期望状态与文案。
///
/// `has_written_rows` 为 `false`（没写过 / `target_record_id` 为空）时返回
/// [`MarkStatus::None`]：不存在的目标行无从标记。
pub(crate) fn desired_mark(
    rule: &ExportRule,
    detail: &InstanceDetail,
    has_written_rows: bool,
) -> MarkOutcome {
    if !has_written_rows
        || !rule.mark_enabled
        || rule
            .status_field
            .as_deref()
            .map_or(true, |field| field.trim().is_empty())
    {
        return MarkOutcome {
            status: MarkStatus::None,
            label: None,
        };
    }

    let status = detail.status.as_deref().unwrap_or_default();
    let mark = if detail.reverted == Some(true) || status == "REVERTED" {
        MarkStatus::Reverted
    } else if status == "REJECTED" {
        MarkStatus::Rejected
    } else if status == "CANCELED" {
        MarkStatus::Canceled
    } else if status == "DELETED" {
        MarkStatus::Deleted
    } else if status == "PENDING" {
        let rolled_back = match rule.trigger_mode {
            // 要求最终通过的已写行只可能来自一次 APPROVED；回到 PENDING
            // 就是被退回重审，不能继续显示「有效」。注意 `node_passed` 也可能
            // 叠加 `require_final_approval`，不能只看 trigger_mode。
            TriggerMode::FinalApproved => true,
            TriggerMode::NodePassed if rule.require_final_approval => true,
            TriggerMode::NodePassed => !compute_node_passed(rule, detail).node_still_passed(),
        };
        if rolled_back {
            MarkStatus::RolledBack
        } else {
            MarkStatus::Valid
        }
    } else {
        MarkStatus::Valid
    };

    MarkOutcome {
        status: mark,
        label: rule.status_labels.label(mark).map(str::to_string),
    }
}

/// 目标列已写的文案与本次期望是否一致（一致则跳过写目标表）。
pub(crate) fn should_apply(applied_label: Option<&str>, outcome: &MarkOutcome) -> bool {
    if outcome.status == MarkStatus::None {
        return false;
    }
    applied_label != outcome.label.as_deref()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::feishu::domain::approval::TaskNode;
    use crate::addon::feishu::domain::approval_export::{Condition, RowFilter};

    fn rule(mode: TriggerMode) -> ExportRule {
        ExportRule {
            id: 1,
            title: "测试".to_string(),
            approval_code: "code".to_string(),
            trigger_mode: mode,
            trigger_node_ids: vec!["n1".to_string()],
            require_final_approval: false,
            detail_widget_id: "detail".to_string(),
            row_filter: RowFilter::AllRows,
            condition: Condition::default(),
            target_base_token: "base".to_string(),
            target_table_id: "table".to_string(),
            status_field: Some("fld_status".to_string()),
            status_field_name: Some("状态".to_string()),
            status_labels: StatusLabels::default(),
            mark_enabled: true,
            enabled: true,
            effective_from: None,
        }
    }

    fn detail(status: &str, reverted: Option<bool>, tasks: Vec<TaskNode>) -> InstanceDetail {
        InstanceDetail {
            instance_code: "INST-1".to_string(),
            approval_code: Some("code".to_string()),
            serial_number: None,
            status: Some(status.to_string()),
            start_time: None,
            end_time: None,
            open_id: None,
            user_id: None,
            department_id: None,
            department_name: None,
            uuid: None,
            reverted,
            task_list: Some(tasks),
            timeline: Some(Vec::new()),
            form: None,
        }
    }

    fn task(status: &str, start_time: &str) -> TaskNode {
        TaskNode {
            id: Some("1".to_string()),
            node_id: Some("n1".to_string()),
            node_name: None,
            user_id: None,
            open_id: None,
            status: Some(status.to_string()),
            r#type: Some("AND".to_string()),
            start_time: Some(start_time.to_string()),
            end_time: Some("0".to_string()),
        }
    }

    #[test]
    fn terminal_states_map_to_their_mark_status() {
        let rule = rule(TriggerMode::NodePassed);
        let cases = [
            ("REJECTED", None, MarkStatus::Rejected, "已拒绝"),
            ("CANCELED", None, MarkStatus::Canceled, "已撤回"),
            ("DELETED", None, MarkStatus::Deleted, "已删除"),
            ("APPROVED", Some(true), MarkStatus::Reverted, "已撤销"),
            ("REVERTED", None, MarkStatus::Reverted, "已撤销"),
            ("APPROVED", None, MarkStatus::Valid, "有效"),
        ];
        for (status, reverted, expected, label) in cases {
            let outcome = desired_mark(&rule, &detail(status, reverted, vec![]), true);
            assert_eq!(outcome.status, expected, "{status}");
            assert_eq!(outcome.label.as_deref(), Some(label), "{status}");
        }
    }

    #[test]
    fn pending_after_rollback_is_rolled_back() {
        let rule = rule(TriggerMode::NodePassed);
        // 新任务在退回后重新出现、尚未通过 → 节点不再通过。
        let pending = detail("PENDING", None, vec![task("PENDING", "2000")]);
        let outcome = desired_mark(&rule, &pending, true);
        assert_eq!(outcome.status, MarkStatus::RolledBack);
        assert_eq!(outcome.label.as_deref(), Some("已退回"));
    }

    #[test]
    fn pending_after_final_approval_is_rolled_back() {
        let rule = rule(TriggerMode::FinalApproved);
        let outcome = desired_mark(&rule, &detail("PENDING", None, vec![]), true);
        assert_eq!(outcome.status, MarkStatus::RolledBack);
        assert_eq!(outcome.label.as_deref(), Some("已退回"));
    }

    #[test]
    fn pending_after_final_approval_requirement_is_rolled_back() {
        let mut rule = rule(TriggerMode::NodePassed);
        rule.require_final_approval = true;
        let outcome = desired_mark(
            &rule,
            &detail("PENDING", None, vec![task("APPROVED", "1000")]),
            true,
        );
        assert_eq!(
            outcome.status,
            MarkStatus::RolledBack,
            "节点仍通过，但规则要求最终通过；回到 PENDING 就是退回"
        );
    }

    #[test]
    fn pending_with_the_node_still_passed_stays_valid() {
        let rule = rule(TriggerMode::NodePassed);
        let pending = detail("PENDING", None, vec![task("APPROVED", "1000")]);
        assert_eq!(
            desired_mark(&rule, &pending, true).status,
            MarkStatus::Valid
        );
    }

    #[test]
    fn rows_that_were_never_written_are_not_marked() {
        let rule = rule(TriggerMode::NodePassed);
        let outcome = desired_mark(&rule, &detail("REJECTED", None, vec![]), false);
        assert_eq!(
            outcome,
            MarkOutcome {
                status: MarkStatus::None,
                label: None
            }
        );
    }

    #[test]
    fn marking_can_be_disabled_or_unconfigured() {
        let mut disabled = rule(TriggerMode::NodePassed);
        disabled.mark_enabled = false;
        assert_eq!(
            desired_mark(&disabled, &detail("REJECTED", None, vec![]), true).status,
            MarkStatus::None
        );

        let mut no_field = rule(TriggerMode::NodePassed);
        no_field.status_field = None;
        assert_eq!(
            desired_mark(&no_field, &detail("REJECTED", None, vec![]), true).status,
            MarkStatus::None
        );
    }

    #[test]
    fn custom_labels_override_defaults_with_english_or_chinese_keys() {
        let labels = StatusLabels::parse(Some(
            r#"{"valid":"生效","已退回":"退回修改","canceled":"已撤销申请"}"#,
        ))
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(labels.label(MarkStatus::Valid), Some("生效"));
        assert_eq!(labels.label(MarkStatus::RolledBack), Some("退回修改"));
        assert_eq!(labels.label(MarkStatus::Canceled), Some("已撤销申请"));
        assert_eq!(labels.label(MarkStatus::Rejected), Some("已拒绝"));

        let mut rule = rule(TriggerMode::NodePassed);
        rule.status_labels = labels;
        assert_eq!(
            desired_mark(&rule, &detail("APPROVED", None, vec![]), true)
                .label
                .as_deref(),
            Some("生效")
        );
    }

    #[test]
    fn malformed_label_json_is_fatal() {
        assert!(matches!(
            StatusLabels::parse(Some("[]")),
            Err(ExportError::Fatal(_))
        ));
        assert!(matches!(
            StatusLabels::parse(Some(r#"{"unknown":"x"}"#)),
            Err(ExportError::Fatal(_))
        ));
        assert!(matches!(
            StatusLabels::parse(Some(r#"{"valid":1}"#)),
            Err(ExportError::Fatal(_))
        ));
    }

    #[test]
    fn skip_when_the_applied_label_is_already_the_desired_one() {
        let outcome = MarkOutcome {
            status: MarkStatus::Valid,
            label: Some("有效".to_string()),
        };
        assert!(!should_apply(Some("有效"), &outcome));
        assert!(should_apply(Some("已拒绝"), &outcome));
        assert!(should_apply(None, &outcome));
        assert!(!should_apply(
            Some("有效"),
            &MarkOutcome {
                status: MarkStatus::None,
                label: None
            }
        ));
    }
}

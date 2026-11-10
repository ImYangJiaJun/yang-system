//! 审批 → 多维表格回写的纯领域引擎。
//!
//! 一条回写规则 = 一个审批定义 + 触发节点 + 实例/行级条件 + 目标 Base/表 +
//! 字段映射 + 状态列标记。本层只做**纯计算**：解析实例表单、判定节点是否通过、
//! 求值条件、把明细行映射成目标记录、派生幂等 Token 与标记文案。
//!
//! # 边界
//!
//! - **不读数据库、不调飞书**：配置由 worker/actions 从 `feishu_approval_export_*`
//!   三张表读出后构造成 [`ExportRule`] / [`ExportFieldMap`] 传进来；
//! - 目标列只依赖 `bitable::FieldItem` 元数据与 `bitable::encode_export_cell`
//!   的编码规则，出站由调用方负责；
//! - 错误按处置分类为 [`ExportError::Retryable`] / [`ExportError::Fatal`] /
//!   [`ExportError::Review`]，调用方据此落 run 行的 `retry` / `terminal` /
//!   `review` 状态。
//!
//! # 真机口径（实现时不可回退）
//!
//! - 节点身份用 `task_list[].node_id`，**不是** `timeline.node_key`
//!   （后者是 `APPROVAL_x_y`）；
//! - 回退重审后同一节点会同时保留旧 `DONE` + 新 `APPROVED`，必须按该节点最近
//!   一次 `ROLLBACK` / `ROLLBACK_SELECTED` 的时间过滤旧任务；
//! - 单选实例回读是**展示文案**（`option.text`），不是提交时的选项 id；
//! - 定义漂移（引用的控件在实例表单里不存在）一律记 [`Drift`]，**不得静默当空**。

pub(crate) mod condition;
pub(crate) mod config;
pub(crate) mod form;
pub(crate) mod idempotency;
pub(crate) mod mapping;
pub(crate) mod marking;
pub(crate) mod trigger;

pub(crate) use condition::Condition;
pub(crate) use form::InstanceForm;
pub(crate) use idempotency::client_token;
pub(crate) use mapping::{
    build_rows, parse_option_map, Converter, ExportFieldMap, RowReview, SourceKind, TargetRow,
};
pub(crate) use marking::{desired_mark, MarkOutcome, MarkStatus, StatusLabels};
pub(crate) use trigger::{compute_node_passed, TriggerOutcome};

/// run 表规则级行的 `row_key`。
pub(crate) const RULE_ROW_KEY: &str = "__rule__";

/// 触发模式为「最终通过」时的节点标识。
pub(crate) const FINAL_NODE_ID: &str = "__final__";

/// 一条回写规则的领域形态。
///
/// 字段与 `feishu_approval_export_rule` 一一对应；`condition` / `status_labels`
/// 已由调用方用 [`Condition::parse`] / [`StatusLabels::parse`] 解析好，
/// `trigger_node_ids` 已由 `trigger_node_ids` 列的 JSON 数组解析好。
#[derive(Debug, Clone)]
pub(crate) struct ExportRule {
    pub(crate) id: i64,
    pub(crate) title: String,
    pub(crate) approval_code: String,
    pub(crate) trigger_mode: TriggerMode,
    pub(crate) trigger_node_ids: Vec<String>,
    pub(crate) require_final_approval: bool,
    /// 明细控件 id（行级回写的来源）。
    pub(crate) detail_widget_id: String,
    pub(crate) row_filter: RowFilter,
    pub(crate) condition: Condition,
    pub(crate) target_base_token: String,
    pub(crate) target_table_id: String,
    /// 目标状态列 field_id。`None` = 不标记。
    pub(crate) status_field: Option<String>,
    pub(crate) status_field_name: Option<String>,
    pub(crate) status_labels: StatusLabels,
    pub(crate) mark_enabled: bool,
    pub(crate) enabled: bool,
    /// 只处理此毫秒时间之后创建的实例（`None` = 不限制）。
    pub(crate) effective_from: Option<i64>,
}

/// 触发模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TriggerMode {
    /// 指定节点通过即触发（是否还要求最终通过由 `require_final_approval` 决定）。
    NodePassed,
    /// 实例最终通过才触发。
    FinalApproved,
}

impl TriggerMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NodePassed => "node_passed",
            Self::FinalApproved => "final_approved",
        }
    }

    pub(crate) fn parse(raw: &str) -> Result<Self, ExportError> {
        match raw.trim() {
            "node_passed" => Ok(Self::NodePassed),
            "final_approved" => Ok(Self::FinalApproved),
            other => Err(ExportError::Fatal(format!(
                "未知的触发模式「{other}」：只支持 node_passed / final_approved"
            ))),
        }
    }
}

/// 行筛选。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowFilter {
    /// 只写满足行条件（`scope=detail`）的明细行。
    MatchedRows,
    /// 实例条件命中后写全部明细行。
    AllRows,
}

impl RowFilter {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::MatchedRows => "matched_rows",
            Self::AllRows => "all_rows",
        }
    }

    pub(crate) fn parse(raw: &str) -> Result<Self, ExportError> {
        match raw.trim() {
            "matched_rows" => Ok(Self::MatchedRows),
            "all_rows" => Ok(Self::AllRows),
            other => Err(ExportError::Fatal(format!(
                "未知的行筛选「{other}」：只支持 matched_rows / all_rows"
            ))),
        }
    }
}

/// 定义漂移：规则引用的控件/列在实例或目标表里不存在。
///
/// 出现的每一处都必须被调用方记录（run 行 `last_error` / 日志），因为
/// 「静默当空」会把审批里的真实值写成空白，比报错更危险。
// 三个变体都以 Missing 开头是刻意的：它们描述的都是「定义漂移导致的缺失」，
// 前缀让调用方在日志与 match 里一眼看出处置类别。
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Drift {
    /// 实例表单里没有这个顶层控件。
    MissingTopWidget { widget_id: String },
    /// 实例表单里没有这个明细控件。
    MissingDetailWidget { detail_widget_id: String },
    /// 明细行里没有这个子控件（顶层控件存在、个别行缺单元格）。
    MissingDetailCell {
        detail_widget_id: String,
        widget_id: String,
    },
}

impl std::fmt::Display for Drift {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingTopWidget { widget_id } => write!(
                formatter,
                "实例表单里缺少控件「{widget_id}」（审批定义可能已变更）"
            ),
            Self::MissingDetailWidget { detail_widget_id } => write!(
                formatter,
                "实例表单里缺少明细控件「{detail_widget_id}」（审批定义可能已变更）"
            ),
            Self::MissingDetailCell {
                detail_widget_id,
                widget_id,
            } => write!(
                formatter,
                "明细「{detail_widget_id}」的行里缺少控件「{widget_id}」（审批定义可能已变更）"
            ),
        }
    }
}

/// 领域计算的失败分类。
///
/// 与出站 [`super::outbound::FailureKind`] 分开：本层没有 I/O，`Retryable`
/// 只用于「数据暂时不完整，过一会儿再看」的形态（例如表单尚未生成完整）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExportError {
    /// 暂时性失败：调用方退避后重试。
    Retryable(String),
    /// 不可恢复：配置/数据形态错误，重试不会变好。
    Fatal(String),
    /// 需要人工核对：配置与当前定义/目标表发生漂移。
    Review(String),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retryable(message) => write!(formatter, "暂时失败，可重试：{message}"),
            Self::Fatal(message) => write!(formatter, "不可重试：{message}"),
            Self::Review(message) => write!(formatter, "需要人工核对：{message}"),
        }
    }
}

impl std::error::Error for ExportError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_mode_and_row_filter_parse_their_stored_values() {
        assert_eq!(
            TriggerMode::parse("node_passed").unwrap_or_else(|error| panic!("{error}")),
            TriggerMode::NodePassed
        );
        assert_eq!(
            TriggerMode::parse(" final_approved ").unwrap_or_else(|error| panic!("{error}")),
            TriggerMode::FinalApproved
        );
        assert!(TriggerMode::parse("approved").is_err());
        assert_eq!(
            RowFilter::parse("matched_rows").unwrap_or_else(|error| panic!("{error}")),
            RowFilter::MatchedRows
        );
        assert_eq!(
            RowFilter::parse("all_rows").unwrap_or_else(|error| panic!("{error}")),
            RowFilter::AllRows
        );
        assert!(RowFilter::parse("all").is_err());
    }

    #[test]
    fn drift_display_names_the_widget_and_the_scope() {
        let drift = Drift::MissingDetailCell {
            detail_widget_id: "detail".to_string(),
            widget_id: "amount".to_string(),
        };
        let text = drift.to_string();
        assert!(text.contains("detail") && text.contains("amount"), "{text}");
    }
}

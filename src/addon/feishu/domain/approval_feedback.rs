//! HTTP 与机器人共享的中文反馈；只展示有来源的名称与安全摘要。

use schemars::JsonSchema;
use serde::Serialize;

use super::approval_convert::{ConvertError, WidgetMap};
use super::approval_match::FormWidget;
use super::bitable::FieldItem;
use super::outbound::{fatal_hint, FailureKind, OutboundFailure};
use super::widget_rules;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FeedbackStatus {
    Succeeded,
    Waiting,
    CreatedPendingBackfill,
    Retryable,
    Failed,
    Accepted,
    /// 单条记录已被受理但本轮没有执行（已在队列中/正被 worker 处理）。
    /// 与 [`Self::Accepted`] 分开：后者是页面按钮的**批量**受理，正文不能写「批量」。
    Queued,
}

impl FeedbackStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Waiting => "waiting",
            Self::CreatedPendingBackfill => "created_pending_backfill",
            Self::Retryable => "retryable",
            Self::Failed => "failed",
            Self::Accepted => "accepted",
            Self::Queued => "queued",
        }
    }

    pub(crate) fn heading(self) -> &'static str {
        match self {
            Self::Succeeded => "审批实例已确认，编号已回填",
            Self::Waiting => "本轮未创建审批，需要修正数据",
            Self::CreatedPendingBackfill => "审批实例已确认，回填尚未完成",
            Self::Retryable => "本轮未完成，审批是否创建尚未确认",
            Self::Failed => "本轮处理失败",
            Self::Accepted => "批量派发已受理",
            Self::Queued => "任务已在队列中",
        }
    }

    fn suggestion(self) -> &'static str {
        match self {
            Self::Succeeded => "请到表格查看审批编号；可选扩展字段以表格实际值为准。",
            Self::Waiting => "按原因补齐数据或修正映射后，再点击按钮。",
            Self::CreatedPendingBackfill => {
                "先检查审批和回填列；修正权限或配置后可重试，系统沿用原审批 UUID。"
            }
            Self::Retryable => "先检查表格和审批记录，稍后重试；同一记录沿用原审批 UUID。",
            Self::Failed => "请管理员核对审批配置与应用权限，并查看派发记录。",
            Self::Accepted => {
                "后台扫描所有启用配置中回填列为空的记录，结果稍后写入各表；此消息不代表处理完成。"
            }
            Self::Queued => "无需重复点击；结果稍后写入表格，可在任务列表查看进度。",
        }
    }
}

pub(crate) fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars()
            .take(max.saturating_sub(1))
            .chain(['…'])
            .collect()
    }
}

pub(crate) fn one_line(text: &str, max: usize) -> String {
    truncate(
        &text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>(),
        max,
    )
}

pub(crate) fn display_name(text: Option<&str>) -> String {
    text.map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| one_line(v, 100))
        .unwrap_or_else(|| "名称暂不可用".into())
}

pub(crate) fn field_name(fields: &[FieldItem], id: &str) -> String {
    display_name(
        fields
            .iter()
            .find(|f| f.field_id == id || f.field_name == id)
            .map(|f| f.field_name.as_str()),
    )
}

pub(crate) fn widget_name(form: &[FormWidget], id: &str) -> String {
    fn find(form: &[FormWidget], id: &str) -> Option<String> {
        for widget in form {
            if widget.id == id {
                return Some(display_name(Some(&widget.name)));
            }
            if let Some(child) = find(&widget.children, id) {
                return Some(format!("{} / {child}", display_name(Some(&widget.name))));
            }
        }
        None
    }
    find(form, id).unwrap_or_else(|| "名称暂不可用".into())
}

fn find_node<'a>(form: &'a [FormWidget], id: &str) -> Option<&'a FormWidget> {
    form.iter().find(|widget| widget.id == id).or_else(|| {
        form.iter()
            .find_map(|widget| find_node(&widget.children, id))
    })
}

/// 必填容器（明细 / 控件组）父级自身不进映射表
/// （`approval_match::is_eligible_for_mapping`），字段定位必然为空。列出它下面
/// **已映射**的列名，否则「请填写来源字段」是在让用户去找一个根本不存在的字段。
fn container_columns(
    form: &[FormWidget],
    id: &str,
    widgets: &[WidgetMap],
    fields: &[FieldItem],
) -> Option<String> {
    let node = find_node(form, id).filter(|node| widget_rules::is_container(&node.r#type))?;
    let names: Vec<String> = node
        .walk()
        .iter()
        .filter_map(|child| widgets.iter().find(|w| w.widget_id == child.id))
        .map(|mapping| field_name(fields, &mapping.bitable_field))
        .collect();
    (!names.is_empty()).then(|| names.join(" / "))
}

pub(crate) fn conversion_reason(
    error: &ConvertError,
    snapshot: &str,
    widgets: &[WidgetMap],
    fields: &[FieldItem],
) -> String {
    let (id, reason, suggestion) = match error {
        ConvertError::MissingRequired { widget_id } => {
            (widget_id, "缺少必填值", "请填写来源字段。")
        }
        ConvertError::BadValue { widget_id, .. } => (
            widget_id,
            "值的格式不符合审批控件要求",
            "请核对来源字段类型与控件要求。",
        ),
        ConvertError::UnmappedOption { widget_id, .. } => (
            widget_id,
            "所选文案没有审批选项映射",
            "请管理员补充选项映射。",
        ),
        ConvertError::Unsupported { widget_id, .. } => (
            widget_id,
            "控件暂不支持 API 提单",
            "请管理员调整审批定义或准备控件所需值。",
        ),
        ConvertError::ConverterMismatch { widget_id, .. } => (
            widget_id,
            "转换器与控件类型不匹配",
            "请管理员修正字段映射。",
        ),
        ConvertError::InvalidDefinition(_) => {
            return "审批定义快照与字段映射不一致，请管理员刷新配置。".into()
        }
        ConvertError::UnknownConverter(_) => {
            return "字段映射的转换器不可用，请管理员修正配置。".into()
        }
    };
    let form: Vec<FormWidget> = serde_json::from_str(snapshot).unwrap_or_default();
    let field = widgets
        .iter()
        .find(|w| w.widget_id == *id)
        .map(|w| w.bitable_field.as_str())
        .unwrap_or_default();
    // 容器没有自己的映射行：改指它下面已映射的列，别让用户去找空字段。
    let container = if field.is_empty() {
        container_columns(&form, id, widgets, fields)
    } else {
        None
    };
    let (source, suggestion) = match container {
        Some(columns) => (columns, "请在多维表格补齐该容器下子控件对应的列值。"),
        None => (field_name(fields, field), suggestion),
    };
    format!("审批控件：{}\n来源字段：{}\n原因：{reason}\n处理：{suggestion}\n控件定位：{}；字段定位：{}",
        widget_name(&form, id), source, one_line(id, 100), one_line(field, 100))
}

/// 飞书原始响应可能含内部信息，用户只看业务码与固定说明。
pub(crate) fn outbound_reason(failure: &OutboundFailure) -> String {
    match failure.kind {
        FailureKind::Fatal { code } => format!(
            "{}（飞书错误码：{code}）",
            fatal_hint(code).unwrap_or("飞书拒绝本次请求，请管理员核对配置和权限")
        ),
        FailureKind::TokenExpired => "飞书应用凭证暂不可用，请管理员检查应用凭证。".into(),
        FailureKind::Retry { .. } => "飞书接口暂不可用或受频控限制，请稍后重试。".into(),
        FailureKind::UuidConflict => "审批 UUID 已被使用，需按原 UUID 核对实例。".into(),
        FailureKind::InstanceNotFound => "审批详情暂不可见，请稍后核对。".into(),
    }
}

#[allow(clippy::too_many_arguments)] // 8 个参数：两条通知链路的展示要素逐个平铺，收结构体只是挪位置。
pub(crate) fn render(
    status: FeedbackStatus,
    reason: &str,
    names: &str,
    record_id: Option<&str>,
    row_number: Option<usize>,
    clicked_at: Option<&str>,
    base: &str,
    table: &str,
) -> String {
    let mut lines = vec![format!("【{}】", status.heading()), names.to_string()];
    // 正文只写行号；`record_id` 只出现在最下面的技术定位行。
    if let Some(row) = row_number {
        lines.push(format!("记录：第 {row} 行"));
    }
    if let Some(clicked_at) = clicked_at {
        lines.push(format!("点击时间：{}", one_line(clicked_at, 40)));
    }
    lines.push(format!("说明：{}", truncate(reason, 950)));
    lines.push(format!("下一步：{}", status.suggestion()));
    lines.push(format!(
        "技术定位：Base={}；表={}{}",
        one_line(base, 100),
        one_line(table, 100),
        record_id
            .map(|id| format!("；记录={}", one_line(id, 100)))
            .unwrap_or_default()
    ));
    truncate(&lines.join("\n"), 2000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_have_safe_fallbacks_and_limits() {
        assert_eq!(display_name(None), "名称暂不可用");
        assert_eq!(display_name(Some("  ")), "名称暂不可用");
        assert_eq!(display_name(Some("  名称\n后半  ")), "名称 后半");
        assert_eq!(one_line(&"😀".repeat(101), 100).chars().count(), 100);
        let form: Vec<FormWidget> = serde_json::from_value(json!([{"id":"parent","name":"明细","children":[{"id":"empty"},{"id":"literal","name":"名称暂不可用"}]}])).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(widget_name(&form, "empty"), "明细 / 名称暂不可用");
        assert_eq!(widget_name(&form, "literal"), "明细 / 名称暂不可用");
        assert_eq!(widget_name(&form, "missing"), "名称暂不可用");
        let body = render(
            FeedbackStatus::Failed,
            "原因",
            "名称信息",
            Some("rec"),
            Some(5),
            Some("2026-10-26 10:32:55"),
            "base",
            "table",
        );
        assert!(body.contains("记录：第 5 行"), "{body}");
        assert!(!body.contains("记录：rec"), "正文不写记录 id：{body}");
        assert!(body.contains("点击时间：2026-10-26 10:32:55"), "{body}");
        assert!(!body.contains("摘要暂不可用"), "{body}");
        assert!(body.contains("技术定位：Base=base；表=table；记录=rec"));
        assert!(!render(
            FeedbackStatus::Accepted,
            "原因",
            "名称信息",
            None,
            None,
            None,
            "base",
            "table"
        )
        .contains("记录："));
        let body = render(
            FeedbackStatus::Failed,
            "原因",
            &"名称".repeat(2000),
            None,
            None,
            None,
            "base",
            "table",
        );
        assert_eq!(body.chars().count(), 2000);
    }

    #[test]
    fn headings_and_suggestions_distinguish_creation_backfill_and_acceptance() {
        for (status, heading, next) in [
            (
                FeedbackStatus::Succeeded,
                "审批实例已确认，编号已回填",
                "查看审批编号",
            ),
            (
                FeedbackStatus::Waiting,
                "本轮未创建审批，需要修正数据",
                "补齐数据",
            ),
            (
                FeedbackStatus::CreatedPendingBackfill,
                "审批实例已确认，回填尚未完成",
                "原审批 UUID",
            ),
            (
                FeedbackStatus::Retryable,
                "审批是否创建尚未确认",
                "先检查表格和审批记录",
            ),
            (FeedbackStatus::Failed, "本轮处理失败", "核对审批配置"),
            (FeedbackStatus::Accepted, "批量派发已受理", "所有启用配置"),
            (FeedbackStatus::Queued, "任务已在队列中", "无需重复点击"),
        ] {
            let body = render(
                status,
                "原因",
                "名称",
                Some("rec"),
                Some(1),
                None,
                "base",
                "table",
            );
            assert!(body.contains(heading) && body.contains(next), "{body}");
        }
    }

    #[test]
    fn conversion_variants_and_remote_failures_are_safe() {
        for error in [
            ConvertError::MissingRequired {
                widget_id: "w".into(),
            },
            ConvertError::BadValue {
                widget_id: "w".into(),
                reason: "private".into(),
            },
            ConvertError::UnmappedOption {
                widget_id: "w".into(),
                label: "private".into(),
            },
            ConvertError::Unsupported {
                widget_id: "w".into(),
                widget_type: "private".into(),
            },
            ConvertError::ConverterMismatch {
                widget_id: "w".into(),
                converter: "direct",
                widget_type: "private".into(),
            },
            ConvertError::InvalidDefinition("private".into()),
            ConvertError::UnknownConverter("private".into()),
        ] {
            let message = conversion_reason(&error, "invalid-json", &[], &[]);
            assert!(!message.contains("private"));
            assert!(message.contains("请"));
        }
        for kind in [
            FailureKind::Fatal { code: 9999 },
            FailureKind::TokenExpired,
            FailureKind::Retry {
                retry_after_seconds: None,
            },
            FailureKind::UuidConflict,
            FailureKind::InstanceNotFound,
        ] {
            assert!(!outbound_reason(&OutboundFailure {
                kind,
                message: "private".into()
            })
            .contains("private"));
        }
    }

    #[test]
    fn conversion_uses_nested_names_and_masks_raw_values() {
        let snapshot =
            r#"[{"id":"detail","name":"付款明细","children":[{"id":"date","name":"付款日期"}]}]"#;
        let widget = WidgetMap {
            widget_id: "date".into(),
            widget_type: "date".into(),
            required: true,
            bitable_field: "fld_date".into(),
            converter: super::super::approval_convert::Converter::Date,
            option_map: Default::default(),
            option_source_key: None,
            currency: None,
        };
        let fields = serde_json::from_value::<Vec<FieldItem>>(
            json!([{"field_id":"fld_date","field_name":"预计付款日"}]),
        )
        .unwrap_or_default();
        let message = conversion_reason(
            &ConvertError::BadValue {
                widget_id: "date".into(),
                reason: "secret-phone-json".into(),
            },
            snapshot,
            &[widget],
            &fields,
        );
        assert!(message.contains("付款明细 / 付款日期"));
        assert!(message.contains("预计付款日"));
        assert!(!message.contains("secret-phone-json"));
        let body = render(
            FeedbackStatus::Waiting,
            &message.repeat(20),
            "表格：测试",
            Some("rec"),
            Some(5),
            Some("2026-10-26 10:32:55"),
            "base",
            "table",
        );
        assert!(body.chars().count() <= 2000);
        assert!(body.contains("下一步："));
        let record = body
            .lines()
            .find_map(|line| line.strip_prefix("记录："))
            .unwrap_or_default();
        assert_eq!(record, "第 5 行", "记录行只写行号");
    }

    #[test]
    fn missing_required_detail_points_at_its_mapped_columns() {
        // 必填明细父级自身不进映射表，字段定位必然是空的。文案若还说
        // 「请填写来源字段」，就是在让用户去找一个不存在的字段——要改成
        // 列出该明细下**已映射**的列名。
        let snapshot = r#"[{"id":"detail","name":"付款明细/Payment Details","type":"fieldList","required":true,"children":[{"id":"fee","name":"费用类型/Fee Type","type":"radioV2"}]}]"#;
        let widget = WidgetMap {
            widget_id: "fee".into(),
            widget_type: "radioV2".into(),
            required: true,
            bitable_field: "fld_fee".into(),
            converter: super::super::approval_convert::Converter::Option,
            option_map: Default::default(),
            option_source_key: None,
            currency: None,
        };
        let fields = serde_json::from_value::<Vec<FieldItem>>(
            json!([{"field_id":"fld_fee","field_name":"费用类型"}]),
        )
        .unwrap_or_default();
        let message = conversion_reason(
            &ConvertError::MissingRequired {
                widget_id: "detail".into(),
            },
            snapshot,
            &[widget],
            &fields,
        );
        assert!(message.contains("付款明细/Payment Details"), "{message}");
        assert!(message.contains("费用类型"), "{message}");
        assert!(!message.contains("名称暂不可用"), "{message}");
        assert!(!message.contains("请填写来源字段"), "{message}");
    }

    #[test]
    fn missing_required_group_points_at_its_mapped_columns() {
        let snapshot = r#"[{"id":"leave","name":"请假","type":"leaveGroupV2","required":true,"value":[{"id":"widgetLeaveGroupReason","name":"请假事由","type":"textarea"}]}]"#;
        let widget = WidgetMap {
            widget_id: "widgetLeaveGroupReason".into(),
            widget_type: "textarea".into(),
            required: true,
            bitable_field: "fld_reason".into(),
            converter: super::super::approval_convert::Converter::Direct,
            option_map: Default::default(),
            option_source_key: None,
            currency: None,
        };
        let fields = serde_json::from_value::<Vec<FieldItem>>(
            json!([{"field_id":"fld_reason","field_name":"请假事由"}]),
        )
        .unwrap_or_default();
        let message = conversion_reason(
            &ConvertError::MissingRequired {
                widget_id: "leave".into(),
            },
            snapshot,
            &[widget],
            &fields,
        );
        assert!(message.contains("请假"), "{message}");
        assert!(message.contains("请假事由"), "{message}");
        assert!(!message.contains("请填写来源字段"), "{message}");
    }
}

//! 审批控件类型的统一规则表。
//!
//! 来源：飞书《审批实例表单控件参数》
//! <https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/approval-v4/instance/approval-instance-form-control-parameters>
//!
//! 配置期（按名匹配）与派发期（值组装、附件上传）共用这里的判据，避免各自
//! 维护一份清单后静默漂移。

#![allow(dead_code)] // 附件上传等消费者在后续步骤接入，规则先集中落地。

/// 不能通过实例 API 提单的控件类型。
///
/// 前 10 项来自官方文档「不支持通过 API 提单的控件」清单：`text` /
/// `mutableGroup` / `account` / `serialNumber` / `tripGroup` /
/// `apaascorehrOnboardingGroup` / `apaascorehrRegularateGroup` / `remedyGroupV2` /
/// `apaascorehrJobAdjustGroup` / `apaascorehrOffboardingGroup`。
/// `shiftGroup` / `shiftGroupV2` 官方支持，但本系统尚未实现控件组子控件映射；
/// `leaveGroup` / `remedyGroup` 是旧版控件组名，不在官方实例 API 支持范围内。
pub(crate) const UNSUPPORTED_TYPES: &[&str] = &[
    "text",
    "mutableGroup",
    "account",
    "serialNumber",
    "tripGroup",
    "apaascorehrOnboardingGroup",
    "apaascorehrRegularateGroup",
    "remedyGroupV2",
    "apaascorehrJobAdjustGroup",
    "apaascorehrOffboardingGroup",
    "shiftGroup",
    "shiftGroupV2",
    "leaveGroup",
    "remedyGroup",
];

/// 配置期必须显式拦下的「必填」控件类型。
///
/// `text` / `mutableGroup` / `serialNumber` 等仍按既有语义静默跳过：它们常出现在
/// 历史定义里，且飞书会接受缺省的非必填控件。下面这些一旦必填，省略后飞书会拒单
/// 或生成缺核心字段的实例，因此配置期就报错。
pub(crate) const CONFIG_REJECT_TYPES: &[&str] = &[
    "account",
    "remedyGroup",
    "remedyGroupV2",
    "leaveGroup",
    "shiftGroup",
    "shiftGroupV2",
];

/// 该类型是否必须在配置期拦下必填项。
pub(crate) fn must_reject_at_config(widget_type: &str) -> bool {
    CONFIG_REJECT_TYPES.contains(&widget_type)
}

/// 需要人工准备值的控件：旧 v1 附件、文档、地址。
///
/// `image` / `imageV2` 已改由附件上传链路自动处理，不再属于人工值。
pub(crate) const MANUAL_VALUE_TYPES: &[&str] = &["address", "attachment", "document"];

/// 容器类控件：自身不进映射表，只用于分组展示或携带子控件。
///
/// `fieldList` 的子控件在 `children` 里；三个控件组的子控件在定义 `form` 节点的
/// `value` 数组里（官方不是 `children`）。
pub(crate) const CONTAINER_TYPES: &[&str] = &["fieldList", "leaveGroupV2", "workGroup", "outGroup"];

/// 该类型是否不能通过实例 API 提单。
pub(crate) fn is_unsupported(widget_type: &str) -> bool {
    UNSUPPORTED_TYPES.contains(&widget_type)
}

/// 该类型是否需要人工准备值，不参与自动派发。
pub(crate) fn needs_manual_value(widget_type: &str) -> bool {
    MANUAL_VALUE_TYPES.contains(&widget_type)
}

/// 该类型是否是容器（自身不落映射）。
pub(crate) fn is_container(widget_type: &str) -> bool {
    CONTAINER_TYPES.contains(&widget_type)
}

/// 该类型是否是「控件组」（不含明细）。旧配置可能给控件组父级落过映射，
/// 派发时要忽略而不是报错；明细父级的旧映射仍按定义错误处理。
pub(crate) fn is_control_group(widget_type: &str) -> bool {
    matches!(widget_type, "leaveGroupV2" | "workGroup" | "outGroup")
}

/// 该类型是否出现在配置向导里（容器父级也展示）。
pub(crate) fn is_configurable(widget_type: &str) -> bool {
    !is_unsupported(widget_type)
}

/// 该类型是否会进映射表（容器父级只作分组展示）。
pub(crate) fn is_mappable(widget_type: &str) -> bool {
    !is_container(widget_type) && is_configurable(widget_type)
}

/// 该类型是否是媒体控件（走附件上传链路）。
pub(crate) fn is_media(widget_type: &str) -> bool {
    matches!(widget_type, "attachmentV2" | "image" | "imageV2")
}

/// 媒体控件上传到飞书时使用的文件类型：图片控件只能传图片。
pub(crate) fn upload_type(widget_type: &str) -> &'static str {
    if matches!(widget_type, "image" | "imageV2") {
        "image"
    } else {
        "attachment"
    }
}

/// 单个文件的大小上限：图片 10 MiB，其它附件 50 MiB。
pub(crate) fn max_upload_bytes(widget_type: &str) -> usize {
    if matches!(widget_type, "image" | "imageV2") {
        10 * 1024 * 1024
    } else {
        50 * 1024 * 1024
    }
}

/// 控件组的子控件白名单，按官方表单顺序排列。
///
/// 定义里控件组的子控件定义在 `value` 数组里；白名单之外的（说明、公式等）
/// 不参与映射，解析时直接丢弃。
pub(crate) fn group_child_ids(widget_type: &str) -> Option<&'static [&'static str]> {
    match widget_type {
        "leaveGroupV2" => Some(&[
            "widgetLeaveGroupType",
            "widgetLeaveGroupStartTime",
            "widgetLeaveGroupEndTime",
            "widgetLeaveGroupReason",
            "widgetLeaveCertification",
            "widgetLeaveGroupFeedingArrivingLate",
            "widgetLeaveGroupFeedingOffLeaveEarly",
        ]),
        "workGroup" => Some(&[
            "widgetWorkGroupOvertimeWorkers",
            "widgetWorkGroupType",
            "widgetWorkGroupTimeRangeFieldList",
            "widgetWorkGroupReason",
        ]),
        "outGroup" => Some(&[
            "widgetOutGroupType",
            "widgetOutGroupStartTime",
            "widgetOutGroupEndTime",
            "widgetOutGroupReason",
            "widgetOutGroupImage",
        ]),
        _ => None,
    }
}

/// 这两个子控件的值可能是飞书假勤/加班类型 ID（不是文案），`option` 可能为空，
/// 允许直取，跳过「选项无法从列里派生」的判定。
pub(crate) fn is_raw_option_child(widget_id: &str) -> bool {
    matches!(widget_id, "widgetLeaveGroupType" | "widgetWorkGroupType")
}

/// 不支持的控件类型对应的可读原因，用于配置期报错。
pub(crate) fn unsupported_reason(widget_type: &str) -> &'static str {
    match widget_type {
        "shiftGroup" | "shiftGroupV2" => "本系统暂未支持该控件组的子控件映射（官方实例 API 支持）",
        "leaveGroup" | "remedyGroup" => "该控件组版本不在官方实例 API 支持范围内",
        _ => "官方实例 API 不支持该控件",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_media_and_reason_rules_follow_the_official_tables() {
        for kind in CONTAINER_TYPES {
            assert!(is_container(kind) && !is_mappable(kind), "{kind}");
        }
        for kind in ["leaveGroupV2", "workGroup", "outGroup"] {
            assert!(is_control_group(kind), "{kind}");
        }
        assert!(!is_control_group("fieldList"));
        for kind in ["input", "radioV2", "attachmentV2", "image"] {
            assert!(is_configurable(kind) && !is_container(kind), "{kind}");
        }
        for kind in ["attachmentV2", "image", "imageV2"] {
            assert!(is_media(kind), "{kind}");
        }
        assert_eq!(upload_type("imageV2"), "image");
        assert_eq!(upload_type("attachmentV2"), "attachment");
        assert_eq!(max_upload_bytes("image"), 10 * 1024 * 1024);
        assert_eq!(max_upload_bytes("attachmentV2"), 50 * 1024 * 1024);
        assert_eq!(
            group_child_ids("leaveGroupV2").unwrap_or(&[]),
            &[
                "widgetLeaveGroupType",
                "widgetLeaveGroupStartTime",
                "widgetLeaveGroupEndTime",
                "widgetLeaveGroupReason",
                "widgetLeaveCertification",
                "widgetLeaveGroupFeedingArrivingLate",
                "widgetLeaveGroupFeedingOffLeaveEarly",
            ]
        );
        assert_eq!(
            group_child_ids("workGroup").unwrap_or(&[]),
            &[
                "widgetWorkGroupOvertimeWorkers",
                "widgetWorkGroupType",
                "widgetWorkGroupTimeRangeFieldList",
                "widgetWorkGroupReason",
            ]
        );
        assert_eq!(
            group_child_ids("outGroup").unwrap_or(&[]),
            &[
                "widgetOutGroupType",
                "widgetOutGroupStartTime",
                "widgetOutGroupEndTime",
                "widgetOutGroupReason",
                "widgetOutGroupImage",
            ]
        );
        assert!(group_child_ids("fieldList").is_none());
        for kind in UNSUPPORTED_TYPES {
            assert!(is_unsupported(kind), "{kind}");
        }
        for kind in ["leaveGroupV2", "workGroup", "outGroup"] {
            assert!(!is_unsupported(kind), "{kind}");
        }
        assert_eq!(MANUAL_VALUE_TYPES, &["address", "attachment", "document"]);
        for kind in CONFIG_REJECT_TYPES {
            assert!(must_reject_at_config(kind), "{kind}");
        }
        assert!(!must_reject_at_config("serialNumber"));
        assert!(is_raw_option_child("widgetWorkGroupType"));
        assert!(!is_raw_option_child("widgetWorkGroupReason"));
        assert_eq!(
            unsupported_reason("shiftGroup"),
            "本系统暂未支持该控件组的子控件映射（官方实例 API 支持）"
        );
        assert_eq!(
            unsupported_reason("remedyGroup"),
            "该控件组版本不在官方实例 API 支持范围内"
        );
        assert_eq!(unsupported_reason("account"), "官方实例 API 不支持该控件");
    }
}

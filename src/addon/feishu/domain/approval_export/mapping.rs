//! 字段映射：规则 + 映射行 + 实例详情/表单 + 目标列元数据 → 待写入记录。
//!
//! # 输出
//!
//! 每条明细行产出一个 [`TargetRow`]：`row_key = row:N`、`fields` 的键是**目标
//! 列名**（Bitable create 只认字段名，传 field_id 会报 `1254045`）、
//! `payload_hash` 是该行内容的稳定摘要。
//!
//! # 缺值/漂移/人工核对
//!
//! - 规则引用的控件在实例表单里不存在 → [`Drift`]（可选字段跳过，必填字段该行进
//!   [`RowReview`]）；
//! - 目标表的列被删/改名 → 整条规则 `Err(Review)`（列身份按 field_id 存，改名
//!   可解析，删除无法安全继续）；
//! - 目标列是公式/查找/关联列 → `Err(Fatal)`（这类列不可写，属配置错误）；
//! - 单行值形态不对（如日期列拿到非数字）→ 该行进 [`RowReview`]，其余行照常写。
//!
//! # 系统字段
//!
//! `instance_code` / `serial_number` / `status` / `start_time` / `end_time` /
//! `initiator` / `department` / `node_approver` / `node_time`。其中
//! `node_approver` 取触发节点上的审批人 open_id，`node_time` 取触发节点任务的
//! 最晚完成时间；最终通过模式用实例完成时间。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::super::approval::InstanceDetail;
use super::super::bitable::{encode_export_cell, resolve_field_name, FieldItem};
use super::form::{FixedNumber, FormNode, InstanceForm};
use super::idempotency::payload_hash;
use super::{Drift, ExportError, ExportRule, RowFilter, TriggerMode};

use super::super::approval_convert::labels_equivalent;

pub(crate) const KNOWN_SYSTEM_KEYS: &[&str] = &[
    "instance_code",
    "serial_number",
    "status",
    "start_time",
    "end_time",
    "initiator",
    "department",
    "node_approver",
    "node_time",
];

/// 目标列可写的字段类型：文本/数字/单选/多选/日期/复选框/人员/电话/超链接。
const WRITABLE_FIELD_TYPES: &[i32] = &[1, 2, 3, 4, 5, 7, 11, 13, 15];

/// 目标列类型是否可写（保存期校验与运行期共用同一份白名单）。
pub(crate) fn is_writable_field_type(field_type: i32) -> bool {
    WRITABLE_FIELD_TYPES.contains(&field_type)
}

/// 映射来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceKind {
    /// 明细行子控件。
    Detail,
    /// 顶层控件。
    Top,
    /// 系统字段（见模块文档）。
    System,
    /// 常量。
    Constant,
}

impl SourceKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Detail => "detail",
            Self::Top => "top",
            Self::System => "system",
            Self::Constant => "constant",
        }
    }

    pub(crate) fn parse(raw: &str) -> Result<Self, ExportError> {
        match raw.trim() {
            "detail" => Ok(Self::Detail),
            "top" => Ok(Self::Top),
            "system" => Ok(Self::System),
            "constant" => Ok(Self::Constant),
            other => Err(ExportError::Fatal(format!(
                "未知的来源类型「{other}」：只支持 detail / top / system / constant"
            ))),
        }
    }
}

/// 值转换器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Converter {
    Text,
    Number,
    Date,
    Person,
    OptionText,
    MultiOption,
    Checkbox,
    Url,
    Json,
}

impl Converter {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::Date => "date",
            Self::Person => "person",
            Self::OptionText => "option_text",
            Self::MultiOption => "multi_option",
            Self::Checkbox => "checkbox",
            Self::Url => "url",
            Self::Json => "json",
        }
    }

    pub(crate) fn parse(raw: &str) -> Result<Self, ExportError> {
        match raw.trim() {
            "text" => Ok(Self::Text),
            "number" => Ok(Self::Number),
            "date" => Ok(Self::Date),
            "person" => Ok(Self::Person),
            "option_text" => Ok(Self::OptionText),
            "multi_option" => Ok(Self::MultiOption),
            "checkbox" => Ok(Self::Checkbox),
            "url" => Ok(Self::Url),
            "json" => Ok(Self::Json),
            other => Err(ExportError::Fatal(format!(
                "未知的值转换器「{other}」：支持 text / number / date / person / \
                 option_text / multi_option / checkbox / url / json"
            ))),
        }
    }
}

/// `feishu_approval_export_field_map` 一行的领域形态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExportFieldMap {
    pub(crate) source_kind: SourceKind,
    /// detail/top 的控件 id；其余为空串。
    pub(crate) source_widget_id: String,
    /// system 的来源键；其余为空串。
    pub(crate) source_system_key: String,
    pub(crate) constant_value: Option<String>,
    pub(crate) target_field_id: String,
    pub(crate) converter: Converter,
    /// 选项文案映射：审批里的展示文案 → 目标列选项名。
    pub(crate) option_map: BTreeMap<String, String>,
    pub(crate) required: bool,
}

/// 一条待创建的目标记录。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TargetRow {
    /// `row:N`（N 为明细行下标，0 起）。
    pub(crate) row_key: String,
    pub(crate) row_index: usize,
    /// 键是**当前字段名**。
    pub(crate) fields: BTreeMap<String, Value>,
    pub(crate) payload_hash: String,
}

impl TargetRow {
    /// 转成 `create_record` 需要的形态。
    pub(crate) fn fields_map(&self) -> serde_json::Map<String, Value> {
        self.fields
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
}

/// 单行需要人工核对的原因（该行不写入，其余行不受影响）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RowReview {
    pub(crate) row_key: String,
    pub(crate) reason: String,
}

/// `build_rows` 的产物。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MappingPlan {
    pub(crate) rows: Vec<TargetRow>,
    /// 定义漂移；即使行能写，也要由调用方记录。
    pub(crate) drifts: Vec<Drift>,
    /// 被拦下的行。非空时调用方应把对应 run 行置 `review`，其余行仍可写。
    pub(crate) reviews: Vec<RowReview>,
}

/// 把规则命中的明细行映射成目标记录。
///
/// `detail` 用于系统字段（`instance_code` / `status` / 审批人 / 时间等）——
/// 这些值不在实例表单里，必须从实例详情取。
pub(crate) fn build_rows(
    rule: &ExportRule,
    maps: &[ExportFieldMap],
    detail: &InstanceDetail,
    form: &InstanceForm,
    target_fields: &[FieldItem],
) -> Result<MappingPlan, ExportError> {
    let Some(rows) = form.rows(&rule.detail_widget_id) else {
        return Err(ExportError::Review(format!(
            "实例表单里缺少明细控件「{}」：规则与当前审批定义可能已漂移",
            rule.detail_widget_id
        )));
    };
    if rows.is_empty() {
        return Ok(MappingPlan {
            rows: Vec::new(),
            drifts: Vec::new(),
            reviews: Vec::new(),
        });
    }

    // ---- 1. 解析目标列名与类型（配置级错误在这里一次性拦下） ----
    let mut resolved: Vec<(&ExportFieldMap, &FieldItem, String)> = Vec::new();
    for map in maps {
        if map.source_kind == SourceKind::System
            && !KNOWN_SYSTEM_KEYS.contains(&map.source_system_key.as_str())
        {
            return Err(ExportError::Review(format!(
                "映射里出现未知的系统字段键「{}」",
                map.source_system_key
            )));
        }
        let field_name =
            resolve_field_name(target_fields, &map.target_field_id).map_err(|error| {
                ExportError::Review(format!("{}（字段 id：{}）", error, map.target_field_id))
            })?;
        let field = target_fields
            .iter()
            .find(|field| field.field_id == map.target_field_id)
            .ok_or_else(|| {
                ExportError::Review(format!("目标表里找不到字段 id「{}」", map.target_field_id))
            })?;
        let Some(field_type) = field.field_type else {
            return Err(ExportError::Fatal(format!(
                "目标列「{field_name}」的类型未知，拒绝写入"
            )));
        };
        if !WRITABLE_FIELD_TYPES.contains(&field_type) {
            return Err(ExportError::Fatal(format!(
                "目标列「{field_name}」类型 {field_type} 不支持回写\
                 （公式/查找/关联/自动字段不可写；附件列请改用文本/超链接）"
            )));
        }
        resolved.push((map, field, field_name));
    }

    // ---- 2. 选行 ----
    let mut drifts: BTreeSet<Drift> = BTreeSet::new();
    let mut selected: Vec<usize> = Vec::new();
    for (index, _) in rows.iter().enumerate() {
        if rule.row_filter == RowFilter::AllRows {
            selected.push(index);
            continue;
        }
        let report = rule.condition.evaluate_row(form, index);
        drifts.extend(report.drifts);
        if report.matched {
            selected.push(index);
        }
    }

    // ---- 3. 逐行映射 ----
    let mut out_rows = Vec::new();
    let mut reviews = Vec::new();
    for row_index in selected {
        let mut fields: BTreeMap<String, Value> = BTreeMap::new();
        let mut reasons: Vec<String> = Vec::new();

        for (map, field, target_name) in &resolved {
            let extracted = extract_source(rule, map, detail, form, row_index);
            let source = match extracted {
                Extraction::Missing(drift) => {
                    drifts.insert(drift);
                    if map.required {
                        reasons.push(format!("缺少必填来源控件「{}」", source_label(map)));
                    }
                    continue;
                }
                Extraction::Empty => {
                    if map.required {
                        reasons.push(format!("必填来源「{}」的值为空", source_label(map)));
                    }
                    continue;
                }
                Extraction::Invalid(reason) => {
                    reasons.push(format!("来源「{}」{}", source_label(map), reason));
                    continue;
                }
                Extraction::Value(value) => value,
            };

            let Some(converted) = convert(map.converter, &source, &map.option_map) else {
                if map.required {
                    reasons.push(format!("必填来源「{}」无法转换出值", source_label(map)));
                }
                continue;
            };

            if matches!(
                map.converter,
                Converter::OptionText | Converter::MultiOption
            ) {
                if let Err(reason) = check_target_options(field, &converted) {
                    reasons.push(reason);
                    continue;
                }
            }

            match encode_export_cell(field.field_type, &converted) {
                Ok(Some(cell)) => {
                    if fields.insert(target_name.clone(), cell).is_some() {
                        return Err(ExportError::Fatal(format!(
                            "多条映射指向同一目标列「{target_name}」：字段名会互相覆盖"
                        )));
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    reasons.push(format!("目标列「{target_name}」：{}", error.message));
                }
            }
        }

        let row_key = format!("row:{row_index}");
        if !reasons.is_empty() {
            reviews.push(RowReview {
                row_key,
                reason: reasons.join("；"),
            });
            continue;
        }
        if fields.is_empty() {
            reviews.push(RowReview {
                row_key,
                reason: "没有任何字段映射出可写值".to_string(),
            });
            continue;
        }
        let hash = payload_hash(&row_key, &fields);
        out_rows.push(TargetRow {
            row_key,
            row_index,
            fields,
            payload_hash: hash,
        });
    }

    Ok(MappingPlan {
        rows: out_rows,
        drifts: drifts.into_iter().collect(),
        reviews,
    })
}

fn source_label(map: &ExportFieldMap) -> String {
    match map.source_kind {
        SourceKind::Detail => format!("明细控件「{}」", map.source_widget_id),
        SourceKind::Top => format!("顶层控件「{}」", map.source_widget_id),
        SourceKind::System => format!("系统字段「{}」", map.source_system_key),
        SourceKind::Constant => "常量".to_string(),
    }
}

/// 解析来源值。
enum Extraction {
    Missing(Drift),
    Empty,
    Invalid(String),
    Value(SourceValue),
}

/// 转换器看到的中性值。
enum SourceValue {
    Text(String),
    Number(FixedNumber),
    Bool(bool),
    Texts(Vec<String>),
    PersonIds(Vec<String>),
    Millis(i64),
    Urls(Vec<String>),
    Raw(Value),
}

fn extract_source(
    rule: &ExportRule,
    map: &ExportFieldMap,
    detail: &InstanceDetail,
    form: &InstanceForm,
    row_index: usize,
) -> Extraction {
    match map.source_kind {
        SourceKind::Detail => {
            match form.cell(&rule.detail_widget_id, row_index, &map.source_widget_id) {
                Some(node) => extract_from_node(node, map.converter),
                None => Extraction::Missing(Drift::MissingDetailCell {
                    detail_widget_id: rule.detail_widget_id.clone(),
                    widget_id: map.source_widget_id.clone(),
                }),
            }
        }
        SourceKind::Top => match form.top(&map.source_widget_id) {
            Some(node) => extract_from_node(node, map.converter),
            None => Extraction::Missing(Drift::MissingTopWidget {
                widget_id: map.source_widget_id.clone(),
            }),
        },
        SourceKind::System => extract_system(rule, detail, &map.source_system_key),
        SourceKind::Constant => extract_constant(map.constant_value.as_deref(), map.converter),
    }
}

fn extract_from_node(node: &FormNode, converter: Converter) -> Extraction {
    if node.is_empty() {
        return Extraction::Empty;
    }
    match converter {
        Converter::Text => Extraction::Value(SourceValue::Text(node.display_text())),
        Converter::Number => match node.number() {
            Some(number) => Extraction::Value(SourceValue::Number(number)),
            None => Extraction::Invalid("的值不是数字".to_string()),
        },
        Converter::Date => match node.millis() {
            Some(millis) => Extraction::Value(SourceValue::Millis(millis)),
            None => Extraction::Invalid("的值不是毫秒时间戳".to_string()),
        },
        Converter::Person => {
            let ids = node.person_ids();
            if ids.is_empty() {
                Extraction::Invalid("里没有人员 open_id".to_string())
            } else {
                Extraction::Value(SourceValue::PersonIds(ids))
            }
        }
        Converter::OptionText => match node.label() {
            Some(label) => Extraction::Value(SourceValue::Text(label)),
            None => Extraction::Empty,
        },
        Converter::MultiOption => {
            let labels = node.labels();
            if labels.is_empty() {
                Extraction::Empty
            } else {
                Extraction::Value(SourceValue::Texts(labels))
            }
        }
        Converter::Checkbox => match node.bool_value() {
            Some(flag) => Extraction::Value(SourceValue::Bool(flag)),
            None => Extraction::Invalid("的值不是布尔".to_string()),
        },
        Converter::Url => {
            let urls = node.urls();
            if urls.is_empty() {
                Extraction::Empty
            } else {
                Extraction::Value(SourceValue::Urls(urls))
            }
        }
        Converter::Json => Extraction::Value(SourceValue::Raw(node.raw().clone())),
    }
}

fn extract_system(rule: &ExportRule, detail: &InstanceDetail, key: &str) -> Extraction {
    match key {
        "instance_code" => text_value(&detail.instance_code),
        "serial_number" => optional_text_value(detail.serial_number.as_deref()),
        "status" => optional_text_value(detail.status.as_deref()),
        "start_time" => millis_value(detail.start_time.as_deref()),
        "end_time" => millis_value(detail.end_time.as_deref()),
        "initiator" => {
            let ids: Vec<String> = detail
                .open_id
                .iter()
                .chain(detail.user_id.iter().filter(|id| id.starts_with("ou_")))
                .cloned()
                .collect();
            if ids.is_empty() {
                Extraction::Empty
            } else {
                Extraction::Value(SourceValue::PersonIds(ids))
            }
        }
        "department" => optional_text_value(
            detail
                .department_name
                .as_deref()
                .or(detail.department_id.as_deref()),
        ),
        "node_approver" => {
            let ids = trigger_tasks(rule, detail)
                .flat_map(|task| {
                    task.open_id
                        .iter()
                        .chain(task.user_id.iter().filter(|id| id.starts_with("ou_")))
                        .cloned()
                })
                .collect::<Vec<_>>();
            if ids.is_empty() {
                Extraction::Empty
            } else {
                Extraction::Value(SourceValue::PersonIds(ids))
            }
        }
        "node_time" => {
            let millis = if rule.trigger_mode == TriggerMode::FinalApproved {
                parse_millis(detail.end_time.as_deref())
            } else {
                trigger_tasks(rule, detail)
                    .filter_map(|task| parse_millis(task.end_time.as_deref()))
                    .max()
            };
            match millis.filter(|value| *value > 0) {
                Some(millis) => Extraction::Value(SourceValue::Millis(millis)),
                None => Extraction::Empty,
            }
        }
        other => Extraction::Invalid(format!("是未知的系统字段键「{other}」")),
    }
}

fn trigger_tasks<'a>(
    rule: &'a ExportRule,
    detail: &'a InstanceDetail,
) -> impl Iterator<Item = &'a super::super::approval::TaskNode> + 'a {
    detail
        .task_list
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter(move |task| {
            // 只取真正通过的任务：OR 节点上未表态的审批人不该被写进「节点审批人」。
            let passed = matches!(task.status.as_deref(), Some("APPROVED") | Some("AUTO_PASS"));
            if !passed {
                return false;
            }
            if rule.trigger_mode == TriggerMode::FinalApproved {
                return task.node_id.is_some();
            }
            task.node_id
                .as_deref()
                .is_some_and(|node_id| rule.trigger_node_ids.iter().any(|id| id == node_id))
        })
}

fn extract_constant(raw: Option<&str>, converter: Converter) -> Extraction {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Extraction::Empty;
    };
    match converter {
        Converter::Text | Converter::OptionText | Converter::Json => {
            Extraction::Value(SourceValue::Text(text.to_string()))
        }
        Converter::Number => match FixedNumber::parse(text) {
            Some(number) => Extraction::Value(SourceValue::Number(number)),
            None => Extraction::Invalid("常量不是数字".to_string()),
        },
        Converter::Date => match text.parse::<i64>() {
            Ok(millis) => Extraction::Value(SourceValue::Millis(millis)),
            Err(_) => Extraction::Invalid("常量不是毫秒时间戳".to_string()),
        },
        Converter::Person => {
            let ids = split_ids(text);
            if ids.is_empty() {
                Extraction::Empty
            } else {
                Extraction::Value(SourceValue::PersonIds(ids))
            }
        }
        Converter::MultiOption => {
            let labels = serde_json::from_str::<Vec<String>>(text)
                .ok()
                .filter(|labels| !labels.is_empty())
                .unwrap_or_else(|| {
                    text.split([',', '，', '、', ';', '\n'])
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                        .collect()
                });
            if labels.is_empty() {
                Extraction::Empty
            } else {
                Extraction::Value(SourceValue::Texts(labels))
            }
        }
        Converter::Checkbox => match text.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "是" => Extraction::Value(SourceValue::Bool(true)),
            "false" | "0" | "no" | "否" => Extraction::Value(SourceValue::Bool(false)),
            _ => Extraction::Invalid("常量不是布尔".to_string()),
        },
        Converter::Url => Extraction::Value(SourceValue::Urls(split_ids(text))),
    }
}

fn split_ids(text: &str) -> Vec<String> {
    text.split([',', '，', ';', '、', ' ', '\n'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn text_value(text: &str) -> Extraction {
    if text.trim().is_empty() {
        Extraction::Empty
    } else {
        Extraction::Value(SourceValue::Text(text.trim().to_string()))
    }
}

fn optional_text_value(text: Option<&str>) -> Extraction {
    text.map(text_value).unwrap_or(Extraction::Empty)
}

fn millis_value(text: Option<&str>) -> Extraction {
    match parse_millis(text) {
        Some(millis) => Extraction::Value(SourceValue::Millis(millis)),
        None => Extraction::Empty,
    }
}

fn parse_millis(text: Option<&str>) -> Option<i64> {
    // 未完成实例的 end_time 是字符串 "0"（不是 null）；写进日期列会变成 1970。
    text?.trim().parse::<i64>().ok().filter(|value| *value > 0)
}

/// 把中性值按转换器落成可编码的 JSON 值。
///
/// `None` = 转换后没有可写值（例如可选的空选项）；是否因此报必填缺失由调用方决定。
fn convert(
    converter: Converter,
    source: &SourceValue,
    option_map: &BTreeMap<String, String>,
) -> Option<Value> {
    let mapped = |label: &str| {
        option_map
            .get(label)
            .cloned()
            .unwrap_or_else(|| label.to_string())
    };
    match converter {
        Converter::Text => Some(Value::String(as_text(source))),
        Converter::Number => match source {
            SourceValue::Number(number) => Some(number.to_json_value()),
            SourceValue::Millis(millis) => Some(Value::from(*millis)),
            SourceValue::Text(text) => FixedNumber::parse(text).map(FixedNumber::to_json_value),
            SourceValue::Bool(flag) => Some(Value::from(*flag)),
            _ => None,
        },
        Converter::Date => match source {
            SourceValue::Millis(millis) => Some(Value::from(*millis)),
            SourceValue::Number(number) => number.as_integer().map(Value::from),
            SourceValue::Text(text) => text.trim().parse::<i64>().ok().map(Value::from),
            _ => None,
        },
        Converter::Person => {
            let ids = match source {
                SourceValue::PersonIds(ids) => ids.clone(),
                SourceValue::Text(text) => split_ids(text),
                SourceValue::Texts(texts) => texts.clone(),
                _ => Vec::new(),
            };
            let ids: Vec<String> = ids.into_iter().filter(|id| id.starts_with("ou_")).collect();
            if ids.is_empty() {
                None
            } else {
                Some(Value::Array(
                    ids.into_iter()
                        .map(|id| serde_json::json!({ "id": id }))
                        .collect(),
                ))
            }
        }
        Converter::OptionText => {
            let label = as_text(source);
            if label.is_empty() {
                None
            } else {
                Some(Value::String(mapped(&label)))
            }
        }
        Converter::MultiOption => {
            let labels = match source {
                SourceValue::Texts(texts) => texts.clone(),
                SourceValue::Text(text) => split_ids(text),
                _ => Vec::new(),
            };
            if labels.is_empty() {
                None
            } else {
                Some(Value::Array(
                    labels
                        .into_iter()
                        .map(|label| Value::String(mapped(&label)))
                        .collect(),
                ))
            }
        }
        Converter::Checkbox => match source {
            SourceValue::Bool(flag) => Some(Value::from(*flag)),
            SourceValue::Text(text) => match text.trim().to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" | "是" => Some(Value::from(true)),
                "false" | "0" | "no" | "否" => Some(Value::from(false)),
                _ => None,
            },
            _ => None,
        },
        Converter::Url => {
            let urls = match source {
                SourceValue::Urls(urls) => urls.clone(),
                SourceValue::Text(text) => vec![text.clone()],
                _ => Vec::new(),
            };
            let urls: Vec<String> = urls
                .into_iter()
                .filter(|url| !url.trim().is_empty())
                .collect();
            if urls.is_empty() {
                None
            } else {
                Some(serde_json::json!({
                    "link": urls[0],
                    "text": urls.join("\n"),
                }))
            }
        }
        Converter::Json => match source {
            SourceValue::Raw(value) => Some(Value::String(
                serde_json::to_string(value).unwrap_or_default(),
            )),
            other => Some(Value::String(
                serde_json::to_string(&as_text(other)).unwrap_or_default(),
            )),
        },
    }
}

fn as_text(source: &SourceValue) -> String {
    match source {
        SourceValue::Text(text) => text.clone(),
        SourceValue::Number(number) => match number.to_json_value() {
            Value::Number(number) => number.to_string(),
            other => other.to_string(),
        },
        SourceValue::Bool(flag) => flag.to_string(),
        SourceValue::Texts(texts) => texts.join("、"),
        SourceValue::PersonIds(ids) => ids.join("、"),
        SourceValue::Millis(millis) => millis.to_string(),
        SourceValue::Urls(urls) => urls.join("\n"),
        SourceValue::Raw(value) => match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        },
    }
}

/// 目标列是单选/多选时，检查转换出的文案是否是该列的选项。
///
/// 选项列表为空（拿不到元数据）时不拦：宁可让 Bitable 报明确错误，也不要
/// 因为元数据缺失拒绝一条本来能写的记录。
fn check_target_options(field: &FieldItem, value: &Value) -> Result<(), String> {
    if !matches!(field.field_type, Some(3) | Some(4)) {
        return Ok(());
    }
    let options = field.option_names();
    if options.is_empty() {
        return Ok(());
    }
    let labels: Vec<String> = match value {
        Value::String(label) => vec![label.clone()],
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };
    for label in labels {
        if !options
            .iter()
            .any(|option| labels_equivalent(option, &label))
        {
            return Err(format!("目标列「{}」没有选项「{label}」", field.field_name));
        }
    }
    Ok(())
}

/// 解析 `option_map` 列的 JSON 对象文本（源文案 → 目标选项名）。
pub(crate) fn parse_option_map(raw: Option<&str>) -> Result<BTreeMap<String, String>, ExportError> {
    let Some(raw) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(BTreeMap::new());
    };
    let value: Value = serde_json::from_str(raw)
        .map_err(|error| ExportError::Fatal(format!("选项映射 JSON 解析失败：{error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| ExportError::Fatal("选项映射必须是 JSON 对象".to_string()))?;
    let mut map = BTreeMap::new();
    for (key, value) in object {
        let label = value
            .as_str()
            .ok_or_else(|| ExportError::Fatal(format!("选项映射「{key}」的值必须是字符串")))?;
        map.insert(key.clone(), label.to_string());
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::feishu::domain::approval::TaskNode;
    use crate::addon::feishu::domain::approval_export::{Condition, StatusLabels};
    use crate::addon::feishu::domain::bitable::{FieldProperty, PropertyOption};

    fn detail() -> InstanceDetail {
        let task = |open_id: &str, end_time: &str| TaskNode {
            id: Some(open_id.to_string()),
            node_id: Some("n1".to_string()),
            node_name: None,
            user_id: None,
            open_id: Some(open_id.to_string()),
            status: Some("APPROVED".to_string()),
            r#type: Some("AND".to_string()),
            start_time: Some("1000".to_string()),
            end_time: Some(end_time.to_string()),
        };
        InstanceDetail {
            instance_code: "INST-1".to_string(),
            approval_code: Some("code".to_string()),
            serial_number: Some("202611100001".to_string()),
            status: Some("APPROVED".to_string()),
            start_time: Some("1790750409857".to_string()),
            end_time: Some("1790751000000".to_string()),
            open_id: Some("ou_initiator".to_string()),
            user_id: None,
            department_id: Some("dept-1".to_string()),
            department_name: Some("财务部".to_string()),
            uuid: None,
            reverted: None,
            task_list: Some(vec![task("ou_a", "1790750500000")]),
            timeline: Some(Vec::new()),
            form: None,
        }
    }

    fn form() -> InstanceForm {
        InstanceForm::parse(Some(
            r#"[
                {"id":"title","type":"input","value":"付款申请"},
                {"id":"kind","type":"radioV2","value":"差旅","option":{"key":"o1","text":"差旅"}},
                {"id":"files","type":"attachmentV2","value":["https://a/1","https://a/2"]},
                {"id":"detail","type":"fieldList","value":[
                    [{"id":"note","type":"input","value":"机票"},
                     {"id":"money","type":"amount","value":80.25},
                     {"id":"owner","type":"contact","value":[{"id":"ou_x"},{"id":"ou_y"}]}],
                    [{"id":"note","type":"input","value":"酒店"},
                     {"id":"money","type":"amount","value":20.25}]
                ]}
            ]"#,
        ))
        .unwrap_or_else(|error| panic!("{error}"))
    }

    fn rule(condition: &str, row_filter: RowFilter) -> ExportRule {
        ExportRule {
            id: 7,
            title: "付款回写".to_string(),
            approval_code: "code".to_string(),
            trigger_mode: TriggerMode::NodePassed,
            trigger_node_ids: vec!["n1".to_string()],
            require_final_approval: true,
            detail_widget_id: "detail".to_string(),
            row_filter,
            condition: Condition::parse(condition).unwrap_or_else(|error| panic!("{error}")),
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

    fn map(kind: SourceKind, widget: &str, target: &str, converter: Converter) -> ExportFieldMap {
        ExportFieldMap {
            source_kind: kind,
            source_widget_id: widget.to_string(),
            source_system_key: String::new(),
            constant_value: None,
            target_field_id: target.to_string(),
            converter,
            option_map: BTreeMap::new(),
            required: false,
        }
    }

    fn system_map(key: &str, target: &str, converter: Converter) -> ExportFieldMap {
        ExportFieldMap {
            source_system_key: key.to_string(),
            ..map(SourceKind::System, "", target, converter)
        }
    }

    fn field(id: &str, name: &str, field_type: i32) -> FieldItem {
        FieldItem {
            is_primary: false,
            field_id: id.to_string(),
            field_name: name.to_string(),
            field_type: Some(field_type),
            ui_type: None,
            property: None,
        }
    }

    fn select_field(id: &str, name: &str, field_type: i32, options: &[&str]) -> FieldItem {
        FieldItem {
            property: Some(FieldProperty {
                options: options
                    .iter()
                    .map(|option| PropertyOption {
                        name: (*option).to_string(),
                    })
                    .collect(),
                table_id: None,
            }),
            ..field(id, name, field_type)
        }
    }

    fn target_fields() -> Vec<FieldItem> {
        vec![
            field("fld_note", "备注", 1),
            field("fld_money", "付款金额", 2),
            field("fld_title", "标题", 1),
            field("fld_code", "实例编码", 1),
            field("fld_kind", "类型", 1),
            field("fld_owner", "负责人", 11),
            field("fld_link", "链接", 15),
            field("fld_text_files", "附件文本", 1),
        ]
    }

    fn maps() -> Vec<ExportFieldMap> {
        vec![
            map(SourceKind::Detail, "note", "fld_note", Converter::Text),
            map(SourceKind::Detail, "money", "fld_money", Converter::Number),
            map(SourceKind::Top, "title", "fld_title", Converter::Text),
            system_map("instance_code", "fld_code", Converter::Text),
            map(SourceKind::Top, "kind", "fld_kind", Converter::OptionText),
            map(SourceKind::Detail, "owner", "fld_owner", Converter::Person),
            map(SourceKind::Top, "files", "fld_link", Converter::Url),
            map(SourceKind::Top, "files", "fld_text_files", Converter::Text),
        ]
    }

    #[test]
    fn maps_every_detail_row_with_field_names() {
        let plan = build_rows(
            &rule("", RowFilter::AllRows),
            &maps(),
            &detail(),
            &form(),
            &target_fields(),
        )
        .unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(plan.rows.len(), 2);
        assert_eq!(plan.rows[0].row_key, "row:0");
        assert_eq!(plan.rows[1].row_key, "row:1");
        assert_eq!(
            plan.rows[0].fields.get("备注"),
            Some(&serde_json::json!("机票"))
        );
        assert_eq!(
            plan.rows[0].fields.get("付款金额"),
            Some(&serde_json::json!(80.25))
        );
        assert_eq!(
            plan.rows[0].fields.get("实例编码"),
            Some(&serde_json::json!("INST-1"))
        );
        assert_eq!(
            plan.rows[0].fields.get("类型"),
            Some(&serde_json::json!("差旅"))
        );
        assert_eq!(
            plan.rows[0].fields.get("负责人"),
            Some(&serde_json::json!([{"id":"ou_x"},{"id":"ou_y"}]))
        );
        let link = plan.rows[0]
            .fields
            .get("链接")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("超链接应是对象"));
        assert_eq!(link.get("link"), Some(&serde_json::json!("https://a/1")));
        assert_eq!(
            plan.rows[0].fields.get("附件文本"),
            Some(&serde_json::json!("https://a/1\nhttps://a/2"))
        );
        assert_eq!(plan.rows[0].payload_hash.len(), 64);
        assert_ne!(plan.rows[0].payload_hash, plan.rows[1].payload_hash);

        assert_eq!(plan.rows.len(), 2);
        assert!(plan.reviews.is_empty(), "可选字段缺失只记漂移，不拦行");
        assert!(!plan.rows[1].fields.contains_key("负责人"));
        assert!(plan.drifts.iter().any(|drift| matches!(
            drift,
            Drift::MissingDetailCell { widget_id, .. } if widget_id == "owner"
        )));
    }

    #[test]
    fn matched_rows_only_writes_rows_that_satisfy_the_row_condition() {
        let condition = r#"{"items":[{"scope":"detail","detail_widget_id":"detail",
            "match":"all","items":[{"widget_id":"money","op":"gt","value":50}]}]}"#;
        let plan = build_rows(
            &rule(condition, RowFilter::MatchedRows),
            &maps()[0..2],
            &detail(),
            &form(),
            &target_fields(),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(plan.rows.len(), 1);
        assert_eq!(plan.rows[0].row_key, "row:0");
    }

    #[test]
    fn required_missing_cell_reviews_only_that_row() {
        let mut owner = map(SourceKind::Detail, "owner", "fld_owner", Converter::Person);
        owner.required = true;
        let plan = build_rows(
            &rule("", RowFilter::AllRows),
            &[maps()[0].clone(), owner],
            &detail(),
            &form(),
            &target_fields(),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(plan.rows.len(), 1);
        assert_eq!(plan.rows[0].row_key, "row:0");
        assert_eq!(plan.reviews.len(), 1);
        assert_eq!(plan.reviews[0].row_key, "row:1");
        assert!(plan.reviews[0].reason.contains("owner"));
    }

    #[test]
    fn missing_target_field_is_review() {
        let error = match build_rows(
            &rule("", RowFilter::AllRows),
            &[map(SourceKind::Detail, "note", "fld_gone", Converter::Text)],
            &detail(),
            &form(),
            &target_fields(),
        ) {
            Ok(_) => panic!("缺列必须拦下"),
            Err(error) => error,
        };
        assert!(matches!(error, ExportError::Review(_)), "{error:?}");
    }

    #[test]
    fn formula_target_column_is_fatal() {
        let mut fields = target_fields();
        fields.push(field("fld_formula", "公式", 20));
        let error = match build_rows(
            &rule("", RowFilter::AllRows),
            &[map(
                SourceKind::Detail,
                "note",
                "fld_formula",
                Converter::Text,
            )],
            &detail(),
            &form(),
            &fields,
        ) {
            Ok(_) => panic!("公式列不可写"),
            Err(error) => error,
        };
        assert!(matches!(error, ExportError::Fatal(_)), "{error:?}");
    }

    #[test]
    fn option_map_rewrites_labels_and_unknown_options_are_reviewed() {
        let mut kind = map(SourceKind::Top, "kind", "fld_kind", Converter::OptionText);
        kind.option_map
            .insert("差旅".to_string(), "交通差旅".to_string());
        let fields = vec![select_field("fld_kind", "类型", 3, &["交通差旅"])];
        let plan = build_rows(
            &rule("", RowFilter::AllRows),
            &[kind.clone()],
            &detail(),
            &form(),
            &fields,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            plan.rows[0].fields.get("类型"),
            Some(&serde_json::json!("交通差旅"))
        );

        let fields = vec![select_field("fld_kind", "类型", 3, &["其它"])];
        let plan = build_rows(
            &rule("", RowFilter::AllRows),
            &[kind],
            &detail(),
            &form(),
            &fields,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert!(plan.rows.is_empty());
        assert!(plan.reviews[0].reason.contains("没有选项"));
    }

    #[test]
    fn system_fields_cover_person_and_node_time() {
        let mut fields = target_fields();
        fields.push(field("fld_person", "审批人", 11));
        fields.push(field("fld_node_time", "节点完成时间", 5));
        let plan = build_rows(
            &rule("", RowFilter::AllRows),
            &[
                system_map("node_approver", "fld_person", Converter::Person),
                system_map("node_time", "fld_node_time", Converter::Date),
            ],
            &detail(),
            &form(),
            &fields,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            plan.rows[0].fields.get("审批人"),
            Some(&serde_json::json!([{"id":"ou_a"}]))
        );
        assert_eq!(
            plan.rows[0].fields.get("节点完成时间"),
            Some(&serde_json::json!(1790750500000i64))
        );
    }

    #[test]
    fn option_map_parser_rejects_non_objects() {
        assert!(parse_option_map(Some("[]")).is_err());
        assert!(parse_option_map(Some(r#"{"a":1}"#)).is_err());
        assert_eq!(
            parse_option_map(Some(r#"{"a":"b"}"#))
                .unwrap_or_default()
                .get("a"),
            Some(&"b".to_string())
        );
    }
}

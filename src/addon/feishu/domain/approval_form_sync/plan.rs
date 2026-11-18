//! 全字段反向映射：审批实例表单 → 目标多维表单元格。
//!
//! 纯领域逻辑；外部选项/部门/附件上传通过 trait 注入，测试用假实现即可覆盖
//! 所有分支，不触网、不连库。

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;

use crate::addon::feishu::domain::approval_convert::{connection_codes, WidgetMap};
use crate::addon::feishu::domain::approval_export::form::{AttachmentRef, FormNode, InstanceForm};
use crate::addon::feishu::domain::bitable::FieldItem;
use crate::addon::feishu::domain::widget_rules;

use super::error::SyncError;
use super::value::{parse_numeric_text, PreparedAttachment, SyncValue};

/// 外部选项反查。
#[async_trait]
pub(crate) trait OptionResolver: Send + Sync {
    /// `option_id → 展示文案`；查不到返回 `None`。
    async fn label_for_id(
        &self,
        source_key: &str,
        option_id: &str,
    ) -> Result<Option<String>, SyncError>;

    /// `展示文案 → option_id`；用于只有文案的回读形态。
    async fn id_for_label(
        &self,
        source_key: &str,
        label: &str,
    ) -> Result<Option<String>, SyncError>;
}

/// 部门 ID → 名称；解析失败返回 `None`，调用方退化为写 ID。
#[async_trait]
pub(crate) trait DepartmentResolver: Send + Sync {
    async fn name_for_id(&self, department_id: &str) -> Result<Option<String>, SyncError>;
}

/// 审批签名 URL → 多维表 `file_token`。
#[async_trait]
pub(crate) trait AttachmentPreparer: Send + Sync {
    async fn prepare(
        &self,
        widget_id: &str,
        widget_type: &str,
        files: &[AttachmentRef],
    ) -> Result<Vec<PreparedAttachment>, SyncError>;
}

/// 一次映射的输入。
pub(crate) struct PlanInput<'a> {
    pub(crate) record_id: &'a str,
    pub(crate) task_id: Option<i64>,
    pub(crate) form: &'a InstanceForm,
    pub(crate) widgets: &'a [WidgetMap],
    pub(crate) fields: &'a [FieldItem],
    /// Base 时区偏移（秒），日期/日期区间解析用。
    pub(crate) timezone_offset_seconds: i32,
    pub(crate) allow_clear: bool,
    /// 明细行下标；非聚合通常为 `Some(0)`，顶层控件忽略。
    pub(crate) row_index: Option<usize>,
    pub(crate) options: &'a dyn OptionResolver,
    pub(crate) departments: &'a dyn DepartmentResolver,
    pub(crate) attachments: &'a dyn AttachmentPreparer,
}

/// 一条记录要写的字段。
#[derive(Debug, Clone)]
pub(crate) struct FieldPatch {
    pub(crate) field_id: String,
    pub(crate) target_type: i32,
    pub(crate) value: SyncValue,
}

/// 被跳过的字段（定义漂移、未开清空、单列区间等），不是失败。
#[derive(Debug, Clone)]
pub(crate) struct SkippedField {
    pub(crate) field_id: String,
    pub(crate) widget_id: String,
    pub(crate) reason: &'static str,
}

/// 需要人工核对的字段。
#[derive(Debug, Clone)]
pub(crate) struct ReviewField {
    pub(crate) field_id: String,
    pub(crate) error: SyncError,
}

/// 只读列（公式/查找）的期望值；不写入，写后回读比对。
#[derive(Debug, Clone)]
pub(crate) struct VerifyField {
    pub(crate) field_id: String,
    pub(crate) expected: SyncValue,
}

/// 一条记录的反向映射结果。
#[derive(Debug, Clone)]
pub(crate) struct RecordPlan {
    pub(crate) record_id: String,
    pub(crate) task_id: Option<i64>,
    pub(crate) patches: Vec<FieldPatch>,
    pub(crate) skipped: Vec<SkippedField>,
    pub(crate) reviews: Vec<ReviewField>,
    pub(crate) verifications: Vec<VerifyField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Location {
    Top,
    Detail {
        detail_id: String,
        row_indices: Vec<usize>,
    },
    Missing,
    Ambiguous,
}

/// 构建一条记录的反向映射。
pub(crate) async fn build_single(input: PlanInput<'_>) -> RecordPlan {
    let mut plan = RecordPlan {
        record_id: input.record_id.to_string(),
        task_id: input.task_id,
        patches: Vec::new(),
        skipped: Vec::new(),
        reviews: Vec::new(),
        verifications: Vec::new(),
    };

    let duplicates = duplicate_targets(input.widgets);

    for widget in input.widgets {
        if widget_rules::is_container(&widget.widget_type) {
            continue;
        }
        if duplicates.contains(&widget.bitable_field)
            || widget
                .interval_end_field
                .as_ref()
                .is_some_and(|field| duplicates.contains(field))
        {
            plan.reviews.push(ReviewField {
                field_id: widget.bitable_field.clone(),
                error: SyncError::DuplicateTarget {
                    field_id: widget.bitable_field.clone(),
                },
            });
            continue;
        }

        let Some(field) = target_field(input.fields, &widget.bitable_field) else {
            plan.reviews.push(ReviewField {
                field_id: widget.bitable_field.clone(),
                error: SyncError::FieldNotFound {
                    field_id: widget.bitable_field.clone(),
                },
            });
            continue;
        };
        let Some(field_type) = field.field_type else {
            plan.reviews.push(ReviewField {
                field_id: widget.bitable_field.clone(),
                error: SyncError::UnsupportedTarget {
                    field_id: widget.bitable_field.clone(),
                    field_type: None,
                },
            });
            continue;
        };
        let read_only = matches!(field_type, 19 | 20);
        if !read_only && !target_supported(&widget.widget_type, field_type) {
            plan.reviews.push(ReviewField {
                field_id: widget.bitable_field.clone(),
                error: SyncError::UnsupportedTarget {
                    field_id: widget.bitable_field.clone(),
                    field_type: Some(field_type),
                },
            });
            continue;
        }

        let location = locate(input.form, &widget.widget_id);
        let node = match node_for(&input, widget, &location) {
            Ok(Some(node)) => node,
            Ok(None) => {
                if widget.required {
                    plan.reviews.push(ReviewField {
                        field_id: widget.bitable_field.clone(),
                        error: SyncError::MissingWidget {
                            widget_id: widget.widget_id.clone(),
                        },
                    });
                } else {
                    plan.skipped.push(SkippedField {
                        field_id: widget.bitable_field.clone(),
                        widget_id: widget.widget_id.clone(),
                        reason: "missing_widget",
                    });
                }
                continue;
            }
            Err(error) => {
                plan.reviews.push(ReviewField {
                    field_id: widget.bitable_field.clone(),
                    error,
                });
                continue;
            }
        };

        let value = match extract_value(&input, widget, &node, field_type).await {
            Ok(value) => value.normalized(),
            Err(error) => {
                plan.reviews.push(ReviewField {
                    field_id: widget.bitable_field.clone(),
                    error,
                });
                continue;
            }
        };

        // 公式/查找列是只读派生值：不写，只记录期望值，由 apply 层回读比对。
        if read_only {
            plan.verifications.push(VerifyField {
                field_id: widget.bitable_field.clone(),
                expected: value,
            });
            continue;
        }

        // 单选控件的文案形态是 Number 时按十进制解析；解析不了再转 review。
        let value = if field_type == 2
            && matches!(widget.widget_type.as_str(), "radio" | "radioV2" | "select")
        {
            match &value {
                SyncValue::Labels(labels) if labels.len() == 1 => {
                    match parse_numeric_text(labels[0].trim()) {
                        Some(number) => SyncValue::Number(number),
                        None => {
                            plan.reviews.push(ReviewField {
                                field_id: widget.bitable_field.clone(),
                                error: SyncError::InvalidValue {
                                    widget_id: widget.widget_id.clone(),
                                    reason: "单选选项不是十进制数字",
                                },
                            });
                            continue;
                        }
                    }
                }
                _ => value,
            }
        } else {
            value
        };

        // 日期区间：配了结束列就拆成两个日期补丁；没配就把对象交给 encode
        // 写开始日期，并记一条 `end_not_written`。
        if widget.widget_type == "dateInterval" {
            match &value {
                SyncValue::DateInterval { start, end } => {
                    if let Some(end_field_id) = widget.interval_end_field.as_deref() {
                        let Some(end_field) = target_field(input.fields, end_field_id) else {
                            plan.reviews.push(ReviewField {
                                field_id: end_field_id.to_string(),
                                error: SyncError::FieldNotFound {
                                    field_id: end_field_id.to_string(),
                                },
                            });
                            continue;
                        };
                        if end_field.field_type != Some(5) {
                            plan.reviews.push(ReviewField {
                                field_id: end_field_id.to_string(),
                                error: SyncError::UnsupportedTarget {
                                    field_id: end_field_id.to_string(),
                                    field_type: end_field.field_type,
                                },
                            });
                            continue;
                        }
                        plan.patches.push(FieldPatch {
                            field_id: widget.bitable_field.clone(),
                            target_type: 5,
                            value: SyncValue::Millis(*start),
                        });
                        plan.patches.push(FieldPatch {
                            field_id: end_field_id.to_string(),
                            target_type: 5,
                            value: SyncValue::Millis(*end),
                        });
                        continue;
                    }
                    plan.skipped.push(SkippedField {
                        field_id: widget.bitable_field.clone(),
                        widget_id: widget.widget_id.clone(),
                        reason: "end_not_written",
                    });
                }
                SyncValue::Empty if input.allow_clear => {
                    if let Some(end_field_id) = widget.interval_end_field.as_deref() {
                        plan.patches.push(FieldPatch {
                            field_id: widget.bitable_field.clone(),
                            target_type: 5,
                            value: SyncValue::Empty,
                        });
                        plan.patches.push(FieldPatch {
                            field_id: end_field_id.to_string(),
                            target_type: 5,
                            value: SyncValue::Empty,
                        });
                        continue;
                    }
                }
                _ => {}
            }
        }

        if value.is_empty() && widget.required {
            plan.reviews.push(ReviewField {
                field_id: widget.bitable_field.clone(),
                error: SyncError::EmptyRequired {
                    widget_id: widget.widget_id.clone(),
                },
            });
            continue;
        }
        if value.is_empty() && !input.allow_clear {
            plan.skipped.push(SkippedField {
                field_id: widget.bitable_field.clone(),
                widget_id: widget.widget_id.clone(),
                reason: "clear_disabled",
            });
            continue;
        }

        // 自由文本写单选列：只接受目标列已有的选项，绝不自动新增选项。
        if field_type == 3
            && matches!(widget.widget_type.as_str(), "input" | "textarea")
            && !value.is_empty()
        {
            let SyncValue::Text(text) = &value else {
                plan.reviews.push(ReviewField {
                    field_id: widget.bitable_field.clone(),
                    error: SyncError::InvalidValue {
                        widget_id: widget.widget_id.clone(),
                        reason: "单选列的值不是文本",
                    },
                });
                continue;
            };
            let matched = field
                .option_names()
                .iter()
                .any(|option| option.trim() == text.trim());
            if !matched {
                plan.reviews.push(ReviewField {
                    field_id: widget.bitable_field.clone(),
                    error: SyncError::OptionNotFound {
                        field_id: widget.bitable_field.clone(),
                    },
                });
                continue;
            }
        }

        // 写单选列时按选项名去空格，避免“匹配成功但写入带空格文案”。
        let value = if field_type == 3
            && matches!(widget.widget_type.as_str(), "input" | "textarea")
            && !value.is_empty()
        {
            match value {
                SyncValue::Text(text) => SyncValue::Text(text.trim().to_string()),
                other => other,
            }
        } else {
            value
        };

        // 超链接列只能放一个实例 Code；多个时写第一个并留一条跳过记录。
        if field_type == 15 {
            if let SyncValue::ConnectCodes(codes) = &value {
                if codes.len() > 1 {
                    plan.skipped.push(SkippedField {
                        field_id: widget.bitable_field.clone(),
                        widget_id: widget.widget_id.clone(),
                        reason: "multiple_connect_codes",
                    });
                    plan.patches.push(FieldPatch {
                        field_id: widget.bitable_field.clone(),
                        target_type: field_type,
                        value: SyncValue::ConnectCodes(vec![codes[0].clone()]),
                    });
                    continue;
                }
            }
        }

        plan.patches.push(FieldPatch {
            field_id: widget.bitable_field.clone(),
            target_type: field_type,
            value,
        });
    }

    plan
}

/// 控件出现在哪些明细里；顶层（含控件组子控件）直接返回 `Top`。
pub(crate) fn locate(form: &InstanceForm, widget_id: &str) -> Location {
    if form.top(widget_id).is_some() {
        return Location::Top;
    }
    let mut found: Vec<(String, Vec<usize>)> = Vec::new();
    for detail_id in form.detail_ids() {
        let Some(rows) = form.rows(&detail_id) else {
            continue;
        };
        let indices: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                row.cells
                    .values()
                    .any(|cell| cell.find_descendant(widget_id).is_some())
            })
            .map(|(index, _)| index)
            .collect();
        if !indices.is_empty() {
            found.push((detail_id, indices));
        }
    }
    match found.len() {
        0 => Location::Missing,
        1 => {
            let (detail_id, row_indices) = found.remove(0);
            Location::Detail {
                detail_id,
                row_indices,
            }
        }
        _ => Location::Ambiguous,
    }
}

fn node_for(
    input: &PlanInput<'_>,
    widget: &WidgetMap,
    location: &Location,
) -> Result<Option<FormNode>, SyncError> {
    match location {
        Location::Top => Ok(input.form.top(&widget.widget_id).cloned()),
        Location::Detail {
            detail_id,
            row_indices,
        } => {
            let Some(rows) = input.form.rows(detail_id) else {
                return Err(SyncError::RowKeyUnresolved {
                    detail_id: detail_id.clone(),
                });
            };
            let index = if input.form.is_nested_detail(detail_id) {
                // 控件组内的明细整单共享：正向只产出一行，反向不支持多行。
                if rows.len() != 1 {
                    return Err(SyncError::Review {
                        reason: "nested_detail_multiple_rows",
                    });
                }
                0
            } else {
                match input.row_index {
                    Some(index) => index,
                    None if rows.len() == 1 => 0,
                    None => {
                        return Err(SyncError::RowCountMismatch {
                            expected: 1,
                            actual: rows.len(),
                        })
                    }
                }
            };
            if index >= rows.len() || !row_indices.contains(&index) {
                return Err(SyncError::RowCountMismatch {
                    expected: row_indices.len(),
                    actual: rows.len(),
                });
            }
            Ok(rows[index]
                .cells
                .values()
                .find_map(|cell| cell.find_descendant(&widget.widget_id)))
        }
        Location::Missing => Ok(None),
        Location::Ambiguous => Err(SyncError::Review {
            reason: "控件出现在多个明细控件里",
        }),
    }
}

async fn extract_value(
    input: &PlanInput<'_>,
    widget: &WidgetMap,
    node: &FormNode,
    field_type: i32,
) -> Result<SyncValue, SyncError> {
    if node.is_empty() {
        return Ok(SyncValue::Empty);
    }
    let widget_id = widget.widget_id.clone();
    match widget.widget_type.as_str() {
        "input" | "textarea" => Ok(SyncValue::Text(node.display_text())),
        "number" | "amount" => {
            node.number()
                .map(SyncValue::Number)
                .ok_or(SyncError::InvalidValue {
                    widget_id,
                    reason: "数字控件不是十进制数字",
                })
        }
        "formula" => {
            if let Some(number) = node.number() {
                Ok(SyncValue::Number(number))
            } else {
                let text = node.display_text();
                if text.trim().is_empty() {
                    Ok(SyncValue::Empty)
                } else {
                    Ok(SyncValue::Text(text))
                }
            }
        }
        "date" => {
            let text = node.display_text();
            if text.trim() == "0" || text.trim() == "0.0" {
                return Ok(SyncValue::Empty);
            }
            node.rfc3339_millis(input.timezone_offset_seconds)
                .map(SyncValue::Millis)
                .ok_or(SyncError::InvalidValue {
                    widget_id,
                    reason: "日期不是合法 RFC3339",
                })
        }
        "dateInterval" => node
            .date_interval_millis(input.timezone_offset_seconds)
            .map(|(start, end)| SyncValue::DateInterval { start, end })
            .ok_or(SyncError::InvalidValue {
                widget_id,
                reason: "日期区间不是 {start,end}",
            }),
        "radio" | "radioV2" | "select" => {
            let labels = resolve_labels(node, widget, input.options).await?;
            if labels.len() != 1 {
                return Err(SyncError::InvalidValue {
                    widget_id,
                    reason: "单选控件回读到多个选项",
                });
            }
            Ok(SyncValue::Labels(labels))
        }
        "checkbox" | "checkboxV2" | "multiSelect" => {
            let labels = resolve_labels(node, widget, input.options).await?;
            Ok(if labels.is_empty() {
                SyncValue::Empty
            } else {
                SyncValue::Labels(labels)
            })
        }
        "telephone" => node
            .telephone_text()
            .map(SyncValue::Text)
            .ok_or(SyncError::InvalidValue {
                widget_id,
                reason: "电话控件没有可读号码",
            }),
        "contact" => {
            let ids = node.person_open_ids();
            if !ids.is_empty() {
                Ok(SyncValue::PersonIds(ids))
            } else if node.person_ids().is_empty() {
                Ok(SyncValue::Empty)
            } else {
                Err(SyncError::InvalidValue {
                    widget_id,
                    reason: "只有 user_id，无法写人员列",
                })
            }
        }
        "department" => {
            let ids = node.person_ids();
            if ids.is_empty() {
                return Ok(SyncValue::Empty);
            }
            let mut names = Vec::with_capacity(ids.len());
            for id in ids {
                names.push(
                    input
                        .departments
                        .name_for_id(&id)
                        .await?
                        .unwrap_or_else(|| id.clone()),
                );
            }
            Ok(SyncValue::Text(names.join("、")))
        }
        "connect" => connection_codes(&node.value)
            .map(SyncValue::ConnectCodes)
            .map_err(|_| SyncError::InvalidValue {
                widget_id,
                reason: "关联审批值不是实例 Code",
            }),
        "attachmentV2" | "image" | "imageV2" | "attachment" | "document" | "address" => {
            let files = node.attachment_files();
            if files.is_empty() {
                if field_type == 1 {
                    return Ok(SyncValue::Text(node.display_text()));
                }
                return Ok(SyncValue::Empty);
            }
            input
                .attachments
                .prepare(&widget.widget_id, &widget.widget_type, &files)
                .await
                .map(SyncValue::Attachments)
        }
        other => Err(SyncError::UnsupportedWidget {
            widget_id,
            widget_type: other.to_string(),
        }),
    }
}

async fn resolve_labels(
    node: &FormNode,
    widget: &WidgetMap,
    options: &dyn OptionResolver,
) -> Result<Vec<String>, SyncError> {
    let pairs = node.option_pairs();
    let mut labels = Vec::new();
    if pairs.is_empty() {
        let texts = node.labels();
        for text in texts {
            labels.push(resolve_text_label(widget, &text, options).await?);
        }
        return Ok(labels);
    }
    for (id, text) in pairs {
        match widget.option_source_key.as_deref() {
            Some(source) => match (id, text) {
                (Some(id), _) => {
                    let label = options.label_for_id(source, &id).await?.ok_or(
                        SyncError::OptionUnresolved {
                            widget_id: widget.widget_id.clone(),
                        },
                    )?;
                    labels.push(label);
                }
                (None, Some(text)) => {
                    if options.id_for_label(source, &text).await?.is_none() {
                        return Err(SyncError::OptionUnresolved {
                            widget_id: widget.widget_id.clone(),
                        });
                    }
                    labels.push(text);
                }
                (None, None) => {
                    return Err(SyncError::OptionUnresolved {
                        widget_id: widget.widget_id.clone(),
                    })
                }
            },
            None => match (id, text) {
                (Some(value), _) => labels.push(invert_fixed(widget, &value)?),
                (None, Some(text)) => labels.push(text),
                (None, None) => {
                    return Err(SyncError::OptionUnresolved {
                        widget_id: widget.widget_id.clone(),
                    })
                }
            },
        }
    }
    Ok(labels)
}

async fn resolve_text_label(
    widget: &WidgetMap,
    text: &str,
    options: &dyn OptionResolver,
) -> Result<String, SyncError> {
    match widget.option_source_key.as_deref() {
        Some(source) => {
            if options.id_for_label(source, text).await?.is_none() {
                return Err(SyncError::OptionUnresolved {
                    widget_id: widget.widget_id.clone(),
                });
            }
            Ok(text.to_string())
        }
        None => {
            if widget.option_map.contains_key(text) {
                Ok(text.to_string())
            } else {
                // 回读只有 value（提交时的 option value）时反查 label；
                // 多对一会在这里变成 OptionAmbiguous。
                invert_fixed(widget, text)
            }
        }
    }
}

/// 固定选项：`label → value` 反查 `value → label`；多对一必须人工核对。
fn invert_fixed(widget: &WidgetMap, value: &str) -> Result<String, SyncError> {
    let labels: Vec<&String> = widget
        .option_map
        .iter()
        .filter(|(_, mapped)| mapped.as_str() == value)
        .map(|(label, _)| label)
        .collect();
    match labels.len() {
        1 => Ok(labels[0].clone()),
        0 => Err(SyncError::OptionUnresolved {
            widget_id: widget.widget_id.clone(),
        }),
        _ => Err(SyncError::OptionAmbiguous {
            widget_id: widget.widget_id.clone(),
        }),
    }
}

fn target_field<'a>(fields: &'a [FieldItem], field_id: &str) -> Option<&'a FieldItem> {
    fields
        .iter()
        .find(|field| field.field_id == field_id || field.field_name == field_id)
}

fn target_supported(widget_type: &str, field_type: i32) -> bool {
    match widget_type {
        "input" | "textarea" => matches!(field_type, 1 | 3),
        "number" | "amount" | "formula" => field_type == 2,
        "date" | "dateInterval" => field_type == 5,
        "radio" | "radioV2" | "select" => matches!(field_type, 1..=3),
        "checkbox" | "checkboxV2" | "multiSelect" => matches!(field_type, 1 | 4),
        "telephone" => matches!(field_type, 1 | 13),
        "contact" => matches!(field_type, 1 | 11),
        "department" => field_type == 1,
        "connect" => matches!(field_type, 1 | 15 | 18 | 21),
        "attachmentV2" | "image" | "imageV2" | "attachment" => field_type == 17,
        "document" | "address" => matches!(field_type, 1 | 17),
        _ => false,
    }
}

fn duplicate_targets(widgets: &[WidgetMap]) -> BTreeSet<String> {
    let mut owners: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for widget in widgets {
        if widget_rules::is_container(&widget.widget_type) {
            continue;
        }
        owners
            .entry(widget.bitable_field.clone())
            .or_default()
            .push(widget.widget_id.clone());
        if let Some(end) = widget.interval_end_field.as_deref() {
            owners
                .entry(end.to_string())
                .or_default()
                .push(widget.widget_id.clone());
        }
    }
    owners
        .into_iter()
        .filter(|(_, owners)| owners.len() > 1)
        .map(|(field, _)| field)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::feishu::domain::approval_convert::Converter;
    use crate::addon::feishu::domain::bitable::{FieldItem, FieldProperty, PropertyOption};

    struct FakeOptions {
        by_id: BTreeMap<(String, String), String>,
        by_label: BTreeMap<(String, String), String>,
    }

    #[async_trait]
    impl OptionResolver for FakeOptions {
        async fn label_for_id(
            &self,
            source_key: &str,
            option_id: &str,
        ) -> Result<Option<String>, SyncError> {
            Ok(self
                .by_id
                .get(&(source_key.to_string(), option_id.to_string()))
                .cloned())
        }

        async fn id_for_label(
            &self,
            source_key: &str,
            label: &str,
        ) -> Result<Option<String>, SyncError> {
            Ok(self
                .by_label
                .get(&(source_key.to_string(), label.to_string()))
                .cloned())
        }
    }

    struct FakeDepartments;

    #[async_trait]
    impl DepartmentResolver for FakeDepartments {
        async fn name_for_id(&self, department_id: &str) -> Result<Option<String>, SyncError> {
            Ok((department_id == "od_1").then(|| "财务部".to_string()))
        }
    }

    struct FakeAttachments;

    #[async_trait]
    impl AttachmentPreparer for FakeAttachments {
        async fn prepare(
            &self,
            _widget_id: &str,
            _widget_type: &str,
            files: &[AttachmentRef],
        ) -> Result<Vec<PreparedAttachment>, SyncError> {
            Ok(files
                .iter()
                .map(|file| PreparedAttachment {
                    file_token: format!("tok-{}", file.name),
                    name: file.name.clone(),
                    size: file.size,
                })
                .collect())
        }
    }

    fn field(field_id: &str, field_type: i32) -> FieldItem {
        FieldItem {
            is_primary: false,
            field_id: field_id.to_string(),
            field_name: field_id.to_string(),
            field_type: Some(field_type),
            ui_type: None,
            property: None,
        }
    }

    fn field_with_options(field_id: &str, field_type: i32, options: &[&str]) -> FieldItem {
        FieldItem {
            property: Some(FieldProperty {
                options: options
                    .iter()
                    .map(|name| PropertyOption {
                        name: (*name).to_string(),
                    })
                    .collect(),
                table_id: None,
            }),
            ..field(field_id, field_type)
        }
    }

    fn number(text: &str) -> SyncValue {
        SyncValue::Number(
            crate::addon::feishu::domain::approval_export::form::FixedNumber::parse(text)
                .unwrap_or_else(|| panic!("{text}")),
        )
    }

    fn widget(
        widget_id: &str,
        widget_type: &str,
        bitable_field: &str,
        option_map: &[(&str, &str)],
        option_source_key: Option<&str>,
    ) -> WidgetMap {
        WidgetMap {
            widget_id: widget_id.to_string(),
            widget_type: widget_type.to_string(),
            required: false,
            bitable_field: bitable_field.to_string(),
            interval_end_field: None,
            converter: Converter::Direct,
            option_map: option_map
                .iter()
                .map(|(label, value)| ((*label).to_string(), (*value).to_string()))
                .collect(),
            option_source_key: option_source_key.map(str::to_string),
            currency: None,
            strip_spaces: false,
        }
    }

    fn options() -> FakeOptions {
        FakeOptions {
            by_id: BTreeMap::from([(
                ("src".to_string(), "opt1".to_string()),
                "已批准".to_string(),
            )]),
            by_label: BTreeMap::from([(
                ("src".to_string(), "已批准".to_string()),
                "opt1".to_string(),
            )]),
        }
    }

    async fn run_plan(form: &str, widgets: &[WidgetMap], fields: &[FieldItem]) -> RecordPlan {
        let form = InstanceForm::parse(Some(form)).unwrap_or_else(|error| panic!("{error}"));
        let options = options();
        let departments = FakeDepartments;
        let attachments = FakeAttachments;
        build_single(PlanInput {
            record_id: "rec1",
            task_id: Some(1),
            form: &form,
            widgets,
            fields,
            timezone_offset_seconds: 8 * 3_600,
            allow_clear: false,
            row_index: Some(0),
            options: &options,
            departments: &departments,
            attachments: &attachments,
        })
        .await
    }

    #[tokio::test]
    async fn maps_all_scalar_types() {
        let form = r#"[
            {"id":"text","type":"input","value":"付款"},
            {"id":"amount","type":"amount","value":12.5},
            {"id":"date","type":"date","value":"2020-01-01T08:00:00+08:00"},
            {"id":"kind","type":"radioV2","value":"opt1",
             "option":{"key":"opt1","text":"已批准"}},
            {"id":"tags","type":"checkboxV2","value":["opt1"],
             "option":[{"key":"opt1","text":"已批准"}]},
            {"id":"phone","type":"telephone",
             "value":{"countryCode":"+86","nationalNumber":"13800138000"}},
            {"id":"person","type":"contact","open_ids":["ou_a"]},
            {"id":"dept","type":"department","value":[{"open_id":"od_1"}]},
            {"id":"files","type":"attachmentV2",
             "value":["https://x/a.pdf"],"ext":["a.pdf"]}
        ]"#;
        let widgets = [
            widget("text", "input", "fld_text", &[], None),
            widget("amount", "amount", "fld_amount", &[], None),
            widget("date", "date", "fld_date", &[], None),
            widget("kind", "radioV2", "fld_kind", &[], Some("src")),
            widget("tags", "checkboxV2", "fld_tags", &[], Some("src")),
            widget("phone", "telephone", "fld_phone", &[], None),
            widget("person", "contact", "fld_person", &[], None),
            widget("dept", "department", "fld_dept", &[], None),
            widget("files", "attachmentV2", "fld_files", &[], None),
        ];
        let fields = [
            field("fld_text", 1),
            field("fld_amount", 2),
            field("fld_date", 5),
            field("fld_kind", 3),
            field("fld_tags", 4),
            field("fld_phone", 13),
            field("fld_person", 11),
            field("fld_dept", 1),
            field("fld_files", 17),
        ];
        let plan = run_plan(form, &widgets, &fields).await;
        assert!(plan.reviews.is_empty(), "{:?}", plan.reviews);
        assert_eq!(plan.patches.len(), 9);
        let by_id: BTreeMap<_, _> = plan
            .patches
            .iter()
            .map(|patch| (patch.field_id.as_str(), patch.value.clone()))
            .collect();
        assert_eq!(by_id["fld_text"], SyncValue::Text("付款".into()));
        assert_eq!(
            by_id["fld_amount"],
            SyncValue::Number(
                crate::addon::feishu::domain::approval_export::form::FixedNumber::parse("12.5")
                    .unwrap_or_else(|| panic!(""))
            )
        );
        assert_eq!(by_id["fld_date"], SyncValue::Millis(1_577_836_800_000));
        assert_eq!(by_id["fld_kind"], SyncValue::Labels(vec!["已批准".into()]));
        assert_eq!(by_id["fld_tags"], SyncValue::Labels(vec!["已批准".into()]));
        assert_eq!(
            by_id["fld_phone"],
            SyncValue::Text("+86 13800138000".into())
        );
        assert_eq!(
            by_id["fld_person"],
            SyncValue::PersonIds(vec!["ou_a".into()])
        );
        assert_eq!(by_id["fld_dept"], SyncValue::Text("财务部".into()));
        assert!(matches!(by_id["fld_files"], SyncValue::Attachments(_)));
    }

    #[tokio::test]
    async fn fixed_options_invert_value_to_label_and_ambiguity_reviews() {
        let form = r#"[
            {"id":"kind","type":"radioV2","value":"a"}
        ]"#;
        let widgets = [widget(
            "kind",
            "radioV2",
            "fld_kind",
            &[("甲", "a"), ("乙", "a")],
            None,
        )];
        let fields = [field("fld_kind", 3)];
        let plan = run_plan(form, &widgets, &fields).await;
        assert!(plan.patches.is_empty());
        assert!(matches!(
            plan.reviews.first().map(|review| &review.error),
            Some(SyncError::OptionAmbiguous { .. })
        ));
    }

    #[tokio::test]
    async fn missing_widget_is_skipped_or_reviewed_by_required_flag() {
        let form = r#"[{"id":"other","type":"input","value":"x"}]"#;
        let optional = [widget("missing", "input", "fld_missing", &[], None)];
        let fields = [field("fld_missing", 1)];
        let plan = run_plan(form, &optional, &fields).await;
        assert!(plan.patches.is_empty());
        assert_eq!(plan.skipped[0].reason, "missing_widget");

        let mut required = optional[0].clone();
        required.required = true;
        let plan = run_plan(form, &[required], &fields).await;
        assert!(matches!(
            plan.reviews.first().map(|review| &review.error),
            Some(SyncError::MissingWidget { .. })
        ));
    }

    #[tokio::test]
    async fn empty_value_requires_clear_permission() {
        let form = r#"[{"id":"text","type":"input","value":""}]"#;
        let widgets = [widget("text", "input", "fld_text", &[], None)];
        let fields = [field("fld_text", 1)];
        let plan = run_plan(form, &widgets, &fields).await;
        assert!(plan.patches.is_empty());
        assert_eq!(plan.skipped[0].reason, "clear_disabled");

        let form = InstanceForm::parse(Some(form)).unwrap_or_else(|error| panic!("{error}"));
        let options = options();
        let departments = FakeDepartments;
        let attachments = FakeAttachments;
        let plan = build_single(PlanInput {
            record_id: "rec1",
            task_id: None,
            form: &form,
            widgets: &widgets,
            fields: &fields,
            timezone_offset_seconds: 0,
            allow_clear: true,
            row_index: None,
            options: &options,
            departments: &departments,
            attachments: &attachments,
        })
        .await;
        assert_eq!(plan.patches[0].value, SyncValue::Empty);
    }

    #[tokio::test]
    async fn duplicate_targets_fail_closed() {
        let form = r#"[
            {"id":"a","type":"input","value":"A"},
            {"id":"b","type":"input","value":"B"}
        ]"#;
        let widgets = [
            widget("a", "input", "fld_same", &[], None),
            widget("b", "input", "fld_same", &[], None),
        ];
        let fields = [field("fld_same", 1)];
        let plan = run_plan(form, &widgets, &fields).await;
        assert!(plan.patches.is_empty());
        assert_eq!(plan.reviews.len(), 2);
        assert!(plan
            .reviews
            .iter()
            .all(|review| matches!(review.error, SyncError::DuplicateTarget { .. })));
    }

    #[tokio::test]
    async fn unsupported_target_and_detail_row_are_handled() {
        let form = r#"[
            {"id":"detail","type":"fieldList","value":[[
                {"id":"amount","type":"amount","value":7}
            ]]}
        ]"#;
        let widgets = [widget("amount", "amount", "fld_amount", &[], None)];
        // 目标列是文本 → 数字控件不支持。
        let plan = run_plan(form, &widgets, &[field("fld_amount", 1)]).await;
        assert!(matches!(
            plan.reviews.first().map(|review| &review.error),
            Some(SyncError::UnsupportedTarget { .. })
        ));

        let plan = run_plan(form, &widgets, &[field("fld_amount", 2)]).await;
        assert!(plan.reviews.is_empty());
        assert_eq!(
            plan.patches[0].value,
            SyncValue::Number(
                crate::addon::feishu::domain::approval_export::form::FixedNumber::parse("7")
                    .unwrap_or_else(|| panic!(""))
            )
        );
    }

    #[tokio::test]
    async fn date_interval_writes_start_and_end_columns() {
        let form = r#"[
            {"id":"range","type":"dateInterval",
             "value":{"start":"2020-01-01T00:00:00+08:00",
                      "end":"2020-01-03T00:00:00+08:00"}}
        ]"#;
        let mut range = widget("range", "dateInterval", "fld_start", &[], None);
        range.interval_end_field = Some("fld_end".into());
        let fields = [field("fld_start", 5), field("fld_end", 5)];
        let plan = run_plan(form, &[range], &fields).await;
        assert!(plan.reviews.is_empty(), "{:?}", plan.reviews);
        assert_eq!(plan.patches.len(), 2);
        assert_eq!(plan.patches[0].value, SyncValue::Millis(1_577_808_000_000));
        assert_eq!(plan.patches[1].value, SyncValue::Millis(1_577_980_800_000));
    }

    #[tokio::test]
    async fn input_to_single_select_writes_when_option_exists() {
        let form = r#"[{"id":"bank","type":"input","value":"中国建设银行"}]"#;
        let widgets = [widget("bank", "input", "fld_bank", &[], None)];
        let fields = [field_with_options(
            "fld_bank",
            3,
            &["中国建设银行", "中信银行"],
        )];
        let plan = run_plan(form, &widgets, &fields).await;
        assert!(plan.reviews.is_empty(), "{:?}", plan.reviews);
        assert_eq!(plan.patches.len(), 1);
        assert_eq!(
            plan.patches[0].value,
            SyncValue::Text("中国建设银行".into())
        );
    }

    #[tokio::test]
    async fn input_to_single_select_trims_matched_option() {
        let form = r#"[{"id":"bank","type":"input","value":"中国建设银行 "}]"#;
        let widgets = [widget("bank", "input", "fld_bank", &[], None)];
        let fields = [field_with_options("fld_bank", 3, &["中国建设银行"])];
        let plan = run_plan(form, &widgets, &fields).await;
        assert!(plan.reviews.is_empty(), "{:?}", plan.reviews);
        assert_eq!(
            plan.patches[0].value,
            SyncValue::Text("中国建设银行".into())
        );
    }

    #[tokio::test]
    async fn input_to_single_select_reviews_when_option_missing() {
        let form = r#"[{"id":"bank","type":"input","value":"不存在银行"}]"#;
        let widgets = [widget("bank", "input", "fld_bank", &[], None)];
        let fields = [field_with_options("fld_bank", 3, &["中国建设银行"])];
        let plan = run_plan(form, &widgets, &fields).await;
        assert!(plan.patches.is_empty());
        assert_eq!(
            plan.reviews
                .first()
                .map(|review| review.error.code())
                .unwrap_or_default(),
            "option_not_found"
        );
    }

    #[tokio::test]
    async fn radio_to_number_parses_numeric_label() {
        let form = r#"[
            {"id":"rate","type":"radioV2","value":"opt1",
             "option":{"key":"opt1","text":"1.5"}}
        ]"#;
        let widgets = [widget(
            "rate",
            "radioV2",
            "fld_rate",
            &[("1.5", "opt1")],
            None,
        )];
        let fields = [field("fld_rate", 2)];
        let plan = run_plan(form, &widgets, &fields).await;
        assert!(plan.reviews.is_empty(), "{:?}", plan.reviews);
        assert_eq!(plan.patches.len(), 1);
        assert_eq!(plan.patches[0].value, number("1.5"));
    }

    #[tokio::test]
    async fn radio_to_number_reviews_non_numeric_label() {
        let form = r#"[
            {"id":"rate","type":"radioV2","value":"opt1",
             "option":{"key":"opt1","text":"不适用"}}
        ]"#;
        let widgets = [widget(
            "rate",
            "radioV2",
            "fld_rate",
            &[("不适用", "opt1")],
            None,
        )];
        let fields = [field("fld_rate", 2)];
        let plan = run_plan(form, &widgets, &fields).await;
        assert!(plan.patches.is_empty());
        assert_eq!(
            plan.reviews
                .first()
                .map(|review| review.error.code())
                .unwrap_or_default(),
            "invalid_value"
        );
    }

    #[tokio::test]
    async fn formula_target_is_not_a_review() {
        let form = r#"[{"id":"total","type":"formula","value":100}]"#;
        let widgets = [widget("total", "formula", "fld_total", &[], None)];
        let fields = [field("fld_total", 19)];
        let plan = run_plan(form, &widgets, &fields).await;
        assert!(plan.reviews.is_empty(), "{:?}", plan.reviews);
        assert!(plan.patches.is_empty());
        assert_eq!(plan.verifications.len(), 1);
        assert_eq!(plan.verifications[0].field_id, "fld_total");
        assert_eq!(plan.verifications[0].expected, number("100"));
    }

    #[tokio::test]
    async fn formula_text_target_keeps_text_for_verification() {
        let form = r#"[{"id":"code","type":"formula","value":"办公费00005"}]"#;
        let widgets = [widget("code", "formula", "fld_code", &[], None)];
        let fields = [field("fld_code", 20)];
        let plan = run_plan(form, &widgets, &fields).await;
        assert!(plan.reviews.is_empty(), "{:?}", plan.reviews);
        assert_eq!(
            plan.verifications[0].expected,
            SyncValue::Text("办公费00005".into())
        );
    }
}

//! `SyncValue` → 多维表 `batch_update` 单元格值。
//!
//! 写入键最终按**当前字段名**；本模块只负责值形态。清空语义按类型分开：文本空串、
//! 数字/日期/单选 `null`、多选/人员 `[]`、复选框 `false`，并且必须显式
//! `allow_clear` 才输出。

use serde_json::{json, Value};

use super::super::approval;
use super::error::SyncError;
use super::value::SyncValue;

/// 把语义值编码成目标列的值；`None` = 不写（空值且未开启清空）。
pub(crate) fn encode_cell(
    field_type: i32,
    value: &SyncValue,
    allow_clear: bool,
) -> Result<Option<Value>, SyncError> {
    if value.is_empty() {
        if !allow_clear {
            return Ok(None);
        }
        return clear_cell(field_type).map(Some);
    }
    let cell = match field_type {
        1 => Value::String(text_for_text_column(value)?),
        2 => number_cell(value)?,
        3 => Value::String(single_label(value)?),
        4 => Value::Array(
            match value {
                SyncValue::Labels(labels) => labels,
                _ => {
                    return Err(invalid("多选列的值必须是标签数组"));
                }
            }
            .iter()
            .map(|label| Value::String(label.clone()))
            .collect(),
        ),
        5 => Value::from(date_millis(value)?),
        7 => match value {
            SyncValue::Bool(flag) => Value::Bool(*flag),
            _ => return Err(invalid("复选框列的值必须是布尔")),
        },
        11 => Value::Array(
            match value {
                SyncValue::PersonIds(ids) => ids,
                _ => return Err(invalid("人员列的值必须是 open_id 数组")),
            }
            .iter()
            .map(|id| json!({ "id": id }))
            .collect(),
        ),
        13 => Value::String(text_for_text_column(value)?),
        15 => hyperlink_cell(value)?,
        17 => Value::Array(
            match value {
                SyncValue::Attachments(files) => files,
                _ => return Err(invalid("附件列的值必须是已上传文件数组")),
            }
            .iter()
            .map(|file| {
                if file.file_token.trim().is_empty() {
                    return Err(SyncError::AttachmentUnavailable {
                        widget_id: String::new(),
                    });
                }
                Ok(json!({ "file_token": file.file_token }))
            })
            .collect::<Result<Vec<_>, _>>()?,
        ),
        other => {
            return Err(SyncError::UnsupportedTarget {
                field_id: String::new(),
                field_type: Some(other),
            })
        }
    };
    Ok(Some(cell))
}

fn invalid(reason: &'static str) -> SyncError {
    SyncError::InvalidValue {
        widget_id: String::new(),
        reason,
    }
}

fn clear_cell(field_type: i32) -> Result<Value, SyncError> {
    match field_type {
        1 | 13 => Ok(Value::String(String::new())),
        2 | 3 | 5 | 15 => Ok(Value::Null),
        4 | 11 | 17 => Ok(Value::Array(Vec::new())),
        7 => Ok(Value::Bool(false)),
        other => Err(SyncError::UnsupportedTarget {
            field_id: String::new(),
            field_type: Some(other),
        }),
    }
}

pub(crate) fn text_for_text_column(value: &SyncValue) -> Result<String, SyncError> {
    match value {
        SyncValue::Text(text) => Ok(text.clone()),
        SyncValue::Number(number) => Ok(number.to_json_value().to_string()),
        SyncValue::Bool(flag) => Ok(flag.to_string()),
        SyncValue::Labels(labels) => Ok(labels.join("、")),
        SyncValue::PersonIds(ids) => Ok(ids.join(",")),
        SyncValue::ConnectCodes(codes) => Ok(codes.join(",")),
        SyncValue::Millis(millis) => Ok(millis.to_string()),
        SyncValue::DateInterval { start, end } => Ok(format!("{start} ~ {end}")),
        SyncValue::Attachments(files) => Ok(files
            .iter()
            .map(|file| file.name.clone())
            .collect::<Vec<_>>()
            .join("、")),
        SyncValue::Empty => Ok(String::new()),
    }
}

fn number_cell(value: &SyncValue) -> Result<Value, SyncError> {
    match value {
        SyncValue::Number(number) => Ok(number.to_json_value()),
        SyncValue::Text(text) => super::super::approval_export::form::FixedNumber::parse(text)
            .map(|number| number.to_json_value())
            .ok_or_else(|| invalid("数字列的值不是十进制数字")),
        _ => Err(invalid("数字列的值必须是数字")),
    }
}

fn single_label(value: &SyncValue) -> Result<String, SyncError> {
    match value {
        SyncValue::Labels(labels) => {
            if labels.len() != 1 {
                return Err(invalid("单选列只能有一个选项"));
            }
            Ok(labels[0].clone())
        }
        SyncValue::Text(text) => Ok(text.clone()),
        _ => Err(invalid("单选列的值必须是单个文案")),
    }
}

fn date_millis(value: &SyncValue) -> Result<i64, SyncError> {
    match value {
        SyncValue::Millis(millis) => Ok(*millis),
        // 单列目标写区间开始时间；结束时间由 plan 单独写到 interval_end_field。
        SyncValue::DateInterval { start, .. } => Ok(*start),
        _ => Err(invalid("日期列的值必须是毫秒时间戳")),
    }
}

fn hyperlink_cell(value: &SyncValue) -> Result<Value, SyncError> {
    match value {
        SyncValue::ConnectCodes(codes) => {
            if codes.len() != 1 {
                return Err(invalid("超链接列只能写一个关联审批实例"));
            }
            let link =
                approval::instance_applink(&codes[0]).map_err(|_| SyncError::InvalidValue {
                    widget_id: String::new(),
                    reason: "关联审批实例 Code 不是合法 AppLink",
                })?;
            Ok(json!({ "text": codes[0], "link": link }))
        }
        SyncValue::Text(text) => Ok(json!({ "text": text, "link": text })),
        _ => Err(invalid("超链接列的值必须是单个链接")),
    }
}

#[cfg(test)]
mod tests {
    use super::super::value::PreparedAttachment;
    use super::*;

    fn number(text: &str) -> SyncValue {
        SyncValue::Number(
            super::super::super::approval_export::form::FixedNumber::parse(text)
                .unwrap_or_else(|| panic!("{text}")),
        )
    }

    #[test]
    fn encodes_each_writable_type() {
        assert_eq!(
            encode_cell(1, &SyncValue::Text("付款".into()), false)
                .unwrap_or_else(|e| panic!("{e}")),
            Some(json!("付款"))
        );
        assert_eq!(
            encode_cell(2, &number("12.50"), false).unwrap_or_else(|e| panic!("{e}")),
            Some(json!(12.5))
        );
        assert_eq!(
            encode_cell(3, &SyncValue::Labels(vec!["已批准".into()]), false)
                .unwrap_or_else(|e| panic!("{e}")),
            Some(json!("已批准"))
        );
        assert_eq!(
            encode_cell(4, &SyncValue::Labels(vec!["A".into(), "B".into()]), false)
                .unwrap_or_else(|e| panic!("{e}")),
            Some(json!(["A", "B"]))
        );
        assert_eq!(
            encode_cell(5, &SyncValue::Millis(1_700_000_000_000), false)
                .unwrap_or_else(|e| panic!("{e}")),
            Some(json!(1_700_000_000_000_i64))
        );
        assert_eq!(
            encode_cell(7, &SyncValue::Bool(false), false).unwrap_or_else(|e| panic!("{e}")),
            Some(json!(false))
        );
        assert_eq!(
            encode_cell(11, &SyncValue::PersonIds(vec!["ou_a".into()]), false)
                .unwrap_or_else(|e| panic!("{e}")),
            Some(json!([{"id":"ou_a"}]))
        );
        assert_eq!(
            encode_cell(13, &SyncValue::Text("+86 138".into()), false)
                .unwrap_or_else(|e| panic!("{e}")),
            Some(json!("+86 138"))
        );
        let code = "19EAC829-F1CB-527F-BE2A-1330422E60C0";
        let link = encode_cell(15, &SyncValue::ConnectCodes(vec![code.into()]), false)
            .unwrap_or_else(|e| panic!("{e}"))
            .unwrap_or(Value::Null);
        assert_eq!(link["text"], code);
        assert!(link["link"].as_str().unwrap_or_default().contains(code));
        assert_eq!(
            encode_cell(
                17,
                &SyncValue::Attachments(vec![PreparedAttachment {
                    file_token: "tok".into(),
                    name: "a.pdf".into(),
                    size: Some(1),
                }]),
                false
            )
            .unwrap_or_else(|e| panic!("{e}")),
            Some(json!([{"file_token":"tok"}]))
        );
    }

    #[test]
    fn clear_requires_explicit_permission_and_uses_type_specific_shape() {
        for (field_type, expected) in [
            (1, json!("")),
            (2, Value::Null),
            (3, Value::Null),
            (4, json!([])),
            (5, Value::Null),
            (7, json!(false)),
            (11, json!([])),
            (13, json!("")),
            (15, Value::Null),
            (17, json!([])),
        ] {
            assert_eq!(
                encode_cell(field_type, &SyncValue::Empty, true).unwrap_or_else(|e| panic!("{e}")),
                Some(expected),
                "type {field_type}"
            );
            assert_eq!(
                encode_cell(field_type, &SyncValue::Empty, false).unwrap_or_else(|e| panic!("{e}")),
                None,
                "type {field_type} 未开清空时必须跳过"
            );
        }
    }

    #[test]
    fn rejects_unwritable_targets_and_wrong_shapes() {
        assert!(matches!(
            encode_cell(19, &SyncValue::Text("x".into()), false),
            Err(SyncError::UnsupportedTarget { .. })
        ));
        assert!(matches!(
            encode_cell(20, &number("1"), false),
            Err(SyncError::UnsupportedTarget { .. })
        ));
        assert!(matches!(
            encode_cell(2, &SyncValue::Text("一百".into()), false),
            Err(SyncError::InvalidValue { .. })
        ));
        assert!(matches!(
            encode_cell(4, &SyncValue::Text("A".into()), false),
            Err(SyncError::InvalidValue { .. })
        ));
        assert!(matches!(
            encode_cell(
                11,
                &SyncValue::PersonIds(vec!["ou_a".into(), "ou_b".into()]),
                false
            )
            .map(|cell| cell.is_some()),
            Ok(true)
        ));
        assert!(matches!(
            encode_cell(
                15,
                &SyncValue::ConnectCodes(vec!["a".into(), "b".into()]),
                false
            ),
            Err(SyncError::InvalidValue { .. })
        ));
    }
}

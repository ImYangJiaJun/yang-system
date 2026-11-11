//! 评论条件判定、正文渲染与去重标记匹配。
//!
//! 纯函数，便于单测；所有飞书调用都在 worker / Action 层。

use std::collections::BTreeMap;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::super::approval_convert::{is_empty, text_from_cell};
use super::super::bitable::{cell_label, CellValue};
use super::config::CommentConfig;

/// 评论条件判定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommentCondition {
    /// 必填齐全且触发值命中，可以发评论。
    Ready,
    /// 数据还缺（下一轮扫描会重新评估）。
    Waiting(String),
    /// 触发值不匹配（下一轮扫描会重新评估）。
    Skipped(String),
}

/// 评论附件（飞书 `files[]` 元素）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommentFile {
    pub(crate) url: String,
    pub(crate) title: String,
    pub(crate) file_size: i64,
    pub(crate) file_type: String,
}

/// 渲染结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenderedComment {
    /// `content` 里的 `text` 正文（含标记行、字段行与记录链接）。
    pub(crate) text: String,
    /// 飞书评论 `files[]`。
    pub(crate) files: Vec<CommentFile>,
}

/// 从一条评论里用于去重的最少信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommentItem {
    pub(crate) id: String,
    pub(crate) content: String,
}

/// 判定一条目标记录是否满足评论条件。
pub(crate) fn evaluate_condition(
    config: &CommentConfig,
    fields: &BTreeMap<String, Value>,
) -> CommentCondition {
    for field_id in &config.required_field_ids {
        let field_id = field_id.trim();
        if field_id.is_empty() {
            continue;
        }
        let value = fields.get(field_id);
        if value.is_none_or(is_empty) {
            return CommentCondition::Waiting(format!(
                "必填字段「{}」为空",
                config.field_name(field_id)
            ));
        }
    }
    let Some(trigger_field_id) = config
        .trigger_field_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return CommentCondition::Ready;
    };
    let value = fields.get(trigger_field_id);
    if value.is_none_or(is_empty) {
        return CommentCondition::Waiting(format!(
            "触发字段「{}」为空",
            config.field_name(trigger_field_id)
        ));
    }
    let value = value.unwrap_or(&Value::Null);
    let labels = cell_labels(value);
    let matched = labels.iter().any(|label| {
        config
            .trigger_values
            .iter()
            .any(|expected| expected.trim() == label.trim())
    });
    if matched {
        CommentCondition::Ready
    } else {
        CommentCondition::Skipped(format!(
            "触发字段「{}」的值为「{}」，不在触发值 {:?} 中",
            config.field_name(trigger_field_id),
            labels.join("、"),
            config.trigger_values
        ))
    }
}

/// 渲染评论正文与附件。
///
/// 标记行固定为 `prefix + record_id`（去重依据）；随后是每个配置字段一行
/// 「列名：值」，最后是记录链接。
pub(crate) fn render_comment(
    config: &CommentConfig,
    record_id: &str,
    base_token: &str,
    table_id: &str,
    fields: &BTreeMap<String, Value>,
) -> RenderedComment {
    let mut lines = vec![config.marker(record_id)];
    let mut files = Vec::new();
    for field_id in config.render_field_ids() {
        let value = fields.get(field_id);
        let (text, mut attachments) = render_cell(value);
        if config.include_attachments {
            files.append(&mut attachments);
        }
        lines.push(format!("{}：{}", config.field_name(field_id), text));
    }
    let url = render_record_url(&config.record_url_template, base_token, table_id, record_id);
    if !url.is_empty() {
        lines.push(url);
    }
    RenderedComment {
        text: lines.join("\n"),
        files,
    }
}

/// 支付内容的稳定指纹（sha256 十六进制）：只覆盖参与评论的字段。
pub(crate) fn payment_fingerprint(
    config: &CommentConfig,
    fields: &BTreeMap<String, Value>,
) -> String {
    let mut hasher = Sha256::new();
    for field_id in config.render_field_ids() {
        hasher.update(field_id.as_bytes());
        hasher.update(b"=");
        let value = fields.get(field_id).unwrap_or(&Value::Null);
        hasher.update(serde_json::to_string(value).unwrap_or_default().as_bytes());
        hasher.update(b"\n");
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 评论正文里是否已有本记录的标记。
pub(crate) fn find_marker<'a>(
    comments: &'a [CommentItem],
    marker: &str,
) -> Option<&'a CommentItem> {
    comments
        .iter()
        .find(|comment| comment.content.contains(marker))
}

fn render_cell(value: Option<&Value>) -> (String, Vec<CommentFile>) {
    let Some(value) = value else {
        return ("（空）".to_string(), Vec::new());
    };
    if is_empty(value) {
        return ("（空）".to_string(), Vec::new());
    }
    let attachments = attachment_files(value);
    if !attachments.is_empty() {
        let names = attachments
            .iter()
            .map(|file| file.title.clone())
            .collect::<Vec<_>>()
            .join("、");
        return (names, attachments);
    }
    // 文本列含 URL 时接口返回富文本分段（`[{"link":…,"text":"Amazon.com"},
    // {"text":" Services LLC"}]`）：逐段拼接、段间不加分隔符。按 `cell_label`
    // 走会用「、」拆开并丢掉链接之后的正文。
    if is_rich_text_array(value) {
        return (text_from_cell(value), Vec::new());
    }
    match cell_label(value) {
        CellValue::Text(labels) => (labels.join("、"), Vec::new()),
        CellValue::Empty => ("（空）".to_string(), Vec::new()),
        CellValue::Unsupported => (compact_json(value), Vec::new()),
    }
}

/// 富文本分段数组：每个元素都是含 `text`/`link` 的对象且没有 `file_token`。
fn is_rich_text_array(value: &Value) -> bool {
    let Value::Array(items) = value else {
        return false;
    };
    !items.is_empty()
        && items.iter().all(|item| match item {
            Value::Object(object) => {
                (object.contains_key("text") || object.contains_key("link"))
                    && string_key(object, &["file_token"]).is_empty()
            }
            _ => false,
        })
}

/// 飞书附件单元格：`[{name, url, size, type, file_token, tmp_url}]`。
///
/// 只认真附件对象：含非空 `file_token`，或同时含 `name`/`file_name` 与非空
/// `size`/`file_size`。富文本 URL 段（`{"link":…,"text":…,"type":"url"}`）不是
/// 附件——旧实现把任意带 `link` 的对象当附件，会生成非法的 `type:"url"` files。
fn attachment_files(value: &Value) -> Vec<CommentFile> {
    let Value::Array(items) = value else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for item in items {
        let Value::Object(object) = item else {
            continue;
        };
        if !is_attachment_object(object) {
            continue;
        }
        // `link` 只用于富文本跳转，不是可下载地址；附件 URL 只取 `url`/`tmp_url`。
        let url = string_key(object, &["url", "tmp_url"]);
        let title = string_key(object, &["name", "file_name", "title"]);
        let file_size = integer_key(object, &["size", "file_size"]).unwrap_or(0);
        // 评论 files[] 只接受 image / attachment，不能透传多维表格的 file/url。
        let file_type = if string_key(object, &["type", "file_type"])
            .to_ascii_lowercase()
            .contains("image")
        {
            "image"
        } else {
            "attachment"
        };
        files.push(CommentFile {
            url,
            title: if title.is_empty() {
                "附件".to_string()
            } else {
                title
            },
            file_size: file_size.max(0),
            file_type: file_type.to_string(),
        });
    }
    files
}

fn is_attachment_object(object: &serde_json::Map<String, Value>) -> bool {
    if !string_key(object, &["file_token"]).is_empty() {
        return true;
    }
    let has_name = !string_key(object, &["name", "file_name"]).is_empty();
    let has_size = integer_key(object, &["size", "file_size"]).is_some_and(|size| size > 0);
    has_name && has_size
}

fn string_key(object: &serde_json::Map<String, Value>, keys: &[&str]) -> String {
    for key in keys {
        if let Some(value) = object.get(*key).and_then(Value::as_str) {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    String::new()
}

fn integer_key(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<i64> {
    for key in keys {
        match object.get(*key) {
            Some(Value::Number(number)) => {
                if let Some(value) = number.as_i64() {
                    return Some(value);
                }
                if let Some(value) = number.as_f64() {
                    return Some(value as i64);
                }
            }
            Some(Value::String(text)) => {
                if let Ok(value) = text.trim().parse::<i64>() {
                    return Some(value);
                }
            }
            _ => {}
        }
    }
    None
}

fn cell_labels(value: &Value) -> Vec<String> {
    match cell_label(value) {
        CellValue::Text(labels) => labels,
        // 单选也可能以 `[{text}]` 形态出现；cell_label 已覆盖。
        CellValue::Empty | CellValue::Unsupported => Vec::new(),
    }
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn render_record_url(template: &str, base_token: &str, table_id: &str, record_id: &str) -> String {
    template
        .replace("{base_token}", base_token)
        .replace("{table_id}", table_id)
        .replace("{record_id}", record_id)
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> CommentConfig {
        let raw = serde_json::json!({
            "enabled": true,
            "field_ids": ["fldBB53wVj","fldHDHvXIH","fldaeNR1GA"],
            "field_names": {
                "fldBB53wVj":"支付ID",
                "fldHDHvXIH":"支付状态",
                "fldaeNR1GA":"支付凭证"
            },
            "required_field_ids": ["fldBB53wVj","fldaeNR1GA"],
            "trigger_field_id": "fldHDHvXIH",
            "trigger_values": ["支付成功"],
            "status_field_id": "fldYArfJiV",
            "status_field_name": "审批评论状态",
            "status_labels": {"commented":"已评论","failed":"评论失败"},
            "author_user_id": "ou_ops",
            "include_attachments": true,
            "prefix": "【支付回填】",
            "record_url_template": "https://tenant/base/{base_token}?table={table_id}&record={record_id}"
        });
        serde_json::from_value(raw).unwrap_or_else(|error| panic!("{error}"))
    }

    fn fields() -> BTreeMap<String, Value> {
        let mut fields = BTreeMap::new();
        fields.insert("fldBB53wVj".to_string(), serde_json::json!("PAY-2026-001"));
        fields.insert("fldHDHvXIH".to_string(), serde_json::json!("支付成功"));
        fields.insert(
            "fldaeNR1GA".to_string(),
            serde_json::json!([
                {"name":"回单.png","url":"https://files/1.png","size":2048,"type":"image"},
                {"name":"发票.pdf","url":"https://files/2.pdf","size":4096,"type":"file"}
            ]),
        );
        fields
    }

    #[test]
    fn condition_requires_all_required_fields_and_matching_trigger() {
        let config = config();
        assert_eq!(
            evaluate_condition(&config, &fields()),
            CommentCondition::Ready
        );

        let mut missing = fields();
        missing.insert("fldaeNR1GA".into(), serde_json::json!([]));
        assert!(matches!(
            evaluate_condition(&config, &missing),
            CommentCondition::Waiting(_)
        ));

        let mut wrong = fields();
        wrong.insert("fldHDHvXIH".into(), serde_json::json!("支付失败"));
        assert!(matches!(
            evaluate_condition(&config, &wrong),
            CommentCondition::Skipped(_)
        ));
    }

    #[test]
    fn render_includes_marker_field_lines_attachment_names_and_link() {
        let rendered = render_comment(&config(), "rec123", "bascn", "tblX", &fields());
        assert!(
            rendered.text.starts_with("【支付回填】rec123\n"),
            "{}",
            rendered.text
        );
        assert!(
            rendered.text.contains("支付ID：PAY-2026-001"),
            "{}",
            rendered.text
        );
        assert!(
            rendered.text.contains("支付状态：支付成功"),
            "{}",
            rendered.text
        );
        assert!(
            rendered.text.contains("支付凭证：回单.png、发票.pdf"),
            "{}",
            rendered.text
        );
        assert!(
            rendered.text.ends_with("record=rec123"),
            "{}",
            rendered.text
        );
        assert_eq!(rendered.files.len(), 2);
        assert_eq!(rendered.files[0].url, "https://files/1.png");
        assert_eq!(rendered.files[0].file_size, 2048);
        assert_eq!(rendered.files[1].title, "发票.pdf");
    }

    #[test]
    fn attachments_can_be_listed_without_being_uploaded() {
        let mut config = config();
        config.include_attachments = false;
        let rendered = render_comment(&config, "rec123", "bascn", "tblX", &fields());
        assert!(rendered.text.contains("回单.png"), "{}", rendered.text);
        assert!(rendered.files.is_empty(), "关闭附件后不应带 files");
    }

    #[test]
    fn marker_dedupe_matches_only_the_exact_record_marker() {
        let comments = vec![
            CommentItem {
                id: "c1".into(),
                content: "【支付回填】recOTHER\n...".into(),
            },
            CommentItem {
                id: "c2".into(),
                content: "【支付回填】rec123\n支付ID：x".into(),
            },
        ];
        assert_eq!(
            find_marker(&comments, "【支付回填】rec123").map(|item| item.id.as_str()),
            Some("c2")
        );
        assert!(find_marker(&comments, "【支付回填】rec124").is_none());
    }

    #[test]
    fn fingerprint_is_stable_and_value_sensitive() {
        let config = config();
        let first = payment_fingerprint(&config, &fields());
        assert_eq!(first, payment_fingerprint(&config, &fields()));
        assert_eq!(first.len(), 64);
        let mut changed = fields();
        changed.insert("fldBB53wVj".into(), serde_json::json!("PAY-2026-002"));
        assert_ne!(first, payment_fingerprint(&config, &changed));
    }

    #[test]
    fn rich_text_url_segments_render_as_one_text_and_never_become_files() {
        // 真机形态（申请编号 202611020064）：文本列含 URL 时被拆成富文本分段。
        let mut fields = fields();
        // 只观察富文本列：清空附件列，files 为空才是真的没被 URL 段污染。
        fields.insert("fldaeNR1GA".into(), serde_json::json!([]));
        fields.insert(
            "fldBB53wVj".into(),
            serde_json::json!([
                {"link":"http://Amazon.com","text":"Amazon.com","type":"url"},
                {"text":" Services LLC","type":"text"}
            ]),
        );
        let rendered = render_comment(&config(), "rec1", "b", "t", &fields);
        assert!(
            rendered.text.contains("支付ID：Amazon.com Services LLC"),
            "链接后的正文不能丢：{}",
            rendered.text
        );
        assert!(
            rendered.files.is_empty(),
            "富文本 URL 段不是附件：{:?}",
            rendered.files
        );
    }

    #[test]
    fn polymorphic_empty_text_cell_counts_as_empty() {
        let mut fields = fields();
        fields.insert("fldaeNR1GA".into(), serde_json::json!([{"text": ""}]));
        assert!(matches!(
            evaluate_condition(&config(), &fields),
            CommentCondition::Waiting(_)
        ));
        let rendered = render_comment(&config(), "rec1", "b", "t", &fields);
        assert!(
            rendered.text.contains("支付凭证：（空）"),
            "{}",
            rendered.text
        );
        assert!(rendered.files.is_empty());
    }

    #[test]
    fn attachment_files_map_to_comment_file_types() {
        let mut fields = fields();
        fields.insert(
            "fldaeNR1GA".into(),
            serde_json::json!([
                {"name":"回单.png","url":"https://files/1.png","size":2048,"type":"image"},
                {"name":"发票.pdf","url":"https://files/2.pdf","size":4096,"type":"file"},
                {"file_token":"tok-1","name":"扫描件.pdf","tmp_url":"https://files/3.pdf","size":10}
            ]),
        );
        let rendered = render_comment(&config(), "rec1", "b", "t", &fields);
        let types: Vec<&str> = rendered
            .files
            .iter()
            .map(|file| file.file_type.as_str())
            .collect();
        assert_eq!(
            types,
            vec!["image", "attachment", "attachment"],
            "评论 files[] 只接受 image/attachment"
        );
        assert_eq!(rendered.files[2].url, "https://files/3.pdf");
    }

    #[test]
    fn empty_cells_render_as_explicit_placeholder() {
        let mut config = config();
        config.required_field_ids.clear();
        let mut fields = fields();
        fields.remove("fldaeNR1GA");
        let rendered = render_comment(&config, "rec1", "b", "t", &fields);
        assert!(
            rendered.text.contains("支付凭证：（空）"),
            "{}",
            rendered.text
        );
    }
}

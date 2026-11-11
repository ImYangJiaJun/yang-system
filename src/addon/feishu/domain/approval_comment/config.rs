//! 审批评论配置：结构、保存期校验与库内解析。
//!
//! 配置挂在回写规则上（`feishu_approval_export_rule.comment_config_json`），
//! 与回写共用目标 Base/表坐标。字段名一律用 snake_case，`deny_unknown_fields`
//! 保证拼错的键在保存期就失败，而不是运行期静默取默认值。
//!
//! # 保存期与运行期两套校验
//!
//! - [`validate_comment_config`]：保存规则时调用，需要目标表字段清单，负责把
//!   `field_names` / `status_field_name` 解析后写回 JSON（列名可改，id 不变）；
//! - [`parse_comment_config`]：worker 从库里读时调用，只做结构校验，不发请求。
//!
//! # 日志纪律
//!
//! 这里的错误文案只包含 field_id / 列名 / 配置键，不含单元格值。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::super::approval_convert::labels_equivalent;
use super::super::bitable::FieldItem;

/// 审批评论状态列的两种文案。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommentStatusLabels {
    /// 评论已发出。
    #[serde(default = "default_commented_label")]
    pub(crate) commented: String,
    /// 评论最终失败（补偿写状态列用）。
    #[serde(default = "default_failed_label")]
    pub(crate) failed: String,
}

fn default_commented_label() -> String {
    "已评论".to_string()
}

fn default_failed_label() -> String {
    "评论失败".to_string()
}

/// 审批评论配置的稳定形状。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommentConfig {
    /// 是否启用。关闭时 worker 不处理该规则。
    #[serde(default)]
    pub(crate) enabled: bool,
    /// 参与评论渲染的列 field_id（顺序即评论中的行顺序）。
    #[serde(default)]
    pub(crate) field_ids: Vec<String>,
    /// 保存时解析并缓存的 field_id → 当前列名。运行期优先按 id 取值，名字只用于渲染。
    #[serde(default)]
    pub(crate) field_names: BTreeMap<String, String>,
    /// 必须非空才允许评论的列。
    #[serde(default)]
    pub(crate) required_field_ids: Vec<String>,
    /// 触发列；命中 `trigger_values` 才评论。缺省表示只看 required。
    #[serde(default)]
    pub(crate) trigger_field_id: Option<String>,
    /// 触发值（与单选/文本的展示文案精确比较）。
    #[serde(default)]
    pub(crate) trigger_values: Vec<String>,
    /// 审批评论状态列 field_id。
    #[serde(default)]
    pub(crate) status_field_id: String,
    /// 状态列名（保存时解析并缓存；worker 写回时优先现场解析）。
    #[serde(default)]
    pub(crate) status_field_name: String,
    /// 状态文案。
    #[serde(default)]
    pub(crate) status_labels: CommentStatusLabels,
    /// 评论作者（运维账号）open_id。
    #[serde(default)]
    pub(crate) author_user_id: String,
    /// 是否把附件列的文件加进评论 files。
    #[serde(default = "default_true")]
    pub(crate) include_attachments: bool,
    /// 评论前缀；标记文本 = prefix + record_id。
    #[serde(default = "default_prefix")]
    pub(crate) prefix: String,
    /// 目标记录链接模板，必须含 {base_token} / {table_id} / {record_id}。
    #[serde(default)]
    pub(crate) record_url_template: String,
}

fn default_true() -> bool {
    true
}

fn default_prefix() -> String {
    "【支付回填】".to_string()
}

impl CommentConfig {
    /// 参与渲染的 field_id 列表（保持配置顺序、去重）。
    pub(crate) fn render_field_ids(&self) -> Vec<&str> {
        let mut seen = BTreeSet::new();
        self.field_ids
            .iter()
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty() && seen.insert(*value))
            .collect()
    }

    /// 触发列是否属于评论关心的字段（自触发过滤用）。
    ///
    /// 同时接受 field_id 与保存时缓存的列名：不同租户/版本的多维表格事件在
    /// `changed_fields` 里可能给 id，也可能给列名，只认 id 会让方案 A 静默失效。
    pub(crate) fn is_relevant_field(&self, field: &str) -> bool {
        let field = field.trim();
        self.field_ids.iter().any(|value| {
            let value = value.trim();
            value == field || self.field_name(value) == field
        })
    }

    /// 评论标记文本：prefix + 记录 id。
    pub(crate) fn marker(&self, record_id: &str) -> String {
        format!("{}{}", self.prefix, record_id)
    }

    /// 解析列名：缓存优先，缺省退回 field_id。
    pub(crate) fn field_name<'a>(&'a self, field_id: &'a str) -> &'a str {
        self.field_names
            .get(field_id)
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(field_id)
    }
}

impl Default for CommentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            field_ids: Vec::new(),
            field_names: BTreeMap::new(),
            required_field_ids: Vec::new(),
            trigger_field_id: None,
            trigger_values: Vec::new(),
            status_field_id: String::new(),
            status_field_name: String::new(),
            status_labels: CommentStatusLabels::default(),
            author_user_id: String::new(),
            include_attachments: true,
            prefix: default_prefix(),
            record_url_template: String::new(),
        }
    }
}

impl Default for CommentStatusLabels {
    fn default() -> Self {
        Self {
            commented: default_commented_label(),
            failed: default_failed_label(),
        }
    }
}

/// 从库内 JSON 文本解析配置。
///
/// `None` / 空串返回 `Ok(None)`（该规则不启用评论）；结构不合法返回可行动文案。
pub(crate) fn parse_comment_config(raw: Option<&str>) -> Result<Option<CommentConfig>, String> {
    let Some(raw) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let config: CommentConfig = serde_json::from_str(raw)
        .map_err(|error| format!("审批评论配置不是合法的 JSON：{error}"))?;
    validate_local(&config).map_err(|reasons| reasons.join("；"))?;
    Ok(Some(config))
}

/// 仅本地的结构校验（不访问飞书）。返回全部原因。
fn validate_local(config: &CommentConfig) -> Result<(), Vec<String>> {
    if !config.enabled {
        return Ok(());
    }
    let mut reasons = Vec::new();
    let field_ids: Vec<&str> = config
        .field_ids
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect();
    if field_ids.is_empty() {
        reasons.push("审批评论启用时必须至少配置一个字段".to_string());
    }
    let mut seen = BTreeSet::new();
    for field_id in &field_ids {
        if !seen.insert(*field_id) {
            reasons.push(format!("审批评论字段「{field_id}」重复"));
        }
    }
    let configured: BTreeSet<&str> = seen.clone();
    for required in &config.required_field_ids {
        let required = required.trim();
        if !required.is_empty() && !configured.contains(required) {
            reasons.push(format!(
                "必填字段「{required}」必须同时出现在评论字段列表中"
            ));
        }
    }
    if let Some(trigger) = config.trigger_field_id.as_deref().map(str::trim) {
        if !trigger.is_empty() {
            if !configured.contains(trigger) {
                reasons.push(format!("触发字段「{trigger}」必须同时出现在评论字段列表中"));
            }
            if config
                .trigger_values
                .iter()
                .all(|value| value.trim().is_empty())
            {
                reasons.push("配置了触发字段时必须至少给出一个触发值".to_string());
            }
        }
    }
    if config.status_field_id.trim().is_empty() {
        reasons.push("审批评论状态列 field_id 不能为空".to_string());
    }
    if config.author_user_id.trim().is_empty() {
        reasons.push("评论作者 open_id 不能为空".to_string());
    }
    if config.status_labels.commented.trim().is_empty() {
        reasons.push("评论状态文案「已评论」不能为空".to_string());
    }
    if config.status_labels.failed.trim().is_empty() {
        reasons.push("评论状态文案「评论失败」不能为空".to_string());
    }
    if config.prefix.trim().is_empty() {
        reasons.push("评论前缀不能为空（去重标记依赖 prefix + 记录 id）".to_string());
    }
    for placeholder in ["{base_token}", "{table_id}", "{record_id}"] {
        if !config.record_url_template.contains(placeholder) {
            reasons.push(format!(
                "记录链接模板缺少占位符 {placeholder}（{{base_token}} / {{table_id}} / {{record_id}} 均为必需）"
            ));
        }
    }
    if reasons.is_empty() {
        Ok(())
    } else {
        Err(reasons)
    }
}

/// 保存期校验 + 规范化：解析列名并把 `field_names` / `status_field_name` 写回。
///
/// 返回落库用的紧凑 JSON；`None` 表示没有启用评论配置。所有原因一次性返回。
pub(crate) fn validate_comment_config(
    raw: Option<&str>,
    fields: &[FieldItem],
) -> Result<Option<String>, Vec<String>> {
    let Some(mut config) = parse_comment_config(raw).map_err(|reason| vec![reason])? else {
        return Ok(None);
    };
    if !config.enabled {
        return serde_json::to_string(&config)
            .map(Some)
            .map_err(|error| vec![format!("审批评论配置序列化失败：{error}")]);
    }

    let mut reasons = Vec::new();
    let field_of = |field_id: &str| {
        fields
            .iter()
            .find(|field| field.field_id.trim() == field_id.trim())
    };

    config.field_ids = dedup_nonempty(&config.field_ids);
    config.required_field_ids = dedup_nonempty(&config.required_field_ids);
    config.trigger_values = dedup_nonempty(&config.trigger_values);
    config.trigger_field_id = config
        .trigger_field_id
        .take()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    config.status_field_id = config.status_field_id.trim().to_string();
    config.status_field_name = config.status_field_name.trim().to_string();
    config.author_user_id = config.author_user_id.trim().to_string();
    config.prefix = config.prefix.trim().to_string();
    config.record_url_template = config.record_url_template.trim().to_string();
    config.status_labels.commented = config.status_labels.commented.trim().to_string();
    config.status_labels.failed = config.status_labels.failed.trim().to_string();

    let mut field_names = BTreeMap::new();
    for field_id in &config.field_ids {
        match field_of(field_id) {
            Some(field) => {
                field_names.insert(field_id.clone(), field.field_name.trim().to_string());
            }
            None => reasons.push(format!("目标表里找不到评论字段 field_id「{field_id}」")),
        }
    }
    config.field_names = field_names;

    match field_of(&config.status_field_id) {
        Some(field) => {
            if field.field_type != Some(3) {
                reasons.push(format!(
                    "审批评论状态列「{}」必须是单选列（当前类型 {:?}）",
                    field.field_name, field.field_type
                ));
            }
            let options = field.option_names();
            if !options.is_empty() {
                for label in [
                    config.status_labels.commented.as_str(),
                    config.status_labels.failed.as_str(),
                ] {
                    if !options
                        .iter()
                        .any(|option| labels_equivalent(option, label))
                    {
                        reasons.push(format!(
                            "审批评论状态列「{}」缺少单选选项「{label}」",
                            field.field_name
                        ));
                    }
                }
            }
            config.status_field_name = field.field_name.trim().to_string();
        }
        None => reasons.push(format!(
            "目标表里找不到审批评论状态列 field_id「{}」",
            config.status_field_id
        )),
    }

    if let Some(trigger) = config.trigger_field_id.as_deref() {
        if field_of(trigger).is_none() {
            reasons.push(format!("目标表里找不到触发字段 field_id「{trigger}」"));
        }
    }

    if reasons.is_empty() {
        if let Err(error) = validate_local(&config) {
            reasons.extend(error);
        }
    }
    if !reasons.is_empty() {
        return Err(reasons);
    }
    serde_json::to_string(&config)
        .map(Some)
        .map_err(|error| vec![format!("审批评论配置序列化失败：{error}")])
}

fn dedup_nonempty(values: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && seen.insert(value.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(field_id: &str, name: &str, field_type: i32, options: &[&str]) -> FieldItem {
        let mut value = serde_json::json!({
            "field_id": field_id,
            "field_name": name,
            "type": field_type,
        });
        if !options.is_empty() {
            value["property"] = serde_json::json!({
                "options": options.iter().map(|name| serde_json::json!({"name": name})).collect::<Vec<_>>()
            });
        }
        serde_json::from_value(value).unwrap_or_else(|error| panic!("{error}"))
    }

    fn valid_raw() -> String {
        serde_json::json!({
            "enabled": true,
            "field_ids": ["fldBB53wVj","fldHDHvXIH","fldaeNR1GA"],
            "required_field_ids": ["fldBB53wVj","fldaeNR1GA"],
            "trigger_field_id": "fldHDHvXIH",
            "trigger_values": ["支付成功"],
            "status_field_id": "fldYArfJiV",
            "status_labels": {"commented":"已评论","failed":"评论失败"},
            "author_user_id": "ou_ops",
            "include_attachments": true,
            "prefix": "【支付回填】",
            "record_url_template": "https://tenant/base/{base_token}?table={table_id}&record={record_id}"
        })
        .to_string()
    }

    fn fields() -> Vec<FieldItem> {
        vec![
            field("fldBB53wVj", "支付ID", 1, &[]),
            field("fldHDHvXIH", "支付状态", 3, &["支付成功", "支付失败"]),
            field("fldaeNR1GA", "支付凭证", 17, &[]),
            field("fldYArfJiV", "审批评论状态", 3, &["已评论", "评论失败"]),
        ]
    }

    #[test]
    fn a_valid_config_is_normalised_with_resolved_field_names() {
        let normalized = validate_comment_config(Some(&valid_raw()), &fields())
            .unwrap_or_else(|reasons| panic!("{reasons:?}"))
            .unwrap_or_else(|| panic!("启用配置不应为空"));
        let config = parse_comment_config(Some(&normalized))
            .unwrap_or_else(|error| panic!("{error}"))
            .unwrap_or_else(|| panic!("应解析出配置"));
        assert_eq!(
            config.field_names.get("fldBB53wVj").map(String::as_str),
            Some("支付ID")
        );
        assert_eq!(config.status_field_name, "审批评论状态");
        assert!(config.include_attachments);
    }

    #[test]
    fn required_subset_trigger_and_placeholders_are_enforced() {
        let mut value: serde_json::Value = serde_json::from_str(&valid_raw())
            .unwrap_or_else(|error| panic!("valid raw config must parse: {error}"));
        value["required_field_ids"] = serde_json::json!(["fldUnknown"]);
        value["trigger_field_id"] = serde_json::json!("fldNotConfigured");
        value["record_url_template"] = serde_json::json!("https://tenant/base/{record_id}");
        let reasons = parse_comment_config(Some(&value.to_string()))
            .err()
            .unwrap_or_default();
        assert!(
            reasons.contains("必须同时出现在评论字段列表中"),
            "{reasons}"
        );
        assert!(reasons.contains("记录链接模板缺少占位符"), "{reasons}");
    }

    #[test]
    fn status_column_must_be_single_select_with_both_options() {
        let mut bad = fields();
        bad[3] = field("fldYArfJiV", "审批评论状态", 1, &[]);
        let reasons = validate_comment_config(Some(&valid_raw()), &bad)
            .err()
            .unwrap_or_default();
        assert!(
            reasons.iter().any(|reason| reason.contains("必须是单选列")),
            "{reasons:?}"
        );

        let mut missing = fields();
        missing[3] = field("fldYArfJiV", "审批评论状态", 3, &["已评论"]);
        let reasons = validate_comment_config(Some(&valid_raw()), &missing)
            .err()
            .unwrap_or_default();
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains("缺少单选选项「评论失败」")),
            "{reasons:?}"
        );
    }

    #[test]
    fn disabled_config_skips_deep_checks_but_is_kept() {
        let raw = r#"{"enabled":false}"#;
        let normalized =
            validate_comment_config(Some(raw), &[]).unwrap_or_else(|reasons| panic!("{reasons:?}"));
        assert!(normalized.is_some());
        let config = parse_comment_config(normalized.as_deref())
            .unwrap_or_else(|error| panic!("{error}"))
            .unwrap_or_else(|| panic!("应保留配置"));
        assert!(!config.enabled);
    }

    #[test]
    fn unknown_keys_and_missing_author_are_rejected() {
        let mut value: serde_json::Value = serde_json::from_str(&valid_raw())
            .unwrap_or_else(|error| panic!("valid raw config must parse: {error}"));
        value["author_user_id"] = serde_json::json!(" ");
        value["typo_key"] = serde_json::json!(true);
        let reasons = parse_comment_config(Some(&value.to_string()))
            .err()
            .unwrap_or_default();
        assert!(reasons.contains("unknown field"), "{reasons}");
    }

    #[test]
    fn missing_field_ids_are_reported_against_the_target_table() {
        let mut value: serde_json::Value = serde_json::from_str(&valid_raw())
            .unwrap_or_else(|error| panic!("valid raw config must parse: {error}"));
        value["field_ids"] = serde_json::json!(["fldBB53wVj", "fldGone"]);
        value["required_field_ids"] = serde_json::json!(["fldBB53wVj"]);
        value["trigger_field_id"] = serde_json::json!("fldBB53wVj");
        let reasons = validate_comment_config(Some(&value.to_string()), &fields())
            .err()
            .unwrap_or_default();
        assert!(
            reasons.iter().any(|reason| reason.contains("fldGone")),
            "{reasons:?}"
        );
    }
}

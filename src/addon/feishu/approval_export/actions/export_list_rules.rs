//! 审批 → 多维表格回写规则列表（前端控制台）。
//!
//! 与 `list_configs` 同构：规则行 + 按 `rule_id` 分组的一次性映射查询。
//! 返回条件与状态文案的**解析结果**而不是原始 JSON 字符串，向导可以直接回填；
//! 解析失败时给 `null`，不阻塞整页。

use std::collections::HashMap;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::Serialize;
use yang_base::action::builtin::OrderByItem;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec};
use yang_base::table::{Record, SortOrder};
use yang_base::BaseError;

use crate::addon::feishu::domain::approval_export::{parse_option_map, StatusLabels};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::list_input::ListInput;
use crate::addon::feishu::domain::repository::all_pages;

/// 规则行投影列。
const RULE_ITEM_COLUMNS: &[&str] = &[
    "id",
    "title",
    "approval_code",
    "approval_name",
    "trigger_mode",
    "trigger_node_ids",
    "require_final_approval",
    "detail_widget_id",
    "row_filter",
    "condition_json",
    "target_base_token",
    "target_table_id",
    "target_base_name",
    "target_table_name",
    "status_field",
    "status_field_name",
    "status_labels_json",
    "mark_enabled",
    "enabled",
    "snapshot_at",
    "effective_from",
    "updated_at",
];

/// 映射行投影列。
const MAP_ITEM_COLUMNS: &[&str] = &[
    "rule_id",
    "source_kind",
    "source_widget_id",
    "source_system_key",
    "constant_value",
    "target_field_id",
    "target_field_name",
    "converter",
    "option_map",
    "required",
];

/// 一条字段映射的对外视图。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(super) struct ExportMapItem {
    /// `detail` / `top` / `system` / `constant`。
    source_kind: String,
    /// 控件来源的控件 id；其余为空串。
    source_widget_id: String,
    /// 系统字段来源的键；其余为空串。
    source_system_key: String,
    constant_value: Option<String>,
    /// 目标列 field_id。
    target_field_id: String,
    /// 保存时缓存的列名（仅供展示；列改名后运行期按 id 重新解析）。
    target_field_name: Option<String>,
    /// `text` / `number` / `date` / `person` / `option_text` / `multi_option` /
    /// `checkbox` / `url` / `json`。
    converter: String,
    /// 选项文案映射（审批文案 → 目标选项名）。
    option_map: serde_json::Value,
    /// 目标列是否必填。
    required: bool,
}

/// 一条回写规则。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ExportRuleItem {
    id: i64,
    title: String,
    approval_code: String,
    approval_name: Option<String>,
    /// `node_passed` / `final_approved`。
    trigger_mode: String,
    trigger_node_ids: Vec<String>,
    require_final_approval: bool,
    detail_widget_id: String,
    /// `matched_rows` / `all_rows`。
    row_filter: String,
    /// 条件 JSON 的解析结果；无 / 解析失败时为 `null`（原串仍在 `condition_json`）。
    condition: Option<serde_json::Value>,
    condition_json: Option<String>,
    target_base_token: String,
    target_table_id: String,
    target_base_name: Option<String>,
    target_table_name: Option<String>,
    status_field: Option<String>,
    status_field_name: Option<String>,
    /// 生效状态文案（默认值与覆盖值合并后）。
    status_labels: serde_json::Value,
    mark_enabled: bool,
    enabled: bool,
    snapshot_at: Option<i64>,
    effective_from: Option<i64>,
    updated_at: i64,
    maps: Vec<ExportMapItem>,
}

fn group_maps(rows: &[Record]) -> Result<HashMap<i64, Vec<ExportMapItem>>, BaseError> {
    let mut groups: HashMap<i64, Vec<ExportMapItem>> = HashMap::new();
    for row in rows {
        let rule_id: i64 = row.require("rule_id")?;
        let option_map_raw: Option<String> = row.optional("option_map")?;
        let option_map = parse_option_map(option_map_raw.as_deref())
            .ok()
            .map(|map| serde_json::to_value(map).unwrap_or(serde_json::Value::Null))
            .unwrap_or_else(|| serde_json::json!({}));
        groups.entry(rule_id).or_default().push(ExportMapItem {
            source_kind: row.require("source_kind")?,
            source_widget_id: row.optional("source_widget_id")?.unwrap_or_default(),
            source_system_key: row.optional("source_system_key")?.unwrap_or_default(),
            constant_value: row.optional("constant_value")?,
            target_field_id: row.require("target_field_id")?,
            target_field_name: row.optional("target_field_name")?,
            converter: row.optional("converter")?.unwrap_or_default(),
            option_map,
            required: row.optional("required")?.unwrap_or(false),
        });
    }
    Ok(groups)
}

fn parse_trigger_nodes(raw: Option<String>) -> Vec<String> {
    raw.map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
        .unwrap_or_default()
}

fn status_labels_value(raw: Option<String>) -> serde_json::Value {
    let labels = StatusLabels::parse(raw.as_deref()).unwrap_or_default();
    serde_json::json!({
        "valid": labels.valid,
        "rolled_back": labels.rolled_back,
        "rejected": labels.rejected,
        "canceled": labels.canceled,
        "reverted": labels.reverted,
        "deleted": labels.deleted,
    })
}

/// 注册回写规则列表端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("export_list_rules"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/export/rules/query",
        )
        .display_name("审批回写规则列表")
        .description("分页查询审批回写规则及其字段映射")
        .permissions(["feishu.approval.read"])
        .register()
}

pub(super) async fn handle(
    _ctx: ActionContext,
    input: ListInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let mut query = context
        .approval_export_rules()
        .query()
        .select_fields(RULE_ITEM_COLUMNS)?
        .search(input.search.as_deref())?;
    if let Some(tree) = input.where_clause {
        query = query.where_tree(tree)?;
    }
    let total = if input.count_total {
        Some(query.clone().count().await?)
    } else {
        None
    };
    let mut ordered = false;
    for OrderByItem { field, direction } in input.order_by {
        query = query.order_by(&field, direction)?;
        ordered = true;
    }
    if !ordered {
        query = query.order_by("updated_at", SortOrder::Desc)?;
        query = query.order_by("id", SortOrder::Asc)?;
    }
    let rows = query
        .page(input.page as usize, input.page_size as usize)?
        .all()
        .await?;

    let ids: Vec<serde_json::Value> = rows
        .iter()
        .map(|record| Ok(serde_json::json!(record.require::<i64>("id")?)))
        .collect::<Result<Vec<_>, BaseError>>()?;
    let mut groups = if ids.is_empty() {
        HashMap::new()
    } else {
        group_maps(
            &all_pages(
                context
                    .approval_export_field_maps()
                    .query()
                    .select_fields(MAP_ITEM_COLUMNS)?
                    .where_in("rule_id", ids)?,
                20,
            )
            .await?,
        )?
    };

    let mut items = Vec::with_capacity(rows.len());
    for record in rows.iter() {
        let id: i64 = record.require("id")?;
        let condition_raw: Option<String> = record.optional("condition_json")?;
        let condition = condition_raw
            .as_deref()
            .map(str::trim)
            .filter(|raw| !raw.is_empty())
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok());
        // 解析失败时 `condition` 为 null，原串仍在 `condition_json` 里；
        // 前端据此提示用户重新保存，而不是把整页打成失败。
        items.push(ExportRuleItem {
            id,
            title: record.require("title")?,
            approval_code: record.require("approval_code")?,
            approval_name: record.optional("approval_name")?,
            trigger_mode: record.require("trigger_mode")?,
            trigger_node_ids: parse_trigger_nodes(record.optional("trigger_node_ids")?),
            require_final_approval: record.require("require_final_approval")?,
            detail_widget_id: record.require("detail_widget_id")?,
            row_filter: record.require("row_filter")?,
            condition,
            condition_json: condition_raw,
            target_base_token: record.require("target_base_token")?,
            target_table_id: record.require("target_table_id")?,
            target_base_name: record.optional("target_base_name")?,
            target_table_name: record.optional("target_table_name")?,
            status_field: record.optional("status_field")?,
            status_field_name: record.optional("status_field_name")?,
            status_labels: status_labels_value(record.optional("status_labels_json")?),
            mark_enabled: record.require("mark_enabled")?,
            enabled: record.require("enabled")?,
            snapshot_at: record.optional("snapshot_at")?,
            effective_from: record.optional("effective_from")?,
            updated_at: record.require("updated_at")?,
            maps: groups.remove(&id).unwrap_or_default(),
        });
    }

    Ok(ApiResponse::success_value(
        serde_json::json!({
            "items": items,
            "page": input.page,
            "page_size": input.page_size,
            "total": total,
        }),
        "查询成功",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_nodes_are_tolerated_when_absent_or_malformed() {
        assert!(parse_trigger_nodes(None).is_empty());
        assert!(parse_trigger_nodes(Some("  ".to_string())).is_empty());
        assert!(parse_trigger_nodes(Some("not json".to_string())).is_empty());
        assert_eq!(
            parse_trigger_nodes(Some(r#"["n1","n2"]"#.to_string())),
            vec!["n1".to_string(), "n2".to_string()]
        );
    }

    #[test]
    fn status_labels_defaults_are_returned_as_an_object() {
        let value = status_labels_value(None);
        assert_eq!(value["valid"], "有效");
        assert_eq!(value["reverted"], "已撤销");
    }

    #[test]
    fn maps_are_grouped_by_rule() {
        let mut first = Record::new();
        first.insert("rule_id", serde_json::json!(1));
        first.insert("source_kind", serde_json::json!("detail"));
        first.insert("source_widget_id", serde_json::json!("amount"));
        first.insert("target_field_id", serde_json::json!("fldA"));
        first.insert("converter", serde_json::json!("number"));
        let mut second = Record::new();
        second.insert("rule_id", serde_json::json!(1));
        second.insert("source_kind", serde_json::json!("system"));
        second.insert("source_widget_id", serde_json::json!(""));
        second.insert("source_system_key", serde_json::json!("instance_code"));
        second.insert("target_field_id", serde_json::json!("fldB"));
        second.insert("converter", serde_json::json!("text"));
        let groups = group_maps(&[first, second]).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(groups.get(&1).map(Vec::len), Some(2));
        assert_eq!(groups[&1][1].source_system_key, "instance_code");
    }
}

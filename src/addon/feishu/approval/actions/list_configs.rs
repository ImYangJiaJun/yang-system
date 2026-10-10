//! 审批派发配置列表（前端控制台）。

use std::collections::HashMap;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::Serialize;
use yang_base::action::builtin::OrderByItem;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec};
use yang_base::table::{Record, SortOrder};
use yang_base::BaseError;

use crate::addon::feishu::domain::approval_match::mapping_widgets;
use crate::addon::feishu::domain::approval_provision::parse_form;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::list_input::ListInput;
use crate::addon::feishu::domain::repository::all_pages;

/// 配置行投影列。
///
/// `form_snapshot` 仅在内部用于解析控件名称，不投影到响应。
pub(super) const CONFIG_ITEM_COLUMNS: &[&str] = &[
    "id",
    "title",
    "base_token",
    "table_id",
    "approval_code",
    "applicant_field",
    "backfill_field",
    "base_timezone",
    "enabled",
    "form_snapshot_at",
    "updated_at",
];

/// 字段映射的投影列。映射数组由 `group_maps` 组装（`config_id` 只用于分组，
/// 不出现在列表项里）。
pub(super) const MAP_ITEM_COLUMNS: &[&str] = &[
    "config_id",
    "widget_id",
    "widget_type",
    "bitable_field",
    "bitable_field_name",
    "required",
    "converter",
];

/// 一条字段映射的对外视图。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(super) struct FieldMapItem {
    /// 审批控件 id。
    widget_id: String,
    /// 明细子控件名称包含父控件前缀；旧快照不可用时为空。
    widget_name: Option<String>,
    /// 审批控件类型（`input` / `radioV2` / …）。
    widget_type: String,
    /// 多维表格字段 ID。对齐靠它（id 不随改名变）。
    bitable_field: String,
    /// 多维表格字段名（缓存，仅供展示）。
    bitable_field_name: Option<String>,
    /// 该控件在审批定义里是否必填。
    required: bool,
    /// 值转换器：`direct` / `date` / `option`。
    converter: String,
}

/// 把映射行按 `config_id` 分组。
///
/// 一次 `where_in` 取回本页所有映射再分组，避免按行 N+1 查询（照
/// `list_datasources::group_bindings` 模式）。
pub(super) fn group_maps(rows: &[Record]) -> Result<HashMap<i64, Vec<FieldMapItem>>, BaseError> {
    let mut groups: HashMap<i64, Vec<FieldMapItem>> = HashMap::new();
    for row in rows {
        let config_id: i64 = row.require("config_id")?;
        groups.entry(config_id).or_default().push(FieldMapItem {
            widget_id: row.require("widget_id")?,
            widget_name: None,
            widget_type: row.require("widget_type")?,
            bitable_field: row.require("bitable_field")?,
            bitable_field_name: row.optional("bitable_field_name")?,
            required: row.optional("required")?.unwrap_or(false),
            converter: row
                .optional("converter")?
                .unwrap_or_else(|| "direct".to_string()),
        });
    }
    Ok(groups)
}

/// 列表项。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ConfigItem {
    /// 主键。更新 / 删除都按它定位。
    id: i64,
    /// 展示名（建配置时由审批定义名自动生成）。
    title: String,
    /// 多维表格 token。
    base_token: String,
    /// 数据表 id。
    table_id: String,
    /// 审批定义 code。
    approval_code: String,
    /// 申请人员字段 ID（存 id：改列名不会让配置失效）。
    applicant_field: String,
    /// 回填字段 ID。
    backfill_field: String,
    /// Base 时区（IANA 名）。
    base_timezone: String,
    /// 是否启用。
    enabled: bool,
    /// 控件结构快照时间（unix 秒）；建配置时快照拉取失败则为 `None`。
    form_snapshot_at: Option<i64>,
    /// 该行最后一次被写入的时间（unix 秒）。
    updated_at: i64,
    /// 字段映射。空数组表示该配置没有映射。
    maps: Vec<FieldMapItem>,
}

/// 注册审批配置列表端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("list_configs"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/approval/configs/query")
        .display_name("审批派发配置列表")
        .description("分页查询飞书审批派发配置")
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
        .approval_configs()
        .query()
        .select_fields(&[CONFIG_ITEM_COLUMNS, &["form_snapshot"]].concat())?
        .search(input.search.as_deref())?;

    if let Some(tree) = input.where_clause {
        query = query.where_tree(tree)?;
    }
    let total = if input.count_total {
        Some(query.clone().count().await?)
    } else {
        None
    };
    // 客户端没给排序时兜底：分页要有确定性全序，否则翻页会漏行或重复。
    let mut ordered = false;
    for OrderByItem { field, direction } in input.order_by {
        query = query.order_by(&field, direction)?;
        ordered = true;
    }
    if !ordered {
        query = query.order_by("updated_at", SortOrder::Desc)?;
        query = query.order_by("id", SortOrder::Asc)?;
    }
    query = query.page(input.page as usize, input.page_size as usize)?;
    let rows = query.all().await?;

    // 本页所有映射一次取回再分组——按行查是 N+1。
    // `where_in` 拒绝空列表，而空页是合法结果，所以这里要短路。
    let ids: Vec<serde_json::Value> = rows
        .iter()
        .map(|record| Ok(serde_json::json!(record.require::<i64>("id")?)))
        .collect::<Result<Vec<_>, BaseError>>()?;
    let mut groups = if ids.is_empty() {
        HashMap::new()
    } else {
        let map_rows = all_pages(
            context
                .approval_field_maps()
                .query()
                .select_fields(MAP_ITEM_COLUMNS)?
                .where_in("config_id", ids)?,
            100,
        )
        .await?;
        group_maps(&map_rows)?
    };

    let mut items = Vec::with_capacity(rows.len());
    for record in rows.iter() {
        let id: i64 = record.require("id")?;
        let mut maps = groups.remove(&id).unwrap_or_default();
        if let Some(snapshot) = record.optional::<String>("form_snapshot")? {
            if let Ok(form) = parse_form(&serde_json::Value::String(snapshot)) {
                let names = mapping_widgets(&form);
                for map in &mut maps {
                    map.widget_name = names
                        .iter()
                        .find(|(w, _)| w.id == map.widget_id)
                        .map(|(_, name)| name.clone());
                }
            }
        }
        items.push(ConfigItem {
            id,
            title: record.require("title")?,
            base_token: record.require("base_token")?,
            table_id: record.require("table_id")?,
            approval_code: record.require("approval_code")?,
            applicant_field: record.require("applicant_field")?,
            backfill_field: record.require("backfill_field")?,
            base_timezone: record.require("base_timezone")?,
            enabled: record.require("enabled")?,
            form_snapshot_at: record.optional("form_snapshot_at")?,
            updated_at: record.require("updated_at")?,
            // 没配映射的配置得到空数组，不是缺键——前端不必到处补 `?? []`
            maps,
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

    fn map_row(config_id: i64, widget_id: &str, bitable_field: &str, required: bool) -> Record {
        let mut row = Record::new();
        row.insert("config_id", serde_json::json!(config_id));
        row.insert("widget_id", serde_json::json!(widget_id));
        row.insert("widget_type", serde_json::json!("input"));
        row.insert("bitable_field", serde_json::json!(bitable_field));
        row.insert("required", serde_json::json!(required));
        row
    }

    #[test]
    fn maps_are_grouped_by_config() {
        // 一次 where_in 取回本页所有映射再分组，避免按行 N+1 查询
        let rows = vec![
            map_row(1, "w1", "fldA", true),
            map_row(1, "w2", "fldB", false),
            map_row(2, "w3", "fldC", true),
        ];
        let groups = group_maps(&rows).unwrap_or_else(|error| panic!("应可分组: {error}"));
        assert_eq!(groups.len(), 2);
        assert_eq!(groups.get(&1).map(Vec::len), Some(2));
        assert_eq!(groups.get(&2).map(Vec::len), Some(1));
        assert_eq!(groups.get(&1).map(|v| v[0].widget_id.as_str()), Some("w1"));
        assert!(!groups.get(&1).map(|v| v[1].required).unwrap_or(true));
    }

    #[test]
    fn empty_map_set_yields_no_groups() {
        assert!(group_maps(&[])
            .unwrap_or_else(|error| panic!("应可分组: {error}"))
            .is_empty());
    }

    /// 列表项序列化**不得**出现 `form_snapshot` 大字段——投影与列表项两处都要钉住。
    #[test]
    fn the_item_never_serializes_the_form_snapshot() {
        let item = ConfigItem {
            id: 1,
            title: "测试配置".to_string(),
            base_token: "appTest".to_string(),
            table_id: "tblTest".to_string(),
            approval_code: "CODE-TEST".to_string(),
            applicant_field: "fldP".to_string(),
            backfill_field: "fldB".to_string(),
            base_timezone: "Asia/Shanghai".to_string(),
            enabled: true,
            form_snapshot_at: Some(1_700_000_000),
            updated_at: 1_700_000_001,
            maps: Vec::new(),
        };
        let value =
            serde_json::to_value(&item).unwrap_or_else(|error| panic!("应可序列化: {error}"));
        assert!(
            !value
                .as_object()
                .map(|object| object.contains_key("form_snapshot"))
                .unwrap_or(true),
            "列表项不得携带 form_snapshot 大字段"
        );
        assert_eq!(value.get("maps"), Some(&serde_json::json!([])));
    }
}

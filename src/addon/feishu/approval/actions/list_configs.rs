//! 审批派发配置列表（前端控制台）。

use std::collections::{HashMap, HashSet};
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
    "base_name",
    "table_name",
    "approval_name",
    "applicant_field_name",
    "backfill_field_name",
    "batch_scope_field_name",
    "base_token",
    "table_id",
    "approval_code",
    "approval_id",
    "applicant_field",
    "backfill_field",
    "batch_scope_field",
    "status_field",
    "flow_field",
    "start_time_field",
    "end_time_field",
    "department_field",
    "handler_field",
    "base_timezone",
    "enabled",
    "config_key",
    "token_rotated_at",
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
    "option_source_key",
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
    /// 外部选项来源；旧配置可能为空。
    option_source_key: Option<String>,
    option_source_name: Option<String>,
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
            option_source_name: None,
            widget_type: row.require("widget_type")?,
            bitable_field: row.require("bitable_field")?,
            bitable_field_name: row.optional("bitable_field_name")?,
            option_source_key: row.optional("option_source_key")?,
            required: row.optional("required")?.unwrap_or(false),
            converter: row.optional("converter")?.unwrap_or_default(),
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
    base_name: Option<String>,
    table_name: Option<String>,
    approval_name: Option<String>,
    applicant_field_name: Option<String>,
    backfill_field_name: Option<String>,
    /// 多维表格 token。
    base_token: String,
    /// 数据表 id。
    table_id: String,
    /// 审批定义 code。
    approval_code: String,
    /// 审批定义的数字 ID（控制台「打开审批定义」跳转用）；存量配置为 `None`。
    approval_id: Option<String>,
    /// 申请人员字段 ID（存 id：改列名不会让配置失效）。
    applicant_field: String,
    /// 回填字段 ID。
    backfill_field: String,
    /// 批量范围复选框列的 field_id；`None` = 未启用（播种只看回填列为空）。
    batch_scope_field: Option<String>,
    /// 批量范围复选框列的展示名（缓存；仅供控制台展示）。
    batch_scope_field_name: Option<String>,
    /// 六个可选回填目标列的 field_id（设计 §5.1）。`None` = 未配置该列回填。
    status_field: Option<String>,
    flow_field: Option<String>,
    start_time_field: Option<String>,
    end_time_field: Option<String>,
    department_field: Option<String>,
    handler_field: Option<String>,
    /// Base 时区（IANA 名）。
    base_timezone: String,
    /// 是否启用。
    enabled: bool,
    /// 工作流接入用的配置标识（进派发 URL 路径段）；未签发过则为 `None`。
    config_key: Option<String>,
    /// 最近一次轮换/签发访问 Token 的时间（unix 秒）；从未签发过则为 `None`。
    ///
    /// 刻意**不投影** `token_hash` / `token_cipher`：那是凭据本体，前端只需要
    /// 「这条配置签发过没有 / 啥时候签发的」这两位元数据（与 `list_datasources`
    /// 不投影 `token_hash` 同一条纪律，见 `list_datasources.rs` 同名注释）。
    token_rotated_at: Option<i64>,
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

    let source_keys: HashSet<String> = groups
        .values()
        .flatten()
        .filter_map(|map| map.option_source_key.clone())
        .collect();
    if !source_keys.is_empty() {
        let bindings = all_pages(
            context
                .datasource_fields()
                .query()
                .select_fields(&["source_key", "datasource_id", "field_name"])?
                .where_in(
                    "source_key",
                    source_keys
                        .into_iter()
                        .map(serde_json::Value::String)
                        .collect(),
                )?,
            100,
        )
        .await?;
        let datasource_ids = bindings
            .iter()
            .map(|row| row.require::<i64>("datasource_id"))
            .collect::<Result<HashSet<_>, _>>()?;
        let sources = if datasource_ids.is_empty() {
            Vec::new()
        } else {
            all_pages(
                context
                    .datasources()
                    .query()
                    .select_fields(&["id", "title"])?
                    .where_in(
                        "id",
                        datasource_ids
                            .into_iter()
                            .map(|id| serde_json::json!(id))
                            .collect(),
                    )?,
                100,
            )
            .await?
        };
        for map in groups.values_mut().flatten() {
            if let Some(binding) = bindings.iter().find(|row| {
                row.optional::<String>("source_key").ok().flatten().as_ref()
                    == map.option_source_key.as_ref()
            }) {
                let source_id: i64 = binding.require("datasource_id")?;
                if let Some(source) = sources
                    .iter()
                    .find(|row| row.optional::<i64>("id").ok().flatten() == Some(source_id))
                {
                    let title: String = source.require("title")?;
                    map.option_source_name =
                        Some(match binding.optional::<String>("field_name")? {
                            Some(name) => format!("{title} · {name}"),
                            None => title,
                        });
                }
            }
        }
    }
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
            base_name: record.optional("base_name")?,
            table_name: record.optional("table_name")?,
            approval_name: record.optional("approval_name")?,
            applicant_field_name: record.optional("applicant_field_name")?,
            backfill_field_name: record.optional("backfill_field_name")?,
            base_token: record.require("base_token")?,
            table_id: record.require("table_id")?,
            approval_code: record.require("approval_code")?,
            approval_id: record.optional("approval_id")?,
            applicant_field: record.require("applicant_field")?,
            backfill_field: record.require("backfill_field")?,
            batch_scope_field: record.optional("batch_scope_field")?,
            batch_scope_field_name: record.optional("batch_scope_field_name")?,
            status_field: record.optional("status_field")?,
            flow_field: record.optional("flow_field")?,
            start_time_field: record.optional("start_time_field")?,
            end_time_field: record.optional("end_time_field")?,
            department_field: record.optional("department_field")?,
            handler_field: record.optional("handler_field")?,
            base_timezone: record.require("base_timezone")?,
            enabled: record.require("enabled")?,
            config_key: record.optional("config_key")?,
            token_rotated_at: record.optional("token_rotated_at")?,
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
    fn grouped_maps_return_saved_option_source() {
        let rows = [map_row(1, "w1", "fldA", true).set("option_source_key", "fldq7lcb6y")];
        let groups = group_maps(&rows).unwrap_or_else(|error| panic!("{error}"));
        let item = serde_json::to_value(&groups[&1][0]).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(item["option_source_key"], "fldq7lcb6y");
    }

    #[test]
    fn missing_converter_is_not_misrepresented_as_direct() {
        let groups =
            group_maps(&[map_row(1, "w1", "fldA", true)]).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(groups[&1][0].converter, "");
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
            base_name: None,
            table_name: None,
            approval_name: None,
            applicant_field_name: None,
            backfill_field_name: None,
            base_token: "appTest".to_string(),
            table_id: "tblTest".to_string(),
            approval_code: "CODE-TEST".to_string(),
            approval_id: None,
            applicant_field: "fldP".to_string(),
            backfill_field: "fldB".to_string(),
            batch_scope_field: Some("fldScope".to_string()),
            batch_scope_field_name: Some("纳入批量".to_string()),
            status_field: Some("fldStatus".to_string()),
            flow_field: None,
            start_time_field: None,
            end_time_field: None,
            department_field: None,
            handler_field: Some("fldHandler".to_string()),
            base_timezone: "Asia/Shanghai".to_string(),
            enabled: true,
            config_key: Some("cfgkeytest".to_string()),
            token_rotated_at: Some(1_700_000_002),
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
        // 六个回填列按 `Option` 形态返回（未配置的列是 null，不是缺键）。
        assert_eq!(
            value.get("status_field"),
            Some(&serde_json::json!("fldStatus"))
        );
        assert_eq!(
            value.get("handler_field"),
            Some(&serde_json::json!("fldHandler"))
        );
        assert_eq!(value.get("flow_field"), Some(&serde_json::json!(null)));
        // 批量范围列同样按 `Option` 形态返回。
        assert_eq!(
            value.get("batch_scope_field"),
            Some(&serde_json::json!("fldScope"))
        );
        assert_eq!(
            value.get("batch_scope_field_name"),
            Some(&serde_json::json!("纳入批量"))
        );
    }
}

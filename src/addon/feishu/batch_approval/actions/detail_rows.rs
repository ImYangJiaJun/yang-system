//! 审批实例明细控件的分行读取：列表只回行数与首行预览，展开后按需分页取行。

use std::sync::Arc;

use serde_json::{json, Value};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, Int, ModuleSpec, Str};
use yang_base::BaseError;

use super::super::api;
use super::field_catalog;
use super::list_tasks::text_of;
use super::task_detail::{self, DetailAccess};
use crate::addon::feishu::batch_approval::domain::widget_catalog::WidgetCatalog;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::widget_rules;
use crate::addon::feishu::sso::credentials;

yang_base::params! {
    pub(super) DetailRowsInput {
        #[param(source = path)]
        task_id: Str::new().require(true),
        #[param(source = query)]
        instance_code: Str::new().require(true),
        #[param(source = query)]
        field_id: Str::new().require(true),
        #[param(source = query)]
        offset: Int::new().require(false).default(0_i64),
        #[param(source = query)]
        limit: Int::new().require(false).default(50_i64),
        #[param(source = query)]
        search: Str::new().require(false),
    }
}

pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    // auth: authenticated-only 自服务操作——登录用户只能读取自己参与的审批实例明细
    module
        .action_fn(
            yang_base::action_name!("feishu_batch_detail_rows"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Get,
            "/api/v1/feishu/batch-approval/tasks/{task_id}/rows",
        )
        .display_name("批量审批明细行")
        .description("分页读取审批实例中明细控件的行")
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: DetailRowsInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let user_id = crate::addon::feishu::sso::require_current_user(&ctx)?;
    let Some(credential) = credentials::load_credential(&ctx, &context, user_id).await? else {
        return Ok(ApiResponse::fail(40901, "尚未绑定飞书身份"));
    };
    let Some(settings) = context.settings() else {
        return Ok(ApiResponse::fail(50301, "飞书集成未配置"));
    };
    let detail = api::instance_detail(&ctx, settings, &input.instance_code)
        .await
        .map_err(|error| error.to_base())?;
    match task_detail::detail_access(&detail, &input.task_id, &credential.open_id) {
        DetailAccess::Allowed => {}
        DetailAccess::TaskNotInInstance => {
            return Ok(ApiResponse::fail(40402, "任务不属于该审批实例"));
        }
        DetailAccess::NotParticipant => {
            return Ok(ApiResponse::fail(40301, "无权查看该审批实例"));
        }
    }

    let field_id = input.field_id.trim();
    let Some((form_name, rows)) = detail_field(&detail, field_id) else {
        return Err(BaseError::ParamInvalid(
            "field_id".to_string(),
            "审批实例中不存在该明细控件".to_string(),
        ));
    };
    let catalog = match detail.get("approval_code").and_then(Value::as_str) {
        Some(code) => field_catalog::load_catalog(&ctx, &context, code).await.ok(),
        None => None,
    };
    let (columns, order) = columns(catalog.as_ref(), field_id, &rows);
    let name = catalog
        .as_ref()
        .and_then(|catalog| catalog.name(field_id))
        .unwrap_or(&form_name)
        .to_string();

    let keyword = input
        .search
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_lowercase);
    let unfiltered_total = rows.len();
    let filtered: Vec<Vec<Value>> = rows
        .into_iter()
        .filter(|row| {
            keyword
                .as_ref()
                .map_or(true, |keyword| row_matches(row, keyword))
        })
        .collect();
    let total = filtered.len();
    let offset = usize::try_from(input.offset.unwrap_or(0)).unwrap_or(0);
    let limit = usize::try_from(input.limit.unwrap_or(50).clamp(1, 500)).unwrap_or(50);
    let page: Vec<Value> = select_page(&filtered, offset, limit)
        .iter()
        .map(|row| row_json(row, &order))
        .collect();

    ApiResponse::success(
        json!({
            "field_id": field_id,
            "name": name,
            "columns": columns,
            "rows": page,
            "total": total,
            "unfiltered_total": unfiltered_total,
            "offset": offset,
            "limit": limit,
        }),
        "ok",
    )
}

fn form_rows(detail: &Value) -> Option<Vec<Value>> {
    match detail.get("form")? {
        Value::Array(rows) => Some(rows.clone()),
        Value::String(raw) => serde_json::from_str::<Value>(raw)
            .ok()
            .and_then(|value| value.as_array().cloned()),
        _ => None,
    }
}

/// 找到明细控件并解析成行数组；控件不存在或不是明细时返回 `None`。
fn detail_field(detail: &Value, field_id: &str) -> Option<(String, Vec<Vec<Value>>)> {
    let form = form_rows(detail)?;
    let row = form
        .iter()
        .find(|row| row.get("id").and_then(Value::as_str) == Some(field_id))?;
    let field_type = row.get("type").and_then(Value::as_str).unwrap_or("");
    if !field_type.eq_ignore_ascii_case("fieldList")
        && !field_type.eq_ignore_ascii_case("field_list")
    {
        return None;
    }
    let name = row
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(field_id)
        .to_string();
    let value = row
        .get("value")
        .or_else(|| row.get("field_value"))
        .cloned()
        .unwrap_or(Value::Null);
    Some((name, parse_rows(&value)))
}

fn parse_rows(value: &Value) -> Vec<Vec<Value>> {
    let rows = match value {
        Value::Array(rows) => rows.clone(),
        Value::String(raw) => serde_json::from_str::<Value>(raw)
            .ok()
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    rows.into_iter()
        .map(|row| match row {
            Value::Array(cells) => cells,
            other => vec![other],
        })
        .collect()
}

/// 列顺序优先用控件目录里的子控件顺序；目录缺失时按行首出现的顺序。
/// 目录可能缺少只在后续行出现的子控件（稀疏明细），所以始终把全部行的 id
/// 并集追加在目录之后，避免附件等列永远不可见。
fn columns(
    catalog: Option<&WidgetCatalog>,
    field_id: &str,
    rows: &[Vec<Value>],
) -> (Vec<Value>, Vec<String>) {
    let mut ids: Vec<String> = catalog
        .map(|catalog| catalog.children(field_id).to_vec())
        .unwrap_or_default();
    for row in rows {
        for cell in row {
            let Some(id) = cell_id(cell) else { continue };
            // 说明文字、容器等不可配置控件不进目录，也不该占一列；
            // 只有「目录没列出来但本身可配置」的子控件才追加（例如附件列）。
            if !cell_is_configurable(cell) {
                continue;
            }
            if !ids.iter().any(|existing| existing == id) {
                ids.push(id.to_string());
            }
        }
    }
    let columns = ids
        .iter()
        .map(|id| {
            let name = catalog.and_then(|catalog| catalog.name(id)).unwrap_or(id);
            let widget_type = catalog
                .and_then(|catalog| catalog.widget_type(id))
                .filter(|value| !value.is_empty())
                .or_else(|| first_cell_type(rows, id))
                .unwrap_or("");
            json!({"id": id, "name": name, "type": widget_type})
        })
        .collect();
    (columns, ids)
}

/// 行内 cell 的控件类型是否可配置（说明文字/容器等不进明细列）。
/// 类型缺失时保守保留：目录不可用时仍需显示行里出现的附件等列。
fn cell_is_configurable(cell: &Value) -> bool {
    cell.get("type")
        .and_then(Value::as_str)
        .map(|widget_type| widget_type.is_empty() || widget_rules::is_configurable(widget_type))
        .unwrap_or(true)
}

/// 目录里没有类型时，退回到行数据里该子控件第一个非空 `type`。
fn first_cell_type<'a>(rows: &'a [Vec<Value>], id: &str) -> Option<&'a str> {
    rows.iter()
        .flat_map(|row| row.iter())
        .filter(|cell| cell_id(cell) == Some(id))
        .find_map(|cell| {
            cell.get("type")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        })
}

fn cell_id(cell: &Value) -> Option<&str> {
    cell.get("id").and_then(Value::as_str)
}

fn row_matches(row: &[Value], keyword: &str) -> bool {
    row.iter().any(|cell| {
        let value = cell
            .get("value")
            .or_else(|| cell.get("field_value"))
            .unwrap_or(cell);
        text_of(value).to_lowercase().contains(keyword)
            || text_of(cell.get("ext").unwrap_or(&Value::Null))
                .to_lowercase()
                .contains(keyword)
    })
}

fn select_page(rows: &[Vec<Value>], offset: usize, limit: usize) -> &[Vec<Value>] {
    let start = offset.min(rows.len());
    let end = start.saturating_add(limit).min(rows.len());
    &rows[start..end]
}

fn row_json(row: &[Value], order: &[String]) -> Value {
    let cells = if order.is_empty() {
        row.iter().map(cell_json).collect()
    } else {
        let mut remaining: Vec<&Value> = row.iter().collect();
        let mut cells = Vec::with_capacity(row.len());
        for id in order {
            if let Some(index) = remaining
                .iter()
                .position(|cell| cell_id(cell) == Some(id.as_str()))
            {
                cells.push(cell_json(remaining.remove(index)));
            }
        }
        cells.extend(remaining.into_iter().map(cell_json));
        cells
    };
    Value::Array(cells)
}

fn cell_json(cell: &Value) -> Value {
    json!({
        "id": cell.get("id").cloned().unwrap_or(Value::Null),
        // 兼容飞书旧形态的 field_value；附件原名在 ext 里（单引号列表/JSON 数组）。
        "value": cell
            .get("value")
            .or_else(|| cell.get("field_value"))
            .cloned()
            .unwrap_or(Value::Null),
        "type": cell.get("type").cloned().unwrap_or(Value::Null),
        "ext": cell.get("ext").cloned().unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::{cell_json, columns, detail_field, row_json, row_matches, select_page};
    use crate::addon::feishu::domain::approval_match::FormWidget;
    use serde_json::{json, Value};

    fn detail() -> serde_json::Value {
        json!({
            "approval_code": "CODE",
            "form": [
                {"id": "title", "name": "标题", "type": "input", "value": "报销"},
                {
                    "id": "items",
                    "name": "费用明细",
                    "type": "fieldList",
                    "value": [
                        [
                            {"id": "name", "type": "input", "value": "笔记本"},
                            {"id": "price", "type": "amount", "value": 4500},
                            {"id": "note", "type": "text", "value": {"text": "含税"}}
                        ],
                        [{"id": "name", "type": "input", "value": "鼠标"}]
                    ]
                }
            ]
        })
    }

    #[test]
    fn parses_rows_and_preserves_raw_values() {
        let (name, rows) = detail_field(&detail(), "items").unwrap_or_else(|| panic!("应找到明细"));
        assert_eq!(name, "费用明细");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][1]["value"], 4500);
        assert_eq!(rows[0][2]["value"]["text"], "含税");
        assert!(detail_field(&detail(), "title").is_none());
        assert!(detail_field(&detail(), "missing").is_none());
    }

    #[test]
    fn search_matches_any_cell_case_insensitively() {
        let (_, rows) = detail_field(&detail(), "items").unwrap_or_else(|| panic!("应找到明细"));
        assert!(row_matches(&rows[0], "笔记本"));
        assert!(row_matches(&rows[0], "含税"));
        assert!(!row_matches(&rows[0], "鼠标"));
    }

    #[test]
    fn slices_filtered_rows() {
        let (_, rows) = detail_field(&detail(), "items").unwrap_or_else(|| panic!("应找到明细"));
        assert_eq!(select_page(&rows, 0, 1).len(), 1);
        assert_eq!(select_page(&rows, 1, 5).len(), 1);
        assert!(select_page(&rows, 9, 5).is_empty());
    }

    #[test]
    fn columns_and_order_come_from_the_catalog() {
        let items = FormWidget {
            id: "items".to_string(),
            name: "费用明细".to_string(),
            r#type: "fieldList".to_string(),
            required: false,
            value: json!(null),
            option: None,
            external_data: None,
            children: vec![
                FormWidget {
                    id: "price".to_string(),
                    name: "单价".to_string(),
                    r#type: "amount".to_string(),
                    required: false,
                    value: json!(null),
                    option: None,
                    external_data: None,
                    children: Vec::new(),
                },
                FormWidget {
                    id: "name".to_string(),
                    name: "物品".to_string(),
                    r#type: "input".to_string(),
                    required: false,
                    value: json!(null),
                    option: None,
                    external_data: None,
                    children: Vec::new(),
                },
            ],
        };
        let catalog = super::super::super::domain::widget_catalog::build(&[items]);
        let (_, rows) = detail_field(&detail(), "items").unwrap_or_else(|| panic!("应找到明细"));
        let (columns, order) = columns(Some(&catalog), "items", &rows);
        // 目录顺序之后只追加可配置的行内子控件；type=text 的说明列（note）
        // 与目录一样被排除，避免出现无法配置的原始 id 列。
        assert_eq!(order, ["price", "name"]);
        assert_eq!(columns[0]["name"], "单价");
        assert_eq!(columns[1]["type"], "input");
        let row = row_json(&rows[0], &order);
        assert_eq!(row[0]["id"], "price");
        assert_eq!(row[1]["id"], "name");
        assert_eq!(row[2]["id"], "note");
    }

    #[test]
    fn falls_back_to_first_row_order_without_catalog() {
        let (_, rows) = detail_field(&detail(), "items").unwrap_or_else(|| panic!("应找到明细"));
        let (columns, order) = columns(None, "items", &rows);
        assert_eq!(order, ["name", "price"]);
        assert_eq!(columns[0]["name"], "name");
    }

    #[test]
    fn cell_json_keeps_ext_and_maps_field_value() {
        let cell = json!({
            "id": "invoice",
            "type": "attachmentV2",
            "field_value": ["https://files.example.com/a.pdf"],
            "ext": "'发票.pdf'"
        });
        let value = cell_json(&cell);
        assert_eq!(value["id"], "invoice");
        assert_eq!(value["value"][0], "https://files.example.com/a.pdf");
        assert_eq!(value["type"], "attachmentV2");
        assert_eq!(value["ext"], "'发票.pdf'");

        let plain = cell_json(&json!({"id": "name", "value": "笔记本"}));
        assert_eq!(plain["ext"], Value::Null);
    }

    #[test]
    fn columns_add_ids_that_only_appear_in_later_rows() {
        let rows = vec![
            vec![json!({"id": "name", "type": "input", "value": "笔记本"})],
            vec![
                json!({"id": "name", "type": "input", "value": "鼠标"}),
                json!({
                    "id": "invoice",
                    "type": "attachmentV2",
                    "value": ["https://files.example.com/a.pdf"]
                }),
            ],
        ];
        let (columns, order) = columns(None, "items", &rows);
        assert_eq!(order, ["name", "invoice"]);
        assert_eq!(columns[1]["id"], "invoice");
        assert_eq!(columns[1]["type"], "attachmentV2");
    }

    #[test]
    fn columns_type_falls_back_to_first_non_empty_cell_type() {
        let rows = vec![
            vec![json!({"id": "invoice", "value": ["https://files.example.com/a.pdf"]})],
            vec![json!({
                "id": "invoice",
                "type": "attachmentV2",
                "value": ["https://files.example.com/b.pdf"]
            })],
        ];
        let (columns, _) = columns(None, "items", &rows);
        assert_eq!(columns[0]["type"], "attachmentV2");
    }

    #[test]
    fn row_search_matches_attachment_file_names_in_ext() {
        let row = vec![json!({
            "id": "invoice",
            "type": "attachmentV2",
            "value": ["https://files.example.com/stream/authcode/opaque"],
            "ext": "'差旅发票.pdf'"
        })];
        assert!(row_matches(&row, "差旅发票"));
        assert!(!row_matches(&row, "住宿"));
    }
}

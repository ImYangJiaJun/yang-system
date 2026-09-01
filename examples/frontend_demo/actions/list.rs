//! 为通用 TableView 提供标准分页数据。

use super::super::model::{DemoItem, DemoItems};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params, SortDirection};
use yang_base::BaseError;

fn default_page() -> usize {
    1
}

fn default_page_size() -> usize {
    20
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct DemoOrder {
    field: String,
    direction: SortDirection,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ListInput {
    #[serde(default = "default_page")]
    page: usize,
    #[serde(default = "default_page_size")]
    page_size: usize,
    #[serde(default)]
    search: Option<String>,
    #[serde(rename = "where", default)]
    where_clause: Option<yang_base::table::WhereCondition>,
    #[serde(default)]
    order_by: Vec<DemoOrder>,
    #[serde(default)]
    count_total: bool,
}

impl ParamInput for ListInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ListOutput {
    items: Vec<DemoItem>,
    page: usize,
    page_size: usize,
    total: Option<usize>,
}

fn item_value(item: &DemoItem, field: &str) -> Option<Value> {
    match field {
        "id" => Some(json!(item.id)),
        "name" => Some(json!(item.name)),
        "category_id" => Some(json!(item.category_id)),
        "status" => Some(json!(item.status)),
        "parent_id" => Some(json!(item.parent_id)),
        _ => None,
    }
}

fn matches_condition(item: &DemoItem, condition: &yang_base::table::WhereCondition) -> bool {
    use yang_base::table::WhereCondition;
    match condition {
        WhereCondition::Eq { field, value } => item_value(item, field).as_ref() == Some(value),
        WhereCondition::And { conditions } => conditions
            .iter()
            .all(|condition| matches_condition(item, condition)),
        WhereCondition::Or { conditions } => conditions
            .iter()
            .any(|condition| matches_condition(item, condition)),
        _ => false,
    }
}

pub(super) async fn handle(
    _ctx: ActionContext,
    input: ListInput,
    items: DemoItems,
) -> Result<ListOutput, BaseError> {
    if input.page == 0 || input.page_size == 0 || input.page_size > 100 {
        return Err(BaseError::ParamInvalid(
            "page/page_size".to_string(),
            "page>=1, 1<=page_size<=100".to_string(),
        ));
    }
    let mut items = items
        .read()
        .await
        .iter()
        .filter(|item| match input.search.as_ref() {
            Some(search) => item
                .name
                .to_lowercase()
                .contains(&search.trim().to_lowercase()),
            None => true,
        })
        .filter(|item| match input.where_clause.as_ref() {
            Some(condition) => matches_condition(item, condition),
            None => true,
        })
        .cloned()
        .collect::<Vec<_>>();
    for order in input.order_by.iter().rev() {
        items.sort_by(|left, right| {
            let ordering = match order.field.as_str() {
                "name" => left.name.cmp(&right.name),
                "status" => left.status.cmp(&right.status),
                "id" => left.id.cmp(&right.id),
                _ => std::cmp::Ordering::Equal,
            };
            if order.direction == SortDirection::Desc {
                ordering.reverse()
            } else {
                ordering
            }
        });
    }
    let total = items.len();
    let start = input.page.saturating_sub(1).saturating_mul(input.page_size);
    let items = items
        .into_iter()
        .skip(start)
        .take(input.page_size)
        .collect();
    Ok(ListOutput {
        items,
        page: input.page,
        page_size: input.page_size,
        total: input.count_total.then_some(total),
    })
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, items: DemoItems) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("list"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&items))
        })
        .route(HttpMethod::Post, "/api/v1/demo/items/query")
        .display_name("项目列表数据")
        .description("为通用 TableView 提供标准分页数据")
        .public()
        .register()
}

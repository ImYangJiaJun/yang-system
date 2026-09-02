//! 分页查询当前用户的便签列表（通用 TableView 的数据 Action）。
//!
//! 输入/输出与框架内置 SelectAction 的标准分页契约保持一致
//! （`page/page_size/search/where/order_by/count_total` → `items/page/page_size/total`），
//! 因此现有通用 TableView 无需任何前端代码即可消费；与内置 Action 的唯一区别是
//! 强制叠加 `owner_user_id = 当前用户` 的所有权边界。

use crate::addon::demo::domain::context::Demo;
use crate::addon::demo::notes::table::{
    CONTENT, CREATED_AT, NOTE_ID, OWNER_USER_ID, TITLE, UPDATED_AT,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use yang_base::action::builtin::OrderByItem;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::{Record, WhereCondition};
use yang_base::BaseError;

fn default_page() -> u32 {
    1
}
fn default_page_size() -> u32 {
    10
}

/// 列表查询输入；与通用 TableView 发送的标准 select 输入完全同构。
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ListNotesInput {
    /// 页码（1 起步），缺省 1
    #[serde(default = "default_page")]
    page: u32,
    /// 每页条数，缺省 10，必须 1..=100
    #[serde(default = "default_page_size")]
    page_size: u32,
    /// 在表定义声明的 searchable 字段中执行关键词搜索。
    #[serde(default)]
    search: Option<String>,
    /// where 布尔过滤树（JSON key 为 `"where"`），缺省无条件
    #[serde(rename = "where", default)]
    where_clause: Option<WhereCondition>,
    /// 排序规则列表
    #[serde(default)]
    order_by: Vec<OrderByItem>,
    /// 是否额外执行 COUNT 查询
    #[serde(default)]
    count_total: bool,
}

impl ParamInput for ListNotesInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 标准分页行数据输出。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ListNotesResult {
    items: Vec<Record>,
    page: u32,
    page_size: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<u64>,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: ListNotesInput,
) -> Result<ListNotesResult, BaseError> {
    let owner_user_id = ctx.actor()?.user_id();
    if input.page == 0 || input.page_size == 0 || input.page_size > 100 {
        return Err(BaseError::ParamInvalid(
            "page/page_size".into(),
            "page>=1, 1<=page_size<=100".into(),
        ));
    }

    let mut query = ctx.table_query()?;
    // 显式投影 View 声明的可读列（字段权限由 TableQuery 校验）。
    query = query.select_fields(&[NOTE_ID, TITLE, CONTENT, CREATED_AT, UPDATED_AT])?;
    // 所有权边界：无论客户端传什么过滤树，都只命中当前用户的便签。
    query = query.where_eq(
        OWNER_USER_ID,
        serde_json::Value::Number(owner_user_id.into()),
    )?;
    query = query.search(input.search.as_deref())?;
    if let Some(tree) = input.where_clause {
        query = query.where_tree(tree)?;
    }
    let total = if input.count_total {
        Some(query.clone().count().await?)
    } else {
        None
    };
    for OrderByItem { field, direction } in input.order_by {
        query = query.order_by(&field, direction)?;
    }
    query = query.page(input.page as usize, input.page_size as usize)?;
    let items = query.all().await?;
    Ok(ListNotesResult {
        items,
        page: input.page,
        page_size: input.page_size,
        total,
    })
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, _demo: Arc<Demo>) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("list_notes"), handle)
        .route(HttpMethod::Post, "/api/v1/demo/notes/query")
        .display_name("便签列表")
        .description("分页查询当前用户的便签（仅本人数据）")
        .permissions(["demo.notes.read"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::table::SortOrder;

    #[test]
    fn input_matches_the_standard_table_view_select_contract() {
        let input = serde_json::from_value::<ListNotesInput>(serde_json::json!({
            "page": 2,
            "page_size": 20,
            "search": "购物",
            "where": {"type": "like", "field": "title", "pattern": "%清单%"},
            "order_by": [{"field": "created_at", "direction": "Desc"}],
            "count_total": true
        }))
        .unwrap_or_else(|error| panic!("通用 TableView 发送的标准输入应可解析: {error}"));
        assert_eq!(input.page, 2);
        assert_eq!(input.page_size, 20);
        assert_eq!(input.search.as_deref(), Some("购物"));
        assert!(input.where_clause.is_some());
        assert_eq!(input.order_by.len(), 1);
        assert_eq!(input.order_by[0].direction, SortOrder::Desc);
        assert!(input.count_total);

        // 缺省值与 TableView 的首次加载请求（仅 count_total）兼容。
        let minimal = serde_json::from_value::<ListNotesInput>(serde_json::json!({
            "count_total": true
        }))
        .unwrap_or_else(|error| panic!("最小请求应可解析: {error}"));
        assert_eq!(minimal.page, 1);
        assert_eq!(minimal.page_size, 10);
        assert!(minimal.search.is_none());
        assert!(minimal.where_clause.is_none());
        assert!(minimal.order_by.is_empty());
    }

    #[test]
    fn input_rejects_unknown_fields() {
        let injected = serde_json::from_value::<ListNotesInput>(serde_json::json!({
            "page": 1,
            "owner_user_id": 42
        }));
        assert!(
            injected.is_err(),
            "客户端不能注入所有权字段，只能由服务端强制"
        );
    }

    #[test]
    fn result_serializes_standard_pagination_shape() {
        let result = ListNotesResult {
            items: Vec::new(),
            page: 1,
            page_size: 10,
            total: Some(0),
        };
        let value = serde_json::to_value(result)
            .unwrap_or_else(|error| panic!("列表结果应可序列化: {error}"));
        for key in ["items", "page", "page_size", "total"] {
            assert!(value.get(key).is_some(), "分页结构必须包含 {key}");
        }
    }
}

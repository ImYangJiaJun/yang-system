//! 选项列表（前端控制台，只读）。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use yang_base::action::builtin::OrderByItem;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec};
use yang_base::table::{Record, SortOrder};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::list_input::ListInput;

/// 选项行的对外视图。
///
/// **用结构体而不是内联 `json!({...})`**：`json!` 的键是字面量，断言不到——
/// 而那正是本仓四次同类 bug 的温床（前端读一个后端不发的键，恒得 `null`，
/// 界面把这当成「服务端说没有」）。结构体的键集由 serde 推导，可以直接与
/// `frontend/contracts/feishu-projections.json` 对账
/// （见本文件末尾的 `the_committed_contract_matches_the_item_struct`）。
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub(super) struct OptionItem {
    /// 飞书契约的选项 id（全局唯一，由唯一索引保证）。
    option_id: String,
    /// 这条选项属于哪个字段的数据源。
    source_key: String,
    label: String,
    /// 多语言的 JSON **文本**（不是对象）；前端按文本解析。
    i18n: Option<String>,
    sort_order: i64,
    is_default: bool,
    enabled: bool,
    /// 级联父键（裸 `option_id`）；无父为 `None`。
    parent_key: Option<String>,
    /// 父选项的文案（派生投影：按本页 `parent_key` 批量反查父行）。
    /// None = 父键指向的选项查不到——推送型数据源没有父键、或旧规则版本遗留行。
    parent_label: Option<String>,
    /// 直接子项数（全表里 `parent_key == 本行 option_id` 的行数；父子可跨字段，同数据源内）。
    children_count: i64,
    /// 这条选项最近一次被写入的时间（unix 秒）。**存活信号用它而不是 `updated_at`**：
    /// 拉取侧恒写它，而 `updated_at` 只在 UPDATE 时变。
    last_push_at: Option<i64>,
    updated_at: i64,
}

impl OptionItem {
    fn from_record(record: &Record) -> Result<Self, BaseError> {
        Ok(Self {
            option_id: record.require("option_id")?,
            source_key: record.require("source_key")?,
            label: record.require("label")?,
            i18n: record.optional("i18n")?,
            sort_order: record.require("sort_order")?,
            is_default: record.optional("is_default")?.unwrap_or(false),
            enabled: record.optional("enabled")?.unwrap_or(true),
            parent_key: record.optional("parent_key")?,
            last_push_at: record.optional("last_push_at")?,
            updated_at: record.require("updated_at")?,
            // 派生投影由 handle 在 rows → items 之后批量填充。
            parent_label: None,
            children_count: 0,
        })
    }
}

/// 本页所有非空父键（trim 后）去重，供 `where_in` 反查父行。
///
/// 空串也算没有父：写入侧 `derive.rs` 的 `ancestor_key` 对空级返回空串，
/// 库里可能存空字符串而不是 NULL，所以这里不能只靠 `Option` 的形状判断。
fn distinct_parent_keys(items: &[OptionItem]) -> Vec<serde_json::Value> {
    items
        .iter()
        .filter_map(|item| item.parent_key.as_deref().map(str::trim))
        .filter(|key| !key.is_empty())
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|key| serde_json::Value::String(key.to_string()))
        .collect()
}

/// 注册选项列表端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("list_options"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/options/query")
        .display_name("选项列表")
        .description("分页查询飞书选项")
        .permissions(["feishu.option.read"])
        .register()
}

pub(super) async fn handle(
    _ctx: ActionContext,
    input: ListInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let mut query = context.options().query().select_fields(&[
        "option_id",
        "source_key",
        "label",
        "i18n",
        "sort_order",
        "is_default",
        "enabled",
        // 级联父键（裸 option_id）；无父时为空串。控制台要能看出某条选项
        // 「挂在谁名下」——否则级联配错时只能看到一个孤零零的子项。
        "parent_key",
        // **存活信号用 `last_push_at` 而不是 `updated_at`。**
        // `updated_at` 只在 UPDATE 时变，补集停用之外的写入也可能不改它，
        // 用它回答「这个源还活着吗」会给出错误的肯定；拉取侧则恒写 `last_push_at`。
        "last_push_at",
        "updated_at",
    ])?;

    // 本服务自己的扩展过滤：前端不发 source_key 时不生效
    if let Some(source_key) = input.source_key.as_deref() {
        query = query.where_eq("source_key", serde_json::json!(source_key))?;
    }
    query = query.search(input.search.as_deref())?;
    if let Some(tree) = input.where_clause {
        query = query.where_tree(tree)?;
    }
    let total = if input.count_total {
        Some(query.clone().count().await?)
    } else {
        None
    };

    // 客户端没给排序时回退到 View 声明的默认排序（sort_order），
    // 并恒以 option_id 收尾——分页必须有确定性全序，否则翻页会漏行或重复。
    let mut ordered = false;
    for OrderByItem { field, direction } in input.order_by {
        query = query.order_by(&field, direction)?;
        ordered = true;
    }
    if !ordered {
        query = query
            .order_by("sort_order", SortOrder::Asc)?
            .order_by("option_id", SortOrder::Asc)?;
    }

    let rows = query
        .page(input.page as usize, input.page_size as usize)?
        .all()
        .await?;

    // i18n 落 Text 列存 JSON 文本；这里回原样，前端按文本展示即可。
    // 映射在 `OptionItem::from_record` 里——结构体化的理由是让**键集可对账**。
    let mut items = rows
        .iter()
        .map(OptionItem::from_record)
        .collect::<Result<Vec<_>, BaseError>>()?;

    // 派生投影：父级文案 + 直接子项数。
    //
    // 父行挂在**父字段自己的 source_key** 下、不在本页结果里，所以必须按 `parent_key`
    // 反查；子行同理按 `parent_key` 反查计数。本页最多 100 行、各一次 `where_in`，
    // 不是 N+1（口径照 `portal_query.rs` 的既有 `where_in("parent_key", …)` 用法）。
    // `where_in` 拒绝空列表，两个集合都先判空短路。
    let parent_keys = distinct_parent_keys(&items);
    let mut parent_labels: HashMap<String, String> = HashMap::new();
    if !parent_keys.is_empty() {
        for record in context
            .options()
            .query()
            .select_fields(&["option_id", "label"])?
            .where_in("option_id", parent_keys)?
            .all()
            .await?
        {
            parent_labels.insert(record.require("option_id")?, record.require("label")?);
        }
    }

    let option_ids: Vec<serde_json::Value> = items
        .iter()
        .map(|item| serde_json::json!(&item.option_id))
        .collect();
    let mut children_counts: HashMap<String, i64> = HashMap::new();
    if !option_ids.is_empty() {
        for record in context
            .options()
            .query()
            .select_fields(&["parent_key"])?
            .where_in("parent_key", option_ids)?
            .all()
            .await?
        {
            let Some(parent_key) = record.optional::<String>("parent_key")? else {
                continue;
            };
            // 根级行的父键是空串，挂不到任何 `option_id` 上，别给它计数。
            if parent_key.is_empty() {
                continue;
            }
            *children_counts.entry(parent_key).or_default() += 1;
        }
    }

    for item in &mut items {
        item.parent_label = item
            .parent_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .and_then(|key| parent_labels.get(key).cloned());
        item.children_count = children_counts.get(&item.option_id).copied().unwrap_or(0);
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

    use crate::addon::feishu::domain::projection_contract;

    /// 选项行的键集与契约对账。
    ///
    /// 这条本来无法存在：响应是内联 `json!({...})`，键是字面量。改成结构体之后
    /// serde 能推出键集，于是这个端点才进得了投影契约。
    #[test]
    fn the_committed_contract_matches_the_item_struct() {
        let item = OptionItem {
            option_id: "opt".to_string(),
            source_key: "k".to_string(),
            label: "L".to_string(),
            i18n: None,
            sort_order: 1,
            is_default: false,
            enabled: true,
            parent_key: None,
            parent_label: None,
            children_count: 0,
            last_push_at: None,
            updated_at: 0,
        };
        projection_contract::assert_keys(&item, &["list_options", "item", "emitted"], "选项行");

        // 轴一：前端在这个端点上发出的排序字段名（`updated_at` + `option_id` 收尾）。
        let spec = crate::addon::feishu::option::table::table_spec()
            .unwrap_or_else(|error| panic!("表声明应有效: {error}"));
        projection_contract::assert_client_fields_are_usable(
            &projection_contract::contract(),
            "list_options",
            &spec,
            "选项",
        );
    }

    /// 空串也算没有父（写入侧 `derive.rs` 对空级返回空串）、空白 trim 后同样算没有；
    /// 重复父键只反查一次。
    #[test]
    fn distinct_parent_keys_trims_blanks_and_dedupes() {
        let item = |parent_key: Option<&str>| OptionItem {
            option_id: "opt".to_string(),
            source_key: "k".to_string(),
            label: "L".to_string(),
            i18n: None,
            sort_order: 1,
            is_default: false,
            enabled: true,
            parent_key: parent_key.map(str::to_string),
            parent_label: None,
            children_count: 0,
            last_push_at: None,
            updated_at: 0,
        };
        let keys: Vec<String> = distinct_parent_keys(&[
            item(Some("a")),
            item(Some("a")),  // 去重
            item(Some("")),   // 空串 = 没有父
            item(Some("  ")), // 空白 = 没有父
            item(None),
            item(Some(" b ")), // trim
        ])
        .into_iter()
        .map(|value| value.as_str().expect("父键应是字符串").to_string())
        .collect();
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&"a".to_string()));
        assert!(keys.contains(&"b".to_string()));
    }
}

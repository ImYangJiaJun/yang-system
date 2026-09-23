//! 选项列表（前端控制台，只读）。

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
        })
    }
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
    let items = rows
        .iter()
        .map(OptionItem::from_record)
        .collect::<Result<Vec<_>, BaseError>>()?;

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
}

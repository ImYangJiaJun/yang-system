//! 选项列表（前端控制台，只读）。

use std::sync::Arc;

use yang_base::action::builtin::OrderByItem;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec};
use yang_base::table::SortOrder;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::list_input::ListInput;

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

    // i18n 落 Text 列存 JSON 文本；这里回原样，前端按文本展示即可
    let items = rows
        .iter()
        .map(|record| {
            Ok(serde_json::json!({
                "option_id": record.require::<String>("option_id")?,
                "source_key": record.require::<String>("source_key")?,
                "label": record.require::<String>("label")?,
                "i18n": record.optional::<String>("i18n")?,
                "sort_order": record.require::<i64>("sort_order")?,
                "is_default": record.optional::<bool>("is_default")?.unwrap_or(false),
                "enabled": record.optional::<bool>("enabled")?.unwrap_or(true),
            }))
        })
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

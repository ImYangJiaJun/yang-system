//! 数据源列表（前端控制台）。

use std::sync::Arc;

use yang_base::action::builtin::OrderByItem;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec};
use yang_base::table::SortOrder;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::list_input::ListInput;

/// 列表项：**刻意不含 `token_hash`**，只在响应里暴露「是否已配置」。
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub(super) struct DatasourceItem {
    /// 数据源标识。
    source_key: String,
    /// 展示名。
    title: String,
    /// 是否启用加密返回。
    encrypt_enabled: bool,
    /// 默认语言。
    default_locale: String,
    /// 状态。
    status: String,
    /// 该行最后一次被写入的时间（unix 秒）。
    ///
    /// 注意它**不是**「选项的最后推送时间」——那个在 `list_options` 的
    /// `updated_at` 上。这里回答的是「这条数据源记录最后一次被改动是什么时候」。
    updated_at: i64,
    /// 取数方式：`push`（多维表格工作流推送）/ `pull`（服务端定时拉取）。
    ingest_mode: String,
    /// 多维表格坐标。`push` 数据源这三项为空。
    bitable_base_token: Option<String>,
    bitable_table_id: Option<String>,
    bitable_view_id: Option<String>,
    /// 取数列的**精确字段名**（接口要名字不要 field_id）。
    bitable_field_name: Option<String>,
    /// 级联映射（JSON 文本）；无级联时为 None。
    linkage_mapping: Option<String>,
    /// 同步状态。控制台靠这四个值判断「这个源还活着吗」。
    ///
    /// `last_success_at` 是唯一诚实的存活信号：`updated_at` 只在整行被写时变，
    /// 而「拉了一轮但内容没变」不会写它。
    last_pull_at: Option<i64>,
    last_success_at: Option<i64>,
    consecutive_failures: i64,
    last_error: Option<String>,
    snapshot_digest: Option<String>,
}

/// 注册数据源列表端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("list_datasources"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/datasources/query")
        .display_name("数据源列表")
        .description("分页查询飞书数据源")
        .permissions(["feishu.datasource.read"])
        .register()
}

pub(super) async fn handle(
    _ctx: ActionContext,
    input: ListInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    // 必须显式列出可读字段：ensure_readable_projection 是框架 crate 私有的，
    // 自定义列表 Action 不会自动拿到「默认可读投影」。token_hash 是 secret 字段，
    // 即便列进来也会被兜住，但显式排除让它更清楚。
    let mut query = context
        .datasources()
        .query()
        .select_fields(&[
            "source_key",
            "title",
            "encrypt_enabled",
            "default_locale",
            "status",
            "updated_at",
            "ingest_mode",
            "bitable_base_token",
            "bitable_table_id",
            "bitable_view_id",
            "bitable_field_name",
            "linkage_mapping",
            "last_pull_at",
            "last_success_at",
            "consecutive_failures",
            "last_error",
            "snapshot_digest",
        ])?
        .search(input.search.as_deref())?;

    if let Some(tree) = input.where_clause {
        query = query.where_tree(tree)?;
    }
    let total = if input.count_total {
        Some(query.clone().count().await?)
    } else {
        None
    };
    // 客户端没给排序时必须兜底：分页要有确定性全序，否则翻页会漏行或重复。
    // 与 list_options 同一处理——前端控制台恒发 source_key 升序，这里兜的是
    // 其它调用方（curl、未来的代码路径），不让它们静默拿到坏分页。
    let mut ordered = false;
    for OrderByItem { field, direction } in input.order_by {
        query = query.order_by(&field, direction)?;
        ordered = true;
    }
    if !ordered {
        query = query.order_by("source_key", SortOrder::Asc)?;
    }
    query = query.page(input.page as usize, input.page_size as usize)?;
    let rows = query.all().await?;

    let items = rows
        .iter()
        .map(|record| {
            Ok(DatasourceItem {
                source_key: record.require("source_key")?,
                title: record.require("title")?,
                encrypt_enabled: record.optional("encrypt_enabled")?.unwrap_or(false),
                default_locale: record.optional("default_locale")?.unwrap_or_default(),
                status: record.require("status")?,
                updated_at: record.require("updated_at")?,
                ingest_mode: record.optional("ingest_mode")?.unwrap_or_default(),
                bitable_base_token: record.optional("bitable_base_token")?,
                bitable_table_id: record.optional("bitable_table_id")?,
                bitable_view_id: record.optional("bitable_view_id")?,
                bitable_field_name: record.optional("bitable_field_name")?,
                linkage_mapping: record.optional("linkage_mapping")?,
                last_pull_at: record.optional("last_pull_at")?,
                last_success_at: record.optional("last_success_at")?,
                consecutive_failures: record.optional("consecutive_failures")?.unwrap_or(0),
                last_error: record.optional("last_error")?,
                snapshot_digest: record.optional("snapshot_digest")?,
            })
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

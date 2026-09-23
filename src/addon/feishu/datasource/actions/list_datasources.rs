//! 数据源列表（前端控制台）。

use std::collections::HashMap;
use std::sync::Arc;

use yang_base::action::builtin::OrderByItem;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec};
use yang_base::table::{Record, SortOrder};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::list_input::ListInput;

/// 一条字段绑定的对外视图。
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub(super) struct FieldBindingItem {
    /// 多维表格字段 ID。**身份就是它**——改名不影响它。
    field_id: String,
    /// 该字段当前的名字（缓存，每轮拉取前按 `field_id` 刷新）。
    field_name: Option<String>,
    /// 进 URL 路径段的数据源标识。
    source_key: String,
    /// 同表内的父列 `field_id`；无父为 `None`。
    parent_field_id: Option<String>,
    /// 是否在用。取消勾选的列会留在这里但被**停用**（不删行）。
    enabled: bool,
    /// 最近一次轮换凭据的时间（unix 秒）；**从未轮换或手填的 Token 为 `None`**。
    ///
    /// 投影它是控制台那一列的全部意义：写入在 `rotate_token.rs`，没有读路径就永远空着。
    /// 手填凭据的绑定从来没有轮换过——那与「轮换过但时间读不出来」是两回事，故用
    /// `Option`（JSON 里是 `null`）而不是 `0`。
    token_rotated_at: Option<i64>,
}

/// 把绑定行按 `datasource_id` 分组。
///
/// 一次 `where_in` 取回本页所有绑定再分组，**避免按行 N+1 查询**：
/// 一页 100 条数据源按行查就是 100 次往返。
pub(super) fn group_bindings(
    rows: &[Record],
) -> Result<HashMap<i64, Vec<FieldBindingItem>>, BaseError> {
    let mut groups: HashMap<i64, Vec<FieldBindingItem>> = HashMap::new();
    for row in rows {
        let datasource_id: i64 = row.require("datasource_id")?;
        groups
            .entry(datasource_id)
            .or_default()
            .push(FieldBindingItem {
                field_id: row.require("field_id")?,
                field_name: row.optional("field_name")?,
                source_key: row.require("source_key")?,
                parent_field_id: row.optional("parent_field_id")?,
                enabled: row.optional("enabled")?.unwrap_or(true),
                token_rotated_at: row.optional("token_rotated_at")?,
            });
    }
    Ok(groups)
}

/// 列表项：**刻意不含任何凭据**（`token_hash` / `token_cipher` 都不投影）。
///
/// 表级行上没有 `source_key`、没有加密开关、没有默认语言——那些都属于
/// **字段绑定**那一层（见 `FieldBindingItem`）。一条表级行只能有一个
/// `source_key`，放在这里语义上自相矛盾。
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub(super) struct DatasourceItem {
    /// 主键。更新 / 删除 / 体检都按它定位。
    id: i64,
    /// 展示名。
    title: String,
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
    /// 表级同步状态。控制台靠这四个值判断「这个源还活着吗」。
    ///
    /// `last_success_at` 是唯一诚实的存活信号：`updated_at` 只在整行被写时变，
    /// 而「拉了一轮但内容没变」不会写它。
    last_pull_at: Option<i64>,
    last_success_at: Option<i64>,
    consecutive_failures: i64,
    last_error: Option<String>,
    /// **已废弃**：摘要归属已改为「每条字段绑定一份」，见 `field_table.rs`。
    snapshot_digest: Option<String>,
    /// 勾选的字段绑定。**空数组表示一条都没配**，不是「没取到」。
    fields: Vec<FieldBindingItem>,
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
            "id",
            "title",
            "status",
            "updated_at",
            "ingest_mode",
            "bitable_base_token",
            "bitable_table_id",
            "bitable_view_id",
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
        // 兜底排序**不能用 source_key**：它已经不在表级行上了（设计 §5）。
        // 先用 title，再补 id——只按 title 时同名行之间没有全序，翻页会漏行或重复。
        query = query.order_by("title", SortOrder::Asc)?;
        query = query.order_by("id", SortOrder::Asc)?;
    }
    query = query.page(input.page as usize, input.page_size as usize)?;
    let rows = query.all().await?;

    // 本页所有绑定一次取回再分组——按行查是 N+1。
    // `where_in` 拒绝空列表，而空页是合法结果，所以这里要短路。
    let ids: Vec<serde_json::Value> = rows
        .iter()
        .map(|record| Ok(serde_json::json!(record.require::<i64>("id")?)))
        .collect::<Result<Vec<_>, BaseError>>()?;
    let mut groups = if ids.is_empty() {
        HashMap::new()
    } else {
        let binding_rows = context
            .datasource_fields()
            .query()
            .select_fields(&[
                "datasource_id",
                "field_id",
                "field_name",
                "source_key",
                "parent_field_id",
                "enabled",
                "token_rotated_at",
            ])?
            .where_in("datasource_id", ids)?
            .all()
            .await?;
        group_bindings(&binding_rows)?
    };

    let mut items = Vec::with_capacity(rows.len());
    for record in rows.iter() {
        let id: i64 = record.require("id")?;
        items.push(DatasourceItem {
            id,
            title: record.require("title")?,
            status: record.require("status")?,
            updated_at: record.require("updated_at")?,
            ingest_mode: record.optional("ingest_mode")?.unwrap_or_default(),
            bitable_base_token: record.optional("bitable_base_token")?,
            bitable_table_id: record.optional("bitable_table_id")?,
            bitable_view_id: record.optional("bitable_view_id")?,
            last_pull_at: record.optional("last_pull_at")?,
            last_success_at: record.optional("last_success_at")?,
            consecutive_failures: record.optional("consecutive_failures")?.unwrap_or(0),
            last_error: record.optional("last_error")?,
            snapshot_digest: record.optional("snapshot_digest")?,
            // 没配绑定的数据源得到**空数组**，不是缺键——前端不必到处补 `?? []`
            fields: groups.remove(&id).unwrap_or_default(),
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

    fn binding_row(
        datasource_id: i64,
        field_id: &str,
        source_key: &str,
        parent: Option<&str>,
        enabled: bool,
    ) -> Record {
        let mut row = Record::new();
        row.insert("datasource_id", serde_json::json!(datasource_id));
        row.insert("field_id", serde_json::json!(field_id));
        row.insert("source_key", serde_json::json!(source_key));
        row.insert("enabled", serde_json::json!(enabled));
        if let Some(parent) = parent {
            row.insert("parent_field_id", serde_json::json!(parent));
        }
        row
    }

    #[test]
    fn bindings_are_grouped_by_datasource() {
        // 一次 where_in 取回本页所有绑定再分组，避免按行 N+1 查询
        let rows = vec![
            binding_row(1, "fldA", "currency", None, true),
            binding_row(1, "fldB", "fx", Some("fldA"), true),
            binding_row(2, "fldC", "summary", None, false),
        ];
        let groups = group_bindings(&rows).unwrap_or_else(|error| panic!("应可分组: {error}"));
        assert_eq!(groups.len(), 2);
        assert_eq!(groups.get(&1).map(Vec::len), Some(2));
        assert_eq!(groups.get(&2).map(Vec::len), Some(1));
    }

    #[test]
    fn the_parent_pointer_and_enabled_flag_survive_the_projection() {
        // 台账要显示「这列挂在谁下面」「还在不在用」，两个都不丢
        let rows = vec![binding_row(1, "fldB", "fx", Some("fldA"), false)];
        let groups = group_bindings(&rows).unwrap_or_else(|error| panic!("应可分组: {error}"));
        let item = &groups
            .get(&1)
            .map(|v| v[0].clone())
            .unwrap_or_else(|| panic!("应有分组"));
        assert_eq!(item.parent_field_id.as_deref(), Some("fldA"));
        assert!(!item.enabled);
        assert_eq!(item.field_id, "fldB");
        assert_eq!(item.source_key, "fx");
    }

    #[test]
    fn the_rotation_time_is_projected_so_the_console_column_is_not_always_blank() {
        // 写入在 `rotate_token.rs`（三列同事务），但此前**没有任何读路径投影它**，
        // 所以控制台那列永远显示「—」。绑定项必须带上 `token_rotated_at`。
        let mut row = binding_row(1, "fldA", "currency", None, true);
        row.insert("token_rotated_at", serde_json::json!(1_700_000_000_i64));
        let groups = group_bindings(&[row]).unwrap_or_else(|error| panic!("应可分组: {error}"));
        let item = groups
            .get(&1)
            .and_then(|items| items.first())
            .unwrap_or_else(|| panic!("应有分组"));
        let value =
            serde_json::to_value(item).unwrap_or_else(|error| panic!("绑定项应可序列化: {error}"));
        assert_eq!(
            value.get("token_rotated_at"),
            Some(&serde_json::json!(1_700_000_000_i64)),
            "轮换时间必须出现在绑定投影里，键名是 token_rotated_at（snake_case）"
        );
    }

    #[test]
    fn a_binding_that_was_never_rotated_projects_null_not_a_missing_key() {
        // 手填的 Token 从来没有轮换过。这里必须是 `null`（键在、值为空），
        // 而不是缺键——前端契约是 `token_rotated_at: number | null`。
        let rows = vec![binding_row(1, "fldA", "currency", None, true)];
        let groups = group_bindings(&rows).unwrap_or_else(|error| panic!("应可分组: {error}"));
        let item = groups
            .get(&1)
            .and_then(|items| items.first())
            .unwrap_or_else(|| panic!("应有分组"));
        let value =
            serde_json::to_value(item).unwrap_or_else(|error| panic!("绑定项应可序列化: {error}"));
        assert_eq!(
            value.get("token_rotated_at"),
            Some(&serde_json::Value::Null)
        );
    }

    #[test]
    fn a_field_without_a_parent_has_none() {
        let rows = vec![binding_row(1, "fldA", "currency", None, true)];
        let groups = group_bindings(&rows).unwrap_or_else(|error| panic!("应可分组: {error}"));
        assert_eq!(
            groups.get(&1).map(|v| v[0].parent_field_id.clone()),
            Some(None)
        );
    }

    #[test]
    fn an_empty_binding_set_yields_no_groups() {
        // 没有绑定的数据源在投影时走 `unwrap_or_default()` 得到空 vec——
        // 绝不是「键不存在」让前端到处补 `?? []`
        assert!(group_bindings(&[])
            .unwrap_or_else(|error| panic!("应可分组: {error}"))
            .is_empty());
    }
}

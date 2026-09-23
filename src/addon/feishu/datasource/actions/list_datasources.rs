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

/// 表级行的**投影列**。与 `frontend/contracts/feishu-projections.json` 是同一份契约。
///
/// 提成常量不是为了好看：`select_fields` 与契约断言**必须读同一个来源**，否则
/// 「查询选的列」与「契约声明的键」会各自漂移——那正是这一整类 bug 的形态。
/// `fields` 不在其中：它没有对应的列，由 `group_bindings` 组装（见 `DATASOURCE_ITEM_COMPOSED`）。
pub(super) const DATASOURCE_ITEM_COLUMNS: &[&str] = &[
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
    // `snapshot_digest` **不再投影**：摘要归属在表级化时已搬到字段绑定，
    // 表级那一列是「保留列」，谁都不写它（`pull.rs` 只把摘要写进绑定行）。
    // 继续投影它 = 前端永远读到 null，却把它文档成「内容摘要」——一句假话。
];

/// 字段绑定的**投影列**。同上，与契约文件是同一份契约。
pub(super) const BINDING_ITEM_COLUMNS: &[&str] = &[
    "datasource_id",
    "field_id",
    "field_name",
    "source_key",
    "parent_field_id",
    "enabled",
    "encrypt_enabled",
    "default_locale",
    "token_rotated_at",
];

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
    /// 加密返回（回给飞书的信封是否加密）。**它是绑定级的**——一条数据源有 N 条绑定，
    /// 每条可以各自决定。表级行上曾经也有这个名字的列，那是字段级时代的残留：
    /// 表级行读它只会恒得 `false`，于是台账那一列对每一条源都画「未加密」。
    encrypt_enabled: bool,
    /// 回给飞书的 `locale`。同样属于绑定层，取值域 `zh_cn` / `en_us` / `ja_jp`，
    /// **后端对它零校验**——界面是唯一的守卫。
    default_locale: String,
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
                // 两列在库上都是 `NOT NULL` + 有默认值，兜底值与列默认值一致：
                // 缺键只可能意味着「投影少列了」，那种时候静默降级成默认值会
                // 把一个缺陷画成「未加密 / 简体中文」——所以兜底值必须**等于**
                // 列默认值，这样万一真漏了，画面与库里的实际值仍然一致。
                encrypt_enabled: row.optional("encrypt_enabled")?.unwrap_or(false),
                default_locale: row
                    .optional("default_locale")?
                    .unwrap_or_else(|| "zh_cn".to_string()),
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
        .select_fields(DATASOURCE_ITEM_COLUMNS)?
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
            // 这两个键**属于绑定层**，而表级行上曾经各有一个同名的残影
            // （读它恒为默认值，于是台账那两列对每一条源都在说假话）。
            // 控制台要在一条数据源里逐字段展示它们，所以必须从这里投影出去。
            .select_fields(BINDING_ITEM_COLUMNS)?
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

    use crate::addon::feishu::domain::projection_contract;

    /// 控制台与后端兜底都按 `id` 收尾排序，而 DSL 的 `sortable` 是 fail-closed。
    ///
    /// 这条不是「测一个布尔位」：`TableQuery::order_by` 对不可排序的字段直接返回
    /// `FieldPermissionDenied`，HTTP 边界把它映射成 **403**（`transport/axum.rs`），
    /// 于是 `/datasources/query` 每次请求都失败——列表页根本打不开，比「排序不生效」
    /// 重得多。前端 `withStableOrder` 恒发 `[{title,Asc},{id,Asc}]`，后端兜底
    /// （本文件 `!ordered` 分支）也是这两条，所以两条都必须真的可用。
    #[tokio::test]
    async fn the_order_clauses_the_console_always_sends_are_applicable() {
        let pool = Arc::new(
            sqlx::MySqlPool::connect_lazy("mysql://user:pass@localhost:3306/yang")
                .unwrap_or_else(|error| panic!("惰性连接池应可构造: {error}")),
        );
        let definition = crate::addon::feishu::datasource::table::table_spec()
            .unwrap_or_else(|error| panic!("表声明应有效: {error}"))
            .table_definition()
            .unwrap_or_else(|error| panic!("应可编译为表定义: {error}"));
        // 与 `Repository::query()` 同一条绑定路径：受信角色 + 惰性连接池。
        // `order_by` 只做校验，不碰数据库。
        let mut query = definition.bind(pool).query(["system"]);
        for field in ["title", "id"] {
            query = query
                .order_by(field, SortOrder::Asc)
                .unwrap_or_else(|error| {
                    panic!(
                        "按 {field} 排序被拒：{error}——不可排序的字段得到 \
                     FieldPermissionDenied，在 HTTP 边界上就是 403，\
                     整个 /datasources/query 恒失败"
                    )
                });
        }
    }

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

    /// 表级行上**没有对应列**的合成键：`fields` 由 `group_bindings` 组装。
    ///
    /// 只在这里用到（生产路径直接组装它，不需要一份清单），所以放测试模块里，
    /// 免得给非测试构建留一个 dead_code。
    const DATASOURCE_ITEM_COMPOSED: &[&str] = &["fields"];

    /// 表级行 / 字段绑定 / 查询列，三处与契约文件逐一对账。
    ///
    /// 挡的是这一整类 bug 的病根——「字段没被删，只是搬到了另一层」：那种漂移对
    /// 「扫已删字段」类检查全盲（`encrypt_enabled` 在绑定表上依然是合法字段名）。
    /// 断言工具在 `domain/projection_contract.rs`，与前端那份测试共用同一份契约文件。
    #[test]
    fn the_committed_contract_matches_the_structs_and_the_query() {
        let contract = projection_contract::contract();

        let item = DatasourceItem {
            id: 1,
            title: "t".to_string(),
            status: "active".to_string(),
            updated_at: 0,
            ingest_mode: "pull".to_string(),
            bitable_base_token: None,
            bitable_table_id: None,
            bitable_view_id: None,
            last_pull_at: None,
            last_success_at: None,
            consecutive_failures: 0,
            last_error: None,
            fields: Vec::new(),
        };
        projection_contract::assert_keys(&item, &["list_datasources", "item", "emitted"], "表级行");

        let binding = FieldBindingItem {
            field_id: "fldA".to_string(),
            field_name: None,
            source_key: "k".to_string(),
            parent_field_id: None,
            enabled: true,
            encrypt_enabled: false,
            default_locale: "zh_cn".to_string(),
            token_rotated_at: None,
        };
        projection_contract::assert_keys(
            &binding,
            &["list_datasources", "binding", "emitted"],
            "字段绑定",
        );

        for level in ["item", "binding"] {
            projection_contract::assert_buckets_are_disjoint(&contract, "list_datasources", level);
        }

        // 轴一：前端在这个端点上**发出**的排序/筛选字段名，必须能在这张表上用。
        // 它们曾经漂移过一次（收尾键从 source_key 换成 id，而 id 当时没开 sortable），
        // 后果是整个列表请求被排序校验打成 400。
        let spec = crate::addon::feishu::datasource::table::table_spec()
            .unwrap_or_else(|error| panic!("表声明应有效: {error}"));
        projection_contract::assert_client_fields_are_usable(
            &contract,
            "list_datasources",
            &spec,
            "表级行",
        );
    }

    /// 查询选的列必须恰好等于 `emitted ∪ query_only`——多一列少一列都说明
    /// 「查询」与「契约」两处各自漂移了。
    #[test]
    fn the_query_columns_match_the_contract_buckets() {
        let contract = projection_contract::contract();

        let mut item_expected: Vec<String> = DATASOURCE_ITEM_COLUMNS
            .iter()
            .map(|key| (*key).to_string())
            .collect();
        item_expected.sort_unstable();
        let mut item_declared =
            projection_contract::string_array(&contract, &["list_datasources", "item", "emitted"]);
        item_declared.retain(|key| !DATASOURCE_ITEM_COMPOSED.contains(&key.as_str()));
        item_declared.extend(projection_contract::bucket_keys(
            &contract,
            &["list_datasources", "item", "query_only"],
        ));
        item_declared.sort_unstable();
        assert_eq!(item_expected, item_declared, "表级行的查询列与契约不一致");

        let mut binding_expected: Vec<String> = BINDING_ITEM_COLUMNS
            .iter()
            .map(|key| (*key).to_string())
            .collect();
        binding_expected.sort_unstable();
        let mut binding_declared = projection_contract::string_array(
            &contract,
            &["list_datasources", "binding", "emitted"],
        );
        binding_declared.extend(projection_contract::bucket_keys(
            &contract,
            &["list_datasources", "binding", "query_only"],
        ));
        binding_declared.sort_unstable();
        assert_eq!(
            binding_expected, binding_declared,
            "字段绑定的查询列与契约不一致（`datasource_id` 属于 query_only：它只用来分组）"
        );
    }

    #[test]
    fn the_per_field_settings_are_projected_from_the_binding_layer() {
        // 这两个键**属于绑定层**：表级行上曾经有同名残影，表级行读它恒为默认值，
        // 于是台账那两列对每一条源都在说假话。控制台要在一条数据源里逐字段展示它们，
        // 所以它们必须从这里投影出去。
        let mut row = binding_row(1, "fldA", "currency", None, true);
        row.insert("encrypt_enabled", serde_json::json!(true));
        row.insert("default_locale", serde_json::json!("en_us"));
        let groups = group_bindings(&[row]).unwrap_or_else(|error| panic!("应可分组: {error}"));
        let item = groups
            .get(&1)
            .and_then(|items| items.first())
            .unwrap_or_else(|| panic!("应有分组"));
        let value =
            serde_json::to_value(item).unwrap_or_else(|error| panic!("绑定项应可序列化: {error}"));
        assert_eq!(value.get("encrypt_enabled"), Some(&serde_json::json!(true)));
        assert_eq!(
            value.get("default_locale"),
            Some(&serde_json::json!("en_us"))
        );
    }

    #[test]
    fn a_binding_missing_those_keys_degrades_to_the_column_defaults() {
        // 兜底值必须**等于列默认值**（`false` / `zh_cn`）：否则投影一旦漏列，
        // 画面上的「未加密 / 简体中文」就与库里的实际值分叉，而那种分叉看不出来。
        let rows = vec![binding_row(1, "fldA", "currency", None, true)];
        let groups = group_bindings(&rows).unwrap_or_else(|error| panic!("应可分组: {error}"));
        let item = groups
            .get(&1)
            .and_then(|items| items.first())
            .unwrap_or_else(|| panic!("应有分组"));
        assert!(!item.encrypt_enabled);
        assert_eq!(item.default_locale, "zh_cn");
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

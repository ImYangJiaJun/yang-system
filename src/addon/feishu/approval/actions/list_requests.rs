//! 派发请求记录列表（前端控制台「派发记录」页）。
//!
//! `request_body` / `response_body` 随行返回：单次派发请求的体量小，前端本地展开
//! 比二次加载更省事。
//!
//! # 读侧脱敏
//!
//! 新写入的 `request_body` 已在 `dispatch::redacted_request_body` 里把 `token`
//! 换成 `<redacted>`；但 2026-10 之前的历史行仍是明文 Token。本端点只读，不能回滚
//! 这些行，也不能把「只读权限」升级成「派发权限」——所以回显前必须再脱敏一次。
//! 解析失败按 fail-closed 返回固定占位文案，绝不原样吐出可能含凭据的残缺报文。
//! 历史行的库内清洗与 Token 轮换仍按暂缓项 0.1 处理，本端点不做任何 DB 写。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Serialize;
use yang_base::action::builtin::OrderByItem;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec};
use yang_base::table::SortOrder;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::list_input::ListInput;

/// 请求记录行投影列（全列，体量小直接展开）。
pub(super) const REQUEST_ITEM_COLUMNS: &[&str] = &[
    "id",
    "requested_by",
    "base_token",
    "table_id",
    "config_id",
    "record_id",
    "request_body",
    "outcome",
    "message",
    "serial_number",
    "response_body",
    "created_at",
];

/// 一条派发请求记录。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct RequestItem {
    /// 主键。
    id: i64,
    /// 请求人（未带时落 `feishu-workflow`）。
    requested_by: Option<String>,
    /// 多维表格 token。
    base_token: String,
    /// 数据表 id。
    table_id: String,
    /// 配置存在或建成后关联；首调失败时为空。
    config_id: Option<i64>,
    /// 单条处理时的记录 id；全表受理为空。
    record_id: Option<String>,
    /// 请求体原文（`DispatchInput` 序列化）。
    request_body: String,
    /// 结果四桶：succeeded / waiting / accepted / failed。
    outcome: String,
    /// 结果说明（失败时为可行动原因）。
    message: String,
    /// 单条成功时的审批单编号。
    serial_number: Option<String>,
    /// 返回信封 `data` 原文；失败出口为空。
    response_body: Option<String>,
    /// 请求时间（unix 秒）。
    created_at: i64,
}

/// 注册派发记录列表端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("list_requests"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/approval/requests/query")
        .display_name("派发请求记录列表")
        .description("分页查询飞书审批派发请求记录")
        .permissions(["feishu.approval.read"])
        .register()
}

/// 无法解析时返回的固定文案（fail-closed：不原样回显）。
const UNPARSEABLE_REQUEST_BODY: &str = "[unparseable request body]";

/// 回显前的读侧脱敏：只替换顶层 `token`，其余字段原样保留。
///
/// 历史行可能仍是明文；新行已经是 `<redacted>`（替换是幂等的）。非对象或
/// 解析失败一律返回固定占位文案——残缺报文里同样可能有凭据片段。
pub(super) fn redact_request_body(raw: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return UNPARSEABLE_REQUEST_BODY.to_string();
    };
    let Some(object) = value.as_object_mut() else {
        return UNPARSEABLE_REQUEST_BODY.to_string();
    };
    if object.contains_key("token") {
        object.insert(
            "token".to_string(),
            serde_json::Value::String("<redacted>".to_string()),
        );
    }
    serde_json::to_string(&value).unwrap_or_else(|_| UNPARSEABLE_REQUEST_BODY.to_string())
}

pub(super) async fn handle(
    _ctx: ActionContext,
    input: ListInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    // 本表没有声明 `searchable` 字段：`TableQuery::search` 对零可搜索字段的表直接
    // 返回 PermissionDenied（filters.rs），所以这里刻意不调用它——通用 TableView
    // 只在目录声明了 search_fields 时才会发 search（本表为空），偶然带上的 search
    // 忽略比 4xx 更符合契约兼容。
    let mut query = context
        .approval_request_logs()
        .query()
        .select_fields(REQUEST_ITEM_COLUMNS)?;

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
        query = query.order_by("created_at", SortOrder::Desc)?;
        query = query.order_by("id", SortOrder::Asc)?;
    }
    query = query.page(input.page as usize, input.page_size as usize)?;
    let rows = query.all().await?;

    let items: Vec<RequestItem> = rows
        .iter()
        .map(|record| {
            Ok(RequestItem {
                id: record.require("id")?,
                requested_by: record.optional("requested_by")?,
                base_token: record.require("base_token")?,
                table_id: record.require("table_id")?,
                config_id: record.optional("config_id")?,
                record_id: record.optional("record_id")?,
                request_body: redact_request_body(&record.require::<String>("request_body")?),
                outcome: record.require("outcome")?,
                message: record.require("message")?,
                serial_number: record.optional("serial_number")?,
                response_body: record.optional("response_body")?,
                created_at: record.require("created_at")?,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 历史行的明文 Token 必须被替换，其余字段原样保留。
    #[test]
    fn legacy_plaintext_token_is_redacted_on_read() {
        let redacted = redact_request_body(
            r#"{"token":"real-dispatch-token","record_id":"rec001","applicant":"张三"}"#,
        );
        let value: serde_json::Value =
            serde_json::from_str(&redacted).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(value["token"], "<redacted>");
        assert_eq!(value["record_id"], "rec001");
        assert_eq!(value["applicant"], "张三");
        assert!(
            !redacted.contains("real-dispatch-token"),
            "明文 Token 不得回显: {redacted}"
        );
    }

    /// 新行已经是占位符：脱敏必须幂等。
    #[test]
    fn already_redacted_body_is_unchanged() {
        let redacted = redact_request_body(r#"{"token":"<redacted>","record_id":"rec001"}"#);
        let value: serde_json::Value =
            serde_json::from_str(&redacted).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(value["token"], "<redacted>");
        assert_eq!(value["record_id"], "rec001");
    }

    /// 解析失败 fail-closed：固定占位文案，绝不原样返回残缺报文。
    #[test]
    fn malformed_body_is_not_echoed() {
        for raw in [
            r#"{"token":"real-dispatch-token""#,
            r#"token=real-dispatch-token"#,
            "[]",
            "null",
        ] {
            let redacted = redact_request_body(raw);
            assert_eq!(redacted, UNPARSEABLE_REQUEST_BODY, "输入: {raw}");
            assert!(!redacted.contains("real-dispatch-token"));
        }
    }

    /// 投影列清单与列表项序列化键逐一对账：多一列少一列都说明漂移。
    #[test]
    fn the_item_keys_match_the_projection() {
        let item = RequestItem {
            id: 1,
            requested_by: Some("张三".to_string()),
            base_token: "appTest".to_string(),
            table_id: "tblTest".to_string(),
            config_id: Some(7),
            record_id: Some("rec001".to_string()),
            request_body: r#"{"base_token":"appTest"}"#.to_string(),
            outcome: "succeeded".to_string(),
            message: "已创建审批实例".to_string(),
            serial_number: Some("202609280001".to_string()),
            response_body: Some(r#"{"accepted":true}"#.to_string()),
            created_at: 1_700_000_000,
        };
        let value =
            serde_json::to_value(&item).unwrap_or_else(|error| panic!("应可序列化: {error}"));
        let keys = value
            .as_object()
            .map(|object| {
                let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
                keys.sort_unstable();
                keys
            })
            .unwrap_or_default();
        let mut expected: Vec<&str> = REQUEST_ITEM_COLUMNS.to_vec();
        expected.sort_unstable();
        assert_eq!(keys, expected, "列表项键必须恰好等于投影列");
    }

    /// 可空列缺省时序列化为 null（键在、值为空），前端契约是 `string | null`。
    #[test]
    fn absent_optional_columns_serialize_as_null() {
        let item = RequestItem {
            id: 2,
            requested_by: None,
            base_token: "appTest".to_string(),
            table_id: "tblTest".to_string(),
            config_id: None,
            record_id: None,
            request_body: r#"{}"#.to_string(),
            outcome: "failed".to_string(),
            message: "参数无效".to_string(),
            serial_number: None,
            response_body: None,
            created_at: 1_700_000_000,
        };
        let value =
            serde_json::to_value(&item).unwrap_or_else(|error| panic!("应可序列化: {error}"));
        for key in [
            "requested_by",
            "config_id",
            "record_id",
            "serial_number",
            "response_body",
        ] {
            assert_eq!(value.get(key), Some(&serde_json::Value::Null), "键 {key}");
        }
    }
}

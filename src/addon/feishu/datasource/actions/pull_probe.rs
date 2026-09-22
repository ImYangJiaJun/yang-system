//! 出站拉取探针：用自建应用凭证真实调一次飞书多维表格，把结果原样报回来。
//!
//! # 为什么需要它
//!
//! 出站能力（凭证换取、缓存、短锁、记录拉取）此前是**没有被任何生产代码引用**的一层
//! 积木：`TenantTokenProvider` 没有任何构造点，`list_all_records` 没有任何调用点。
//! 换句话说「凭证注入」这件事在单元测试里成立，却从未在真实租户上跑过一次。
//!
//! 本探针把那一层接上，并按官方文档的要求注入凭证：
//! `Authorization: Bearer <tenant_access_token>`（《列出记录》《列出字段》的请求头
//! 都标为必填，且值格式就是 `Bearer <access_token>`）。
//!
//! # 它回答什么
//!
//! 一次调用即可判定下列全部问题，不必等 worker 与写路径落地：
//!
//! - 凭证**换得到**吗（`app_id`/`app_secret` 对不对、应用是否停用）；
//! - 换到的 token **能读这张表**吗（飞书侧有没有给应用「添加文档应用」——
//!   没有会得到 `1254302`，而不是 HTTP 403）；
//! - `field_names` 要**精确字段名**这个结论对不对（传 `field_id` 会吃 `1254024`，
//!   所以本探针走「列出字段 → 用 field_id 解析出准确名字 → 再喂给列出记录」）；
//! - 取数列的**类型**是什么（官方《列出字段》给的 `type`/`ui_type` 是权威依据，
//!   比从单元格值反推可靠）；
//! - 单元格值**真实的 JSON 形态**——这是本探针最有价值的一项：官方文档说单选是
//!   裸字符串，而本仓库用 lark-cli 抓的落盘数据是长度 1 的数组。两个方向都有证据，
//!   探针把观测到的原始形态（`string` / `array` / …）直接报出来，当场定论。
//!
//! # 安全边界
//!
//! **只读**：只调「列出字段」与「列出记录」，不写任何东西、不落库、不改数据源状态。
//! 只取**一页**（默认 200 行），不按 `MAX_PAGES` 翻页——它是诊断，不是同步。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::bitable::{
    cell_label, check_coordinate_field, first_records_page, list_all_fields, resolve_field_name,
    BitableCoordinates, CellValue,
};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound::{HttpClientTransport, OutboundFailure, TokioSleeper};
use crate::addon::feishu::domain::tenant_token::{
    FeishuCredentials, RedisTenantTokenCache, TenantTokenProvider,
};

/// 单次探测最多取多少行。诊断足够，且刻意远小于 `PAGE_SIZE`(500)，避免大表首屏过重。
const PROBE_PAGE_SIZE: u32 = 200;

/// 回报的样本条数上限。再多只是把响应撑大，不影响判定。
const SAMPLE_LIMIT: usize = 10;

/// 探针输入。
///
/// 坐标为**请求体直传**而不是从数据源行读取：数据源表的坐标列（`bitable_base_token` /
/// `bitable_table_id` / `bitable_field_id`）属 T2-2，尚未落地；而探针的价值恰恰在于
/// **先于那些列**证明凭证链路可用。等坐标列落地后，这里可以再加一个 `source_key`
/// 分支走库。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct PullProbeInput {
    /// 多维表格 app_token（URL 里 `feishu.cn/base/<这里>` 那段）。
    pub(super) base_token: String,
    /// 数据表 table_id。
    pub(super) table_id: String,
    /// 取数列的 field_id。给了它就顺带验证「field_id → 精确字段名」的解析路径。
    #[serde(default)]
    pub(super) field_id: Option<String>,
    /// 取数列的**精确字段名**。与 `field_id` 二选一；都给了以 `field_id` 为准。
    #[serde(default)]
    pub(super) field_name: Option<String>,
    /// 视图 ID；省略则取全表。
    #[serde(default)]
    pub(super) view_id: Option<String>,
}

impl ParamInput for PullProbeInput {
    fn params() -> Params {
        Params::new()
    }
}

impl PullProbeInput {
    fn validate(&self) -> Result<(), BaseError> {
        for (name, value) in [
            ("base_token", &self.base_token),
            ("table_id", &self.table_id),
        ] {
            if value.trim().is_empty() {
                return Err(BaseError::ParamInvalid(
                    name.to_string(),
                    "不能为空".to_string(),
                ));
            }
        }
        if self.field_id.is_none() && self.field_name.is_none() {
            return Err(BaseError::ParamInvalid(
                "field_id/field_name".to_string(),
                "至少要给一个取数列（field_id 或 field_name）".to_string(),
            ));
        }
        Ok(())
    }
}

/// 探针结果。字段名刻意直白——它是给人看的诊断输出，不是稳定契约。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct PullProbeResult {
    /// 实际用于 `field_names` 的精确字段名。
    field_name: String,
    /// 字段类型（`ui_type` 与 `type` 码）。
    field_type: String,
    /// 该表返回的字段总数（验证「列出字段」分页是否拉全）。
    field_count: usize,
    /// 首屏 `total`；`None` 表示飞书没给。
    record_total: Option<i64>,
    /// 本页实际取到的行数。
    scanned: usize,
    /// 是否还有下一页（探针只取一页，故此处为 true 属正常）。
    has_more: bool,
    /// 取到的选项文案样本。
    sample_labels: Vec<String>,
    /// 这些样本**原始 JSON 的形态**（`string` / `array` / `object` / …）。
    ///
    /// 这是本探针最想拿到的一项：它能一次性判定「单选到底回裸字符串还是数组」，
    /// 而这个结论决定 `cell_label` 里那条兼容分支会不会被真实流量走到。
    sample_raw_kinds: Vec<String>,
    /// 该列在本页为空的行数。
    empty: usize,
    /// 该列在本页**取值不支持的形态**（多值、人员、地理位置等）的行数。
    unsupported: usize,
}

/// 注册出站拉取探针。
///
/// **只在 `can_pull()` 为真时注册**：它要出站调飞书并消耗频控配额，凭证没配好时
/// 注册出来只会让人以为可用（与 `option` module 的机器入口同一取舍）。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("pull_probe"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&context))
        })
        .route(HttpMethod::Post, "/api/v1/feishu/datasources/pull-probe")
        .display_name("出站拉取探针")
        .description("用自建应用凭证真实调用飞书多维表格，回报字段类型与取值形态")
        // 用 write 而不是 read：它会出站调飞书、消耗本应用的频控配额，
        // 属于运维动作，不该和纯读的控制台查询共用同一权限。
        .permissions(["feishu.datasource.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: PullProbeInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    // 1) 凭证与出站设施。三样都来自框架资源，本函数不自己建连接池。
    let settings = context
        .settings()
        .ok_or_else(|| BaseError::ConfigError("未配置 [feishu] 段，无法出站拉取".to_string()))?;
    if !settings.can_pull() {
        return Ok(ApiResponse::fail(
            40902,
            "出站凭证未配置或仍是占位值（需要非占位的 feishu.app_id / feishu.app_secret）",
        ));
    }
    let app_id = settings.app_id.clone().unwrap_or_default();
    let app_secret = settings.app_secret.clone().unwrap_or_default();
    // 部署命名空间复用授权缓存那一份，而不是在 [feishu] 段再配一个：
    // 同一个部署在缓存层必须是同一个键空间，两份配置迟早会漂移。
    let deployment = ctx
        .tools()
        .extension::<crate::authorization::AuthorizationVersionCache>()?
        .deployment()
        .to_string();

    let cache = Arc::new(RedisTenantTokenCache::new(
        ctx.tools().cache()?.clone(),
        &deployment,
    ));
    let transport = Arc::new(HttpClientTransport::new(ctx.tools().http()?.clone()));
    let sleeper = Arc::new(TokioSleeper);
    let tokens = TenantTokenProvider::new(
        cache,
        transport.clone(),
        sleeper.clone(),
        FeishuCredentials { app_id, app_secret },
        &deployment,
    )
    .map_err(|error| BaseError::ConfigError(error.to_string()))?;

    let coordinates = BitableCoordinates {
        app_token: input.base_token.trim().to_string(),
        table_id: input.table_id.trim().to_string(),
        view_id: input
            .view_id
            .as_deref()
            .map(|value| value.trim().to_string()),
    };

    // 2) 解析取数列：field_id -> 精确字段名，并校验它是单值字段。
    let fields = list_all_fields(transport.as_ref(), sleeper.as_ref(), &tokens, &coordinates)
        .await
        .map_err(outbound_error)?;

    let (field_name, field) = match input.field_id.as_deref() {
        Some(field_id) => {
            let name = resolve_field_name(&fields, field_id).map_err(|error| {
                BaseError::ParamInvalid("field_id".to_string(), error.to_string())
            })?;
            let field = fields
                .iter()
                .find(|candidate| candidate.field_id == field_id)
                .cloned();
            (name, field)
        }
        None => {
            let name = input.field_name.clone().unwrap_or_default();
            let field = fields
                .iter()
                .find(|candidate| candidate.field_name.trim() == name.trim())
                .cloned();
            (name, field)
        }
    };

    let field_type = match field.as_ref() {
        Some(field) => check_coordinate_field(field).map_err(|error| {
            BaseError::ParamInvalid("field_name".to_string(), error.to_string())
        })?,
        // 元数据里找不到同名列时不拦：可能是权限只放开了部分字段，
        // 让下面的真实拉取去暴露问题，比在这里猜更准确。
        None => "未知（列出字段里没有同名项）".to_string(),
    };

    // 3) 真实拉取一页，凭证在这一步注入（Authorization: Bearer <tenant_access_token>）。
    let page = first_records_page(
        transport.as_ref(),
        sleeper.as_ref(),
        &tokens,
        &coordinates,
        std::slice::from_ref(&field_name),
        PROBE_PAGE_SIZE,
    )
    .await
    .map_err(outbound_error)?;

    // 4) 逐行取值，同时统计形态——形态统计就是「单选是字符串还是数组」的答案。
    let mut sample_labels = Vec::new();
    let mut sample_raw_kinds = Vec::new();
    let mut empty = 0usize;
    let mut unsupported = 0usize;
    for record in &page.items {
        let Some(value) = record.fields.get(&field_name) else {
            empty += 1;
            continue;
        };
        match cell_label(value) {
            CellValue::Text(label) => {
                if sample_labels.len() < SAMPLE_LIMIT {
                    sample_labels.push(label);
                    sample_raw_kinds.push(json_kind(value).to_string());
                }
            }
            CellValue::Empty => empty += 1,
            CellValue::Unsupported => unsupported += 1,
        }
    }

    ApiResponse::success(
        PullProbeResult {
            field_name,
            field_type,
            field_count: fields.len(),
            record_total: page.total,
            scanned: page.items.len(),
            has_more: page.has_more,
            sample_labels,
            sample_raw_kinds,
            empty,
            unsupported,
        },
        "探测完成",
    )
}

/// 把出站失败转成可归因的业务错误。
///
/// 用 `ApiResponse::fail` 的码值域而不是 HTTP 5xx：这一层的失败几乎都是**配置或权限**
/// 问题（列名不对、没给应用加文档权限、凭证写错），返回可读文案比返回一个 500
/// 更能让运维直接定位。
fn outbound_error(failure: OutboundFailure) -> BaseError {
    BaseError::ParamInvalid("feishu".to_string(), failure.to_string())
}

/// 单元格值的 JSON 形态名，用于回报「原始数据长什么样」。
fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(field_id: Option<&str>, field_name: Option<&str>) -> PullProbeInput {
        PullProbeInput {
            base_token: "bascnCMII2ORej2RItqpZZUNMIe".to_string(),
            table_id: "tblxI2tWaxP5dG7p".to_string(),
            field_id: field_id.map(str::to_string),
            field_name: field_name.map(str::to_string),
            view_id: None,
        }
    }

    #[test]
    fn requires_both_coordinates() {
        let mut payload = input(Some("fldA"), None);
        payload.base_token = "  ".to_string();
        assert!(payload.validate().is_err(), "空 base_token 应被拒绝");

        let mut payload = input(Some("fldA"), None);
        payload.table_id = String::new();
        assert!(payload.validate().is_err(), "空 table_id 应被拒绝");
    }

    #[test]
    fn requires_at_least_one_field_selector() {
        assert!(
            input(None, None).validate().is_err(),
            "既不给 field_id 也不给 field_name 就无法取数"
        );
        assert!(input(Some("fldA"), None).validate().is_ok());
        assert!(input(None, Some("名称")).validate().is_ok());
        // 两个都给是允许的：以 field_id 为准，顺带验证解析路径
        assert!(input(Some("fldA"), Some("名称")).validate().is_ok());
    }

    #[test]
    fn json_kind_names_the_observed_shape() {
        assert_eq!(json_kind(&serde_json::json!("A")), "string");
        assert_eq!(json_kind(&serde_json::json!(["A"])), "array");
        assert_eq!(json_kind(&serde_json::json!(3)), "number");
        assert_eq!(json_kind(&serde_json::json!(null)), "null");
        assert_eq!(json_kind(&serde_json::json!({"text": "A"})), "object");
    }
}

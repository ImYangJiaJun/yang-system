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
use crate::addon::feishu::domain::outbound_setup;

/// 单次探测最多取多少行。诊断足够，且刻意远小于 `PAGE_SIZE`(500)，避免大表首屏过重。
const PROBE_PAGE_SIZE: u32 = 200;

/// 回报的样本条数上限。再多只是把响应撑大，不影响判定。
const SAMPLE_LIMIT: usize = 10;

/// 探针输入。**两种模式，二选一**。
///
/// - 给了 `datasource_id`：坐标取自那条表级数据源行，取数列默认取它**第一条启用中的
///   绑定**（`field_id → 精确字段名` 的解析正是表级拉取每轮做的那一步）。这是配置
///   落库之后的排查路径：拉取失败时想知道「到底是凭证、权限、还是这一列」。
/// - 只给坐标：**请求体直传**，不走库。保留它是为了探针最初、也是最不可替代的用途——
///   **先于任何数据源**证明凭证链路可用（那时候库里一行都还没有）。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct PullProbeInput {
    /// 表级数据源 `id`。给了它就不必再给坐标与取数列。
    #[serde(default)]
    pub(super) datasource_id: Option<i64>,
    /// 多维表格 app_token（URL 里 `feishu.cn/base/<这里>` 那段）。直传模式下必填。
    #[serde(default)]
    pub(super) base_token: Option<String>,
    /// 数据表 table_id。直传模式下必填。
    #[serde(default)]
    pub(super) table_id: Option<String>,
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
        if self.datasource_id.is_some_and(|id| id <= 0) {
            return Err(BaseError::ParamInvalid(
                "datasource_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        // 走库模式只需要 `datasource_id`：坐标与取数列都从那条源上读。
        if self.datasource_id.is_some() {
            return Ok(());
        }
        for (name, value) in [
            ("base_token", self.base_token.as_deref()),
            ("table_id", self.table_id.as_deref()),
        ] {
            if !value.is_some_and(|text| !text.trim().is_empty()) {
                return Err(BaseError::ParamInvalid(
                    name.to_string(),
                    "直传坐标时必须给（或改用 datasource_id 走库）".to_string(),
                ));
            }
        }
        if self.field_id.is_none() && self.field_name.is_none() {
            return Err(BaseError::ParamInvalid(
                "field_id/field_name".to_string(),
                "直传坐标时至少要给一个取数列（field_id 或 field_name）".to_string(),
            ));
        }
        Ok(())
    }
}

/// 探针要探测的坐标与取数列。两个来源（走库 / 直传）在这里汇成同一份。
struct ProbeTarget {
    coordinates: BitableCoordinates,
    field_id: Option<String>,
    field_name: Option<String>,
}

impl ProbeTarget {
    fn from_input(input: &PullProbeInput) -> Self {
        Self {
            coordinates: BitableCoordinates {
                app_token: input
                    .base_token
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_string(),
                table_id: input
                    .table_id
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_string(),
                view_id: input.view_id.as_deref().map(str::trim).map(str::to_string),
            },
            field_id: input.field_id.clone(),
            field_name: input.field_name.clone(),
        }
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
        .description(
            "用自建应用凭证真实调用飞书多维表格，回报字段类型与取值形态\
             （给 datasource_id 走库，或直传坐标先于任何数据源验证凭证链路）",
        )
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

    // 1) 凭证与出站设施。四件套收敛在 `outbound_setup`（T3 抽出来的那份），
    //    本函数不自己建连接池、也不自己拼凭证。
    let settings = match outbound_setup::require_settings(&context) {
        Ok(settings) => settings,
        Err((code, message)) => return Ok(ApiResponse::fail(code, message)),
    };
    let outbound = outbound_setup::build(&ctx, settings)?;

    let target = resolve_target(&input, &context).await?;

    // 2) 解析取数列：field_id -> 精确字段名，并校验它是单值字段。
    let fields = list_all_fields(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        &target.coordinates,
    )
    .await
    .map_err(outbound_setup::outbound_error)?;

    let (field_name, field) = match target.field_id.as_deref() {
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
            let name = target.field_name.clone().unwrap_or_default();
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
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        &target.coordinates,
        std::slice::from_ref(&field_name),
        PROBE_PAGE_SIZE,
    )
    .await
    .map_err(outbound_setup::outbound_error)?;

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

/// 决定这次探什么：走库（`datasource_id`）还是直传坐标。
///
/// 走库模式要读两处：那条表级行（坐标）与它的**第一条启用中的绑定**（取数列）。
/// 「第一条」而不是按 `source_key` 挑，是因为探针要回答的是「这条源现在能不能拉」，
/// 任一条绑定都足以把它答出来；而启用中的绑定为空时，这条源本来就没有可拉的列。
async fn resolve_target(
    input: &PullProbeInput,
    context: &FeishuContext,
) -> Result<ProbeTarget, BaseError> {
    let Some(datasource_id) = input.datasource_id else {
        return Ok(ProbeTarget::from_input(input));
    };

    let row = context
        .datasources()
        .query()
        .select_fields(&["bitable_base_token", "bitable_table_id", "bitable_view_id"])?
        .where_eq("id", serde_json::json!(datasource_id))?
        .optional()
        .await?;
    let Some(row) = row else {
        return Err(BaseError::RecordNotFound("数据源不存在".to_string()));
    };

    let binding = context
        .datasource_fields()
        .query()
        .select_fields(&["field_id"])?
        .where_eq("datasource_id", serde_json::json!(datasource_id))?
        .where_eq("enabled", serde_json::json!(true))?
        .optional()
        .await?;

    // 显式给的 field_id / field_name 优先：排查「就是这一列」时不该被第一条绑定盖掉。
    let field_id = input.field_id.clone().or_else(|| {
        binding
            .as_ref()
            .and_then(|row| row.optional("field_id").ok().flatten())
    });

    Ok(ProbeTarget {
        coordinates: BitableCoordinates {
            app_token: trimmed_column(&row, "bitable_base_token")?.unwrap_or_default(),
            table_id: trimmed_column(&row, "bitable_table_id")?.unwrap_or_default(),
            view_id: trimmed_column(&row, "bitable_view_id")?,
        },
        field_id,
        field_name: input.field_name.clone(),
    })
}

/// 读一列并 trim；空串与 `NULL` 一样按「没有」处理，避免拿一个空坐标去打飞书。
fn trimmed_column(
    row: &yang_base::table::Record,
    column: &str,
) -> Result<Option<String>, BaseError> {
    Ok(row
        .optional::<String>(column)?
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty()))
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
            datasource_id: None,
            base_token: Some("bascnCMII2ORej2RItqpZZUNMIe".to_string()),
            table_id: Some("tblxI2tWaxP5dG7p".to_string()),
            field_id: field_id.map(str::to_string),
            field_name: field_name.map(str::to_string),
            view_id: None,
        }
    }

    #[test]
    fn requires_both_coordinates_in_direct_mode() {
        let mut payload = input(Some("fldA"), None);
        payload.base_token = Some("  ".to_string());
        assert!(payload.validate().is_err(), "空 base_token 应被拒绝");

        let mut payload = input(Some("fldA"), None);
        payload.table_id = None;
        assert!(payload.validate().is_err(), "缺 table_id 应被拒绝");
    }

    #[test]
    fn requires_at_least_one_field_selector_in_direct_mode() {
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
    fn a_datasource_id_is_enough_on_its_own() {
        // 走库模式：坐标与取数列都从那条源上读，请求体里什么都不必再给
        let payload = PullProbeInput {
            datasource_id: Some(7),
            base_token: None,
            table_id: None,
            field_id: None,
            field_name: None,
            view_id: None,
        };
        assert!(payload.validate().is_ok());
    }

    #[test]
    fn rejects_a_non_positive_datasource_id() {
        let mut payload = input(Some("fldA"), None);
        payload.datasource_id = Some(0);
        assert!(payload.validate().is_err(), "id 必须是正整数");
    }

    #[test]
    fn the_direct_mode_still_trims_its_coordinates() {
        // 直传坐标是旧路径，trim 行为不能因为多了一个模式而改变
        let mut payload = input(Some("fldA"), None);
        payload.base_token = Some("  base  ".to_string());
        let target = ProbeTarget::from_input(&payload);
        assert_eq!(target.coordinates.app_token, "base");
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

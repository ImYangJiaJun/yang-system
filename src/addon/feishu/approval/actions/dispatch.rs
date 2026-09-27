//! 审批派发端点：多维表格按钮的入口。
//!
//! # 两种粒度，一个端点
//!
//! 请求体带 `record_id` 时**同步**处理单条（行内按钮 `buttonField`），当场创建实例、
//! 取编号、回写，并把结果返回给工作流。不带时**异步**受理（页面按钮 `buttonElement`），
//! 立即返回 `accepted`，由后台 worker 限速处理全表待处理记录。
//!
//! 为什么粒度决定同步/异步：
//!
//! - 单条 = 创建 + 取详情两次调用，秒级完成，远在飞书 HTTP 节点 **60 秒上限**之内，
//!   所以能同步——而且同步才有价值：工作流可以据此发消息或写日志。
//! - 全表可能有几十到几百条，受创建接口 **100 次/分钟** 限制，同步必然超时。
//!
//! **两种 button_type 的输出能力不同**：`buttonField` 能引用记录的字段与属性
//! （所以传得出 `record_id`），`buttonElement` **仅基础触发属性**——这是工作流侧的
//! 硬约束，不是本端点的设计选择。
//!
//! # 鉴权
//!
//! 复用 `ManagementTokenMiddleware`（与多维表格写入端点同一把静态 Token），
//! Action 保持 `public`。中间件在 `actions/mod.rs` 里按 ActionRef 精确挂载。
//!
//! **此外还必须校验 `base_token`/`table_id` 落在已配置的启用行内**：光靠 Token 保密
//! 不够——持 Token 者可以传任意表格坐标，那等于把「以任意用户身份创建审批实例」
//! 这个能力开放到所有协作表格上。这是本端点最重要的一道防线。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::approval_convert::FixedOffset;
use crate::addon::feishu::domain::approval_dispatch::{
    dispatch_one, widget_maps_from_rows, BitableBackfill, DispatchInput as OrchestrationInput,
};
use crate::addon::feishu::domain::bitable::{self, BitableCoordinates};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound_setup;

/// 派发端点的输入契约。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct DispatchInput {
    /// 多维表格 token。
    pub(super) base_token: String,
    /// 数据表 id。
    pub(super) table_id: String,
    /// 单条处理时的记录 id；省略表示处理全表待处理记录。
    #[serde(default)]
    pub(super) record_id: Option<String>,
}

impl ParamInput for DispatchInput {
    fn params() -> Params {
        Params::new()
    }
}

impl DispatchInput {
    /// 进入处理前的入参校验。
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
        if let Some(record_id) = &self.record_id {
            if record_id.trim().is_empty() {
                return Err(BaseError::ParamInvalid(
                    "record_id".to_string(),
                    "传了就必须非空；处理全表请整个省略该字段".to_string(),
                ));
            }
        }
        Ok(())
    }
}

/// 派发结果。
///
/// **响应体必须扁平**：工作流的 `HTTPClientAction` 在 `response_type=json` 时
/// **只能引用 `response_value` 中声明过的字段**，嵌套结构取不到。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct DispatchResultBody {
    /// 是否成功（单条）或是否受理（全表）。
    accepted: bool,
    /// 结果说明。单条失败时是**可行动的原因**，不是原始响应体。
    message: String,
    /// 单条成功时的审批单编号。
    #[serde(skip_serializing_if = "Option::is_none")]
    serial_number: Option<String>,
}

/// 注册派发端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("dispatch_approval"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/approval/dispatch")
        .display_name("创建飞书审批实例")
        .description("多维表格按钮入口：单条同步创建，或受理全表待处理记录")
        .public()
        .register()
}

/// 处理派发请求。
pub(super) async fn handle(
    _ctx: ActionContext,
    input: DispatchInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    // ---- 白名单校验：坐标必须在已配置且启用的行内 ----
    //
    // 这是本端点最重要的防线。管理 Token 是全局单值、不按数据源绑定
    // （既有决策 A8），所以光靠 Token 保密不足以限制可利用面。
    let config = context
        .approval_configs()
        .query()
        .where_eq("base_token", serde_json::json!(input.base_token.trim()))?
        .where_eq("table_id", serde_json::json!(input.table_id.trim()))?
        .where_eq("enabled", serde_json::json!(true))?
        .optional()
        .await?;

    let Some(config) = config else {
        return Ok(ApiResponse::fail(
            40401,
            "该多维表格未配置审批派发（或已停用）",
        ));
    };
    let config_id: i64 = config.require("id")?;

    match input.record_id.as_deref() {
        // ---- 单条：同步处理 ----
        Some(record_id) => dispatch_single(&_ctx, &context, config_id, record_id).await,
        // ---- 全表：异步受理 ----
        None => {
            // 受理的实现在后续任务（worker）里落地。当前版本明确返回「未启用」，
            // 而不是假装受理成功——后者会让工作流显示成功而实际什么都没做。
            Ok(ApiResponse::fail(50101, "全表派发尚未启用，请使用行内按钮"))
        }
    }
}

/// 同步处理一条记录。
///
/// 编排逻辑在 `domain::approval_dispatch`；这里负责取配置、读记录、装配出站栈、
/// 折算响应——都是「把领域能力接起来」的接线工作，不含业务判断。
async fn dispatch_single(
    ctx: &ActionContext,
    context: &FeishuContext,
    config_id: i64,
    record_id: &str,
) -> Result<ApiResponse, BaseError> {
    let Some(settings) = context.settings().filter(|value| value.can_pull()) else {
        return Ok(ApiResponse::fail(50301, "飞书出站凭证未配置"));
    };

    // ---- 配置 ----
    let config_row = context
        .approval_configs()
        .query()
        .where_primary_key_eq(serde_json::json!(config_id))?
        .optional()
        .await?
        .ok_or_else(|| BaseError::ConfigError("审批配置在读取前被删除".to_string()))?;

    let base_token: String = config_row.require("base_token")?;
    let table_id: String = config_row.require("table_id")?;
    let approval_code: String = config_row.require("approval_code")?;
    let applicant_field: String = config_row.require("applicant_field")?;
    let backfill_field: String = config_row.require("backfill_field")?;
    let base_timezone: String = config_row.require("base_timezone")?;

    // ---- 字段映射 ----
    let map_rows = context
        .approval_field_maps()
        .query()
        .where_eq("config_id", serde_json::json!(config_id))?
        .all()
        .await?;
    let map_maps: Vec<serde_json::Map<String, serde_json::Value>> = map_rows
        .into_iter()
        .map(|record| record.into_map())
        .collect();
    let Some(widgets) = widget_maps_from_rows(&map_maps, |_| None) else {
        return Ok(ApiResponse::fail(
            50001,
            "字段映射配置损坏（转换器标识非法）",
        ));
    };
    if widgets.is_empty() {
        return Ok(ApiResponse::fail(50002, "该配置没有字段映射"));
    }
    let timezone_offset = match FixedOffset::from_iana(base_timezone.trim()) {
        Ok(offset) => offset,
        Err(error) => {
            return Ok(ApiResponse::fail(
                50003,
                format!("Base 时区不可用：{error}"),
            ))
        }
    };

    // ---- 只投影需要的字段 ----
    //
    // 多读列既慢，又可能因为某列形态异常而使整个查询失败，而这里只要少数几列。
    let mut field_names: Vec<String> = widgets
        .iter()
        .map(|widget| widget.bitable_field.clone())
        .collect();
    field_names.push(applicant_field.clone());
    field_names.sort();
    field_names.dedup();

    let outbound = outbound_setup::build(ctx, settings)?;
    let coordinates = BitableCoordinates {
        app_token: base_token,
        table_id,
        view_id: None,
    };

    // ---- 读记录 ----
    //
    // 按 `record_id` 过滤而不是整表分页——后者会把整张表拉回来只为拿一行。
    let found = bitable::search_records(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        &coordinates,
        bitable::record_by_id_query(record_id, &field_names),
    )
    .await;
    let cells = match found {
        Ok(data) => match data.items.into_iter().next() {
            // `RecordItem.fields` 是 `BTreeMap`（读取侧的形态），而编排的转换器
            // 与申请人提取都按 `serde_json::Map` 工作——这里换一次容器，
            // 而不是让 domain 依赖读取侧的容器类型。
            Some(record) => record.fields.into_iter().collect(),
            None => return Ok(ApiResponse::fail(40402, "多维表格里找不到该记录")),
        },
        Err(failure) => {
            return Ok(ApiResponse::fail(
                50201,
                format!("读取记录失败：{}", failure.message),
            ))
        }
    };

    // ---- 编排 ----
    let backfill = BitableBackfill {
        transport: outbound.transport(),
        sleeper: outbound.sleeper(),
        tokens: &outbound.tokens,
        coordinates: &coordinates,
    };
    let result = dispatch_one(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        &backfill,
        &OrchestrationInput {
            coordinates: &coordinates,
            record_id,
            cells: &cells,
            applicant_field: &applicant_field,
            backfill_field: &backfill_field,
            approval_code: &approval_code,
            widgets: &widgets,
            timezone_offset,
        },
    )
    .await;

    let (accepted, message, serial_number) = result.response_parts();
    response(accepted, message, serial_number)
}

/// 组一个扁平响应体。
///
/// 用 `ApiResponse::success` 的 `data` 通道而不是把结果塞进 `message`：工作流的
/// `response_value` 只能引用**声明过的字段**，所以结果要落成具名字段（`accepted`
/// / `serial_number`）才好引用，扁平结构最好声明。
///
/// 失败（`accepted = false`）仍走 `success` 通道：这是**业务结果**不是接口错误，
/// 工作流要能同时读到成败与原因。接口级错误（鉴权失败、坐标未配置）才用 `fail`。
fn response(
    accepted: bool,
    message: String,
    serial_number: Option<String>,
) -> Result<ApiResponse, BaseError> {
    ApiResponse::success(
        DispatchResultBody {
            accepted,
            message,
            serial_number,
        },
        "ok",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_input() -> DispatchInput {
        DispatchInput {
            base_token: "appbcbWCzen6".to_string(),
            table_id: "tblsRc9GRRX".to_string(),
            record_id: Some("rec001".to_string()),
        }
    }

    #[test]
    fn blank_coordinates_are_rejected() {
        let mut input = valid_input();
        input.base_token = "   ".to_string();
        assert!(input.validate().is_err());

        let mut input = valid_input();
        input.table_id = String::new();
        assert!(input.validate().is_err());
    }

    #[test]
    fn blank_record_id_is_rejected_but_absence_is_fine() {
        // 空的 record_id 多半是工作流把引用取空了——静默当成「全表」会让一次
        // 单条点击变成批量处理，是危险的方向。
        let mut input = valid_input();
        input.record_id = Some("  ".to_string());
        assert!(input.validate().is_err());

        let mut input = valid_input();
        input.record_id = None;
        assert!(input.validate().is_ok(), "省略 record_id 表示全表，合法");
    }

    #[test]
    fn valid_input_passes() {
        assert!(valid_input().validate().is_ok());
    }

    #[test]
    fn result_body_is_flat_for_workflow_reference() {
        // 工作流 response_value 只能引用声明过的字段，嵌套结构取不到。
        let body = DispatchResultBody {
            accepted: true,
            message: "ok".to_string(),
            serial_number: Some("202609280001".to_string()),
        };
        let json = serde_json::to_value(&body).unwrap_or_else(|error| panic!("{error}"));
        let object = json.as_object().unwrap_or_else(|| panic!("应是对象"));
        assert!(object.contains_key("accepted"));
        assert!(object.contains_key("message"));
        assert!(object.contains_key("serial_number"));
        // 所有值都是标量，没有嵌套对象/数组。
        for (key, value) in object {
            assert!(
                !value.is_object() && !value.is_array(),
                "字段 {key} 是嵌套结构，工作流引用不到"
            );
        }
    }

    #[test]
    fn serial_number_is_omitted_when_absent() {
        // 全表受理的响应没有编号；省略而不是给 null，让工作流的字段声明更简单。
        let body = DispatchResultBody {
            accepted: true,
            message: "已受理".to_string(),
            serial_number: None,
        };
        let json = serde_json::to_value(&body).unwrap_or_else(|error| panic!("{error}"));
        assert!(!json
            .as_object()
            .map(|o| o.contains_key("serial_number"))
            .unwrap_or(true));
    }
}

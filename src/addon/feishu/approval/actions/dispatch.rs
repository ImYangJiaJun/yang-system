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
use crate::addon::feishu::domain::approval_provision::{
    build_plan, insert_plan, ProvisionError, ProvisionInput, ProvisionPlan,
};
use crate::addon::feishu::domain::bitable::{self, BitableCoordinates};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound_setup;
use crate::config::FeishuSettings;
use crate::feishu_approval_worker::ApprovalDispatchHandle;

use crate::addon::feishu::domain::approval_dispatch::Backfill;

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
    /// 目标审批定义。**配置不存在时**由它建配置（首次调用）。
    ///
    /// 只有三条配置信息是「只有调用方知道」的——要提哪个定义、谁当发起人、
    /// 编号写回哪列，它们都写在工作流的 `raw_body` 里。已配好时本字段与另两条
    /// 被忽略，改配置要**删掉配置行**再调一次。
    #[serde(default)]
    pub(super) approval_code: Option<String>,
    /// 申请人员列（`field_id` **或**列名都接受）。仅首次建配置时消费。
    #[serde(default)]
    pub(super) applicant_field: Option<String>,
    /// 回填列（同上）。仅首次建配置时消费。
    #[serde(default)]
    pub(super) backfill_field: Option<String>,
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
        // 配置三件套**要么全给、要么全不给**。半套的后果是：建了配置但没有回填列，
        // 而「没有回填列」意味着校验失败也报不进多维表格——那条错误只能回到 HTTP
        // 响应里，而页面按钮拿不到它（工作流的 response_value 只有那几个字段）。
        let given = [
            self.approval_code.is_some(),
            self.applicant_field.is_some(),
            self.backfill_field.is_some(),
        ];
        let count = given.iter().filter(|value| **value).count();
        if count != 0 && count != 3 {
            return Err(BaseError::ParamInvalid(
                "approval_code".to_string(),
                "首次建配置时 approval_code / applicant_field / backfill_field 必须同时给出（缺任一条时校验失败也报不进多维表格）"
                    .to_string(),
            ));
        }
        if let (Some(code), Some(applicant), Some(backfill)) = (
            self.approval_code.as_deref(),
            self.applicant_field.as_deref(),
            self.backfill_field.as_deref(),
        ) {
            for (name, value) in [
                ("approval_code", code),
                ("applicant_field", applicant),
                ("backfill_field", backfill),
            ] {
                if value.trim().is_empty() {
                    return Err(BaseError::ParamInvalid(
                        name.to_string(),
                        "不能为空".to_string(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// 是否给出了建配置所需的三件套。
    fn has_provision_fields(&self) -> bool {
        self.approval_code.is_some()
            && self.applicant_field.is_some()
            && self.backfill_field.is_some()
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
    ctx: ActionContext,
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

    // ---- 没有配置行 → 首次调用，自动建配置 ----
    //
    // 校验不过就报错并**回填到回填列**（使用方要的就是「配置期报错」，而报错必须
    // 落在多维表格里才有人看见——HTTP 响应对页面按钮无用，见 `response_value` 的
    // 字段限制）。所以**回填列必须由调用方给**：配置还没建成时本服务不知道写哪一列。
    let config_id: i64 = match config {
        Some(row) => row.require("id")?,
        None => {
            if !input.has_provision_fields() {
                return Ok(ApiResponse::fail(
                    40401,
                    "该多维表格未配置审批派发。首次调用请同时给出 approval_code / applicant_field / backfill_field",
                ));
            }
            // 建配置要在飞书与本库两端取数，凭证缺了根本走不下去。
            let Some(settings) = context.settings().filter(|value| value.can_pull()) else {
                return Ok(ApiResponse::fail(50301, "飞书出站凭证未配置"));
            };

            match provision(&ctx, &context, settings, &input).await {
                Ok(id) => id,
                Err(failure) => {
                    // 报错要**同时**回填与返回：回填让表格里的人看见，返回让工作流
                    // 的日志里也有原文。回填失败只降级为「只返回」——配置根本没建成，
                    // 也不该因为回填这一下而把真实原因盖掉。
                    backfill_provision_failure(&ctx, settings, &input, &failure).await;
                    return Ok(ApiResponse::fail(35600, failure.to_string()));
                }
            }
        }
    };

    match input.record_id.as_deref() {
        // ---- 单条：同步处理 ----
        Some(record_id) => dispatch_single(&ctx, &context, config_id, record_id).await,
        // ---- 全表：异步受理 ----
        //
        // 受理**必须在白名单校验之后**：把 Token 当成「可以指任意表格」的通行证
        // 是本端点最大的越权面。上面那句 `config` 查询就是白名单，通过了才放行。
        None => {
            // 拿不到句柄说明 worker 没起（凭证缺失）。**不返回 accepted**——
            // 那会让工作流显示成功而实际什么都没发生。
            let handle = ctx.tools().extension::<ApprovalDispatchHandle>()?;
            handle.request_dispatch()?;

            // 语义是「跑一轮全队列」，**不是**只跑本表。
            //
            // 用方原意就是「处理所有没有审批编号的数据」，而这三张表是按
            // `(base_token, table_id)` 配置的；按表分流会让同一队列出现「点了 A 的
            // 按钮却不处理 B 的数据」这种反直觉行为。多表并发时谁先点谁先跑整批，
            // 结果仍是一致的（uuid 幂等兜底）。
            response(true, "已受理，处理结果稍后回填至表格".to_string(), None)
        }
    }
}

/// 首次调用：取定义、取列、按名匹配、校验，全过才把配置与映射**同一事务**落库。
///
/// 落库前的一切失败都是**可修**的（列没建、名字不对、绑定没配），所以这里不写任何
/// 东西——半套配置（有配置没映射）比不建更坏，它会让之后每一次派发都卡在
/// 「该配置没有字段映射」而配置行看着是好的。
async fn provision(
    ctx: &ActionContext,
    context: &FeishuContext,
    settings: &FeishuSettings,
    input: &DispatchInput,
) -> Result<i64, ProvisionError> {
    let outbound = outbound_setup::build(ctx, settings)
        .map_err(|error| ProvisionError::Fetch(error.to_string()))?;

    let plan: ProvisionPlan = build_plan(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        context,
        &ProvisionInput {
            base_token: input.base_token.trim(),
            table_id: input.table_id.trim(),
            approval_code: input.approval_code.as_deref().unwrap_or_default().trim(),
            applicant_field: input.applicant_field.as_deref().unwrap_or_default(),
            backfill_field: input.backfill_field.as_deref().unwrap_or_default(),
            // 部署默认时区。多维表格日期是不带时区的毫秒时间戳，而审批 `date` 控件要
            // 带偏移量——猜错会让审批里的时间整体偏移，所以必须由配置给定。
            base_timezone: &settings.approval_base_timezone,
        },
    )
    .await?;

    let mut transaction = ctx
        .begin_transaction()
        .await
        .map_err(|error| ProvisionError::Store(error.to_string()))?;
    let result = insert_plan(context, &mut transaction, &plan).await;
    let config_id = match result {
        Ok(id) => id,
        Err(error) => {
            if let Err(rollback) = transaction.rollback().await {
                tracing::error!(error = %rollback, "审批配置回滚失败");
            }
            return Err(ProvisionError::Store(error.to_string()));
        }
    };
    transaction
        .commit()
        .await
        .map_err(|error| ProvisionError::Store(error.to_string()))?;

    tracing::info!(
        config_id,
        base_token = %input.base_token.trim(),
        table_id = %input.table_id.trim(),
        approval_code = %input.approval_code.as_deref().unwrap_or_default().trim(),
        "审批派发配置已自动创建"
    );
    Ok(config_id)
}

/// 建配置失败时，把原因**写回回填列**，再返回原始错误。
///
/// 这是「配置期就报错」的落点：报错只留在 HTTP 响应里，页面按钮的人看不到
/// （工作流的 `response_value` 只声明了 `accepted` / `message` / `serial_number`）。
///
/// 只在**首次**（调用方给了三件套）时写：老配置坏掉时回填列可能已被当成输出位占用，
/// 再写一句校验错误会盖掉真正的编号。老配置的错误走日志。
async fn backfill_provision_failure(
    ctx: &ActionContext,
    settings: &FeishuSettings,
    input: &DispatchInput,
    failure: &ProvisionError,
) {
    if !input.has_provision_fields() {
        return;
    }
    let Some(record_id) = input.record_id.as_deref().filter(|id| !id.is_empty()) else {
        // 全表受理时没有具体记录可写——那一句只有 queue 才有意义，
        // 而队列根本没建起来。让它退回日志。
        tracing::warn!(error = %failure, "审批配置校验失败（全表受理，无记录可回填）");
        return;
    };
    let Some(backfill_field) = input.backfill_field.as_deref() else {
        return;
    };

    let Ok(outbound) = outbound_setup::build(ctx, settings) else {
        return;
    };
    let coordinates = BitableCoordinates {
        app_token: input.base_token.trim().to_string(),
        table_id: input.table_id.trim().to_string(),
        view_id: None,
    };
    let writer = BitableBackfill {
        transport: outbound.transport(),
        sleeper: outbound.sleeper(),
        tokens: &outbound.tokens,
        coordinates: &coordinates,
    };

    // 回填的是**配置的错误**，不是数据的错误，所以要留个前缀让人分得开：
    // 表格里同时会出现「缺必填列」（数据问题）与「配置校验不过」（本问题）。
    let message = format!("[配置] {failure}");
    if let Err(error) = writer.write(record_id, backfill_field, &message).await {
        // 回填失败不改变结论——错误已经返回给工作流了。这里降级为日志：
        // 真正的原因（配置没建好）比「报错没写进表格」更值得留下。
        tracing::warn!(error = %error.message, "配置校验失败的回填写入失败");
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

    let outbound = outbound_setup::build(ctx, settings)?;
    let coordinates = BitableCoordinates {
        app_token: base_token,
        table_id,
        view_id: None,
    };

    // ---- 表结构（重映射要用，先拿） ----
    //
    // 官方《批量获取记录》的 `fields` **按字段名**作键，而本域按 `field_id` 取值
    // （配置存 id）。没有这一次重映射，按 id 去查一个按名作键的 map 永远查不到，
    // 症状是**每条记录都「缺少申请人」**——不报错，只是永远停在等待态。
    let fields = match bitable::list_all_fields(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        &coordinates,
    )
    .await
    {
        Ok(fields) => fields,
        Err(failure) => {
            return Ok(ApiResponse::fail(
                50201,
                format!("读取表结构失败：{}", failure.message),
            ))
        }
    };

    // ---- 读记录 ----
    //
    // 走 `records/batch_get` 而不是「查询记录 + filter」：`record_id` 是**响应里的
    // 系统字段、不是可过滤字段**，拿它当 `field_name` 过滤得不到按 id 的定位。
    let found = bitable::get_records_by_ids(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        &coordinates,
        &[record_id.to_string()],
    )
    .await;
    let cells = match found {
        Ok(items) => match items.into_iter().next() {
            Some(record) => match bitable::rekey_cells_by_field_id(&fields, &record.fields) {
                Ok(cells) => cells,
                Err(error) => {
                    return Ok(ApiResponse::fail(
                        50201,
                        format!("解析记录字段失败：{error}"),
                    ))
                }
            },
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

    /// 已配好配置的请求：三件套**全不给**（那是首次建配置才用的）。
    fn valid_input() -> DispatchInput {
        DispatchInput {
            base_token: "appbcbWCzen6".to_string(),
            table_id: "tblsRc9GRRX".to_string(),
            record_id: Some("rec001".to_string()),
            approval_code: None,
            applicant_field: None,
            backfill_field: None,
        }
    }

    /// 首次建配置的请求：三件套齐备。
    fn first_call_input() -> DispatchInput {
        DispatchInput {
            approval_code: Some("CODE-TEST".to_string()),
            applicant_field: Some("申请人".to_string()),
            backfill_field: Some("审批编号".to_string()),
            ..valid_input()
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
    fn provision_fields_are_all_or_nothing() {
        // 半套（只给 approval_code）最危险：配置建了却没回填列，于是**校验失败也
        // 报不进多维表格**——报错只能回到 HTTP 响应里，而页面按钮拿不到它。
        let mut input = first_call_input();
        input.applicant_field = None;
        assert!(input.validate().is_err(), "只给 approval_code 应被拒");

        let mut input = first_call_input();
        input.backfill_field = None;
        assert!(
            input.validate().is_err(),
            "只给 approval_code + applicant 应被拒"
        );

        // 三件套齐备 → 合法。
        assert!(first_call_input().validate().is_ok());
        // 全不给也合法（那是「配置已存在」的常态），且**不算**给了三件套。
        assert!(valid_input().validate().is_ok());
        assert!(
            !valid_input().has_provision_fields(),
            "三件套全 None 时不能算「已给出」——那正是老配置的常态"
        );
        assert!(first_call_input().has_provision_fields());
    }

    #[test]
    fn blank_provision_field_is_rejected() {
        // 空串与「没给」不同：给了空串是调用方的 bug，要指出来而不是当作没给。
        let mut input = first_call_input();
        input.approval_code = Some("  ".to_string());
        assert!(input.validate().is_err(), "空 approval_code 应被拒");
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

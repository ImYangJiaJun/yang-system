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

use crate::addon::feishu::approval::domain::request_log_writer::{
    normalized_requested_by, outcome_for, Outcome, RequestLog,
};
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
///
/// `Serialize` 是请求记录落库需要的：`request_body` 列存的就是本结构的序列化
/// 原文（见 `domain/request_log_writer.rs`）。
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct DispatchInput {
    /// 多维表格 token。
    pub(super) base_token: String,
    /// 数据表 id。
    pub(super) table_id: String,
    /// 单条处理时的记录 id；省略表示处理全表待处理记录。
    #[serde(default)]
    pub(super) record_id: Option<String>,
    /// 请求人（工作流触发人）。工作流模板在 `raw_body` 里带 `$.step_btn.user`；
    /// 未带或 trim 后为空时落库统一记 `feishu-workflow`。仅记录用，不影响派发。
    #[serde(default)]
    pub(super) requested_by: Option<String>,
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
        // 请求人只记录不参与派发：trim 后空串按缺失处理（落 `feishu-workflow`），
        // 非空则长度受控——列上限 varchar(128) 按字符数计。
        if let Some(requested_by) = &self.requested_by {
            if requested_by.trim().chars().count() > 128 {
                return Err(BaseError::ParamInvalid(
                    "requested_by".to_string(),
                    "不能超过 128 个字符".to_string(),
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

/// 一次派发请求的结果记录事实：所有出口（含 `validate()` 失败）都收敛成它，
/// 外层**唯一一处**组装响应并落库——散落的出口各写一遍必漏。
struct DispatchOutcome {
    /// 结果四桶。
    outcome: Outcome,
    /// 结果说明（复用响应 message；失败时为可行动原因）。
    message: String,
    /// 单条成功时的审批单编号。
    serial_number: Option<String>,
    /// 配置存在或建成后关联；校验失败与配置未建成（40401/35600）的请求为空。
    config_id: Option<i64>,
    /// 原本要返回给工作流的结果。落记录绝不改变它——写失败只降级日志，
    /// 派发已提交到 task 表与飞书，不能被日志拖死。
    response: Result<ApiResponse, BaseError>,
}

impl DispatchOutcome {
    /// `ApiResponse::fail` 出口：记录文案与响应文案同源，不写第二遍。
    fn fail(code: i32, config_id: Option<i64>, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            outcome: Outcome::Failed,
            message: message.clone(),
            serial_number: None,
            config_id,
            response: Ok(ApiResponse::fail(code, message)),
        }
    }

    /// 内部错误冒泡出口：`Err(BaseError)` 原样保留，文案用错误的 Display。
    fn internal(config_id: Option<i64>, error: BaseError) -> Self {
        Self {
            outcome: Outcome::Failed,
            message: error.to_string(),
            serial_number: None,
            config_id,
            response: Err(error),
        }
    }

    /// 成功 / 等待 / 受理出口（响应走 `success` 通道）。
    fn success(
        outcome: Outcome,
        config_id: Option<i64>,
        accepted: bool,
        message: String,
        serial_number: Option<String>,
    ) -> Self {
        Self {
            outcome,
            message: message.clone(),
            serial_number: serial_number.clone(),
            config_id,
            response: response(accepted, message, serial_number),
        }
    }
}

/// 处理派发请求。
pub(super) async fn handle(
    ctx: ActionContext,
    input: DispatchInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    // 业务主体抽成内部函数、返回结构化结果而不是直接 return：每个出口
    // （含 `validate()` 失败——它也要落一条 failed 记录，且要落在还能拿到
    // `request_body` 的位置）都收敛成一条可落库的记录事实。落库用独立连接
    // （`TableQuery::insert` 不经事务自动提交），写失败只降级日志。
    let outcome = match handle_dispatch(&ctx, &input, &context).await {
        Ok(outcome) => outcome,
        // 白名单查询这类「连 config_id 都拿不到」的内部错误：折算成失败记录。
        Err(error) => DispatchOutcome::internal(None, error),
    };
    write_request_log(&context, &input, &outcome).await;
    outcome.response
}

/// 派发业务主体。
///
/// 返回 `Err` 的只有白名单查询这类「连 config_id 都拿不到」的内部错误；
/// 已知 `config_id` 的单条/受理/建配置出口都在内部折算成
/// [`DispatchOutcome`]（见各分支）。
async fn handle_dispatch(
    ctx: &ActionContext,
    input: &DispatchInput,
    context: &FeishuContext,
) -> Result<DispatchOutcome, BaseError> {
    // 校验失败**不冒泡**——它同样要落记录，这里兜住再折算成失败出口。
    if let Err(error) = input.validate() {
        return Ok(DispatchOutcome::internal(None, error));
    }

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
                return Ok(DispatchOutcome::fail(
                    40401,
                    None,
                    "该多维表格未配置审批派发。首次调用请同时给出 approval_code / applicant_field / backfill_field",
                ));
            }
            // 建配置要在飞书与本库两端取数，凭证缺了根本走不下去。
            let Some(settings) = context.settings().filter(|value| value.can_pull()) else {
                return Ok(DispatchOutcome::fail(50301, None, "飞书出站凭证未配置"));
            };

            match provision(ctx, context, settings, input).await {
                Ok(id) => id,
                Err(failure) => {
                    // 报错要**同时**回填与返回：回填让表格里的人看见，返回让工作流
                    // 的日志里也有原文。回填失败只降级为「只返回」——配置根本没建成，
                    // 也不该因为回填这一下而把真实原因盖掉。
                    backfill_provision_failure(ctx, settings, input, &failure).await;
                    return Ok(DispatchOutcome::fail(35600, None, failure.to_string()));
                }
            }
        }
    };

    match input.record_id.as_deref() {
        // ---- 单条：同步处理 ----
        Some(record_id) => match dispatch_single(ctx, context, config_id, record_id).await {
            Ok(outcome) => Ok(outcome),
            // 单条内部的 `?` 出口：config_id 在调用点已知，失败记录带上它。
            Err(error) => Ok(DispatchOutcome::internal(Some(config_id), error)),
        },
        // ---- 全表：异步受理 ----
        //
        // 受理**必须在白名单校验之后**：把 Token 当成「可以指任意表格」的通行证
        // 是本端点最大的越权面。上面那句 `config` 查询就是白名单，通过了才放行。
        None => {
            // 拿不到句柄说明 worker 没起（凭证缺失）。**不返回 accepted**——
            // 那会让工作流显示成功而实际什么都没发生。
            let handle = match ctx.tools().extension::<ApprovalDispatchHandle>() {
                Ok(handle) => handle,
                Err(error) => return Ok(DispatchOutcome::internal(Some(config_id), error)),
            };
            if let Err(error) = handle.request_dispatch() {
                return Ok(DispatchOutcome::internal(Some(config_id), error));
            }

            // 语义是「跑一轮全队列」，**不是**只跑本表。
            //
            // 用方原意就是「处理所有没有审批编号的数据」，而这三张表是按
            // `(base_token, table_id)` 配置的；按表分流会让同一队列出现「点了 A 的
            // 按钮却不处理 B 的数据」这种反直觉行为。多表并发时谁先点谁先跑整批，
            // 结果仍是一致的（uuid 幂等兜底）。
            Ok(DispatchOutcome::success(
                Outcome::Accepted,
                Some(config_id),
                true,
                "已受理，处理结果稍后回填至表格".to_string(),
                None,
            ))
        }
    }
}

/// 把一次派发请求落成一条请求记录（独立连接自动提交）。
///
/// 写失败只降级 `tracing::error!`，**绝不改变派发结果**——见
/// [`DispatchOutcome::response`] 的注释。
async fn write_request_log(
    context: &FeishuContext,
    input: &DispatchInput,
    outcome: &DispatchOutcome,
) {
    let request_body = match serde_json::to_string(input) {
        Ok(body) => body,
        Err(error) => {
            // 输入全是字符串字段，序列化失败实际不可达；真发生了也宁可不记
            // 也不记半行（request_body 必填）。
            tracing::error!(error = %error, "派发请求体序列化失败，放弃落请求记录");
            return;
        }
    };
    let log = RequestLog {
        requested_by: normalized_requested_by(input.requested_by.as_deref()),
        base_token: input.base_token.trim().to_string(),
        table_id: input.table_id.trim().to_string(),
        config_id: outcome.config_id,
        record_id: input
            .record_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        request_body,
        outcome: outcome.outcome,
        message: outcome.message.clone(),
        serial_number: outcome.serial_number.clone(),
        // 返回信封的 `data` JSON 原文；失败出口（`Err` 冒泡或 `ApiResponse::fail`）
        // 没有 data，落 NULL。
        response_body: outcome
            .response
            .as_ref()
            .ok()
            .and_then(|response| response.data.as_ref())
            .map(|data| data.to_string()),
    };
    if let Err(error) = context
        .approval_request_logs()
        .query()
        .insert(log.into_record())
        .await
    {
        tracing::error!(error = %error, "审批派发请求记录写入失败");
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

    if let Err(error) = write_config_failure(&writer, record_id, backfill_field, failure).await {
        // 回填失败不改变结论——错误已经返回给工作流了。这里降级为日志：
        // 真正的原因（配置没建好）比「报错没写进表格」更值得留下。
        tracing::warn!(error = %error.message, "配置校验失败的回填写入失败");
    }
}

/// 把「配置校验不过」写进回填列。
///
/// # 为什么单独抽出来
///
/// 这是「配置期报错」**唯一的用户可见出口**：HTTP 响应只有工作流日志看得到，而
/// 点按钮的人在多维表格里。整条链路——文案前缀、写哪一列、单元格形态——都只有在
/// 真的写一次之后才知道对不对，所以它必须能被端到端用例直接驱动。
/// `backfill_provision_failure` 里那层 `ActionContext`/凭证构造与本函数无关，
/// 不该成为它的测试门槛（`Backfill` trait 当初也是为同一个理由抽的）。
///
/// # 前缀 `[配置]` 不能省
///
/// 同一个回填列里会同时出现两类文本：**数据问题**（「缺必填列」，由
/// `classify_failure` 写入）与**配置问题**（本函数）。没有前缀，表格里的人分不开
/// 「去补这一行的数据」和「去改配置」——而这两件事的处置完全不同。
async fn write_config_failure(
    backfill: &dyn Backfill,
    record_id: &str,
    backfill_field: &str,
    failure: &ProvisionError,
) -> Result<(), crate::addon::feishu::domain::outbound::OutboundFailure> {
    backfill
        .write(record_id, backfill_field, &format!("[配置] {failure}"))
        .await
}

/// 同步处理一条记录。
///
/// 编排逻辑在 `domain::approval_dispatch`；这里负责取配置、读记录、装配出站栈、
/// 折算响应——都是「把领域能力接起来」的接线工作，不含业务判断。
///
/// 业务失败出口折成 [`DispatchOutcome::fail`]；`?` 冒泡的内部错误由调用点
/// （已知 `config_id`）统一折算成失败记录。
async fn dispatch_single(
    ctx: &ActionContext,
    context: &FeishuContext,
    config_id: i64,
    record_id: &str,
) -> Result<DispatchOutcome, BaseError> {
    let Some(settings) = context.settings().filter(|value| value.can_pull()) else {
        return Ok(DispatchOutcome::fail(
            50301,
            Some(config_id),
            "飞书出站凭证未配置",
        ));
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
        return Ok(DispatchOutcome::fail(
            50001,
            Some(config_id),
            "字段映射配置损坏（转换器标识非法）",
        ));
    };
    if widgets.is_empty() {
        return Ok(DispatchOutcome::fail(
            50002,
            Some(config_id),
            "该配置没有字段映射",
        ));
    }
    let timezone_offset = match FixedOffset::from_iana(base_timezone.trim()) {
        Ok(offset) => offset,
        Err(error) => {
            return Ok(DispatchOutcome::fail(
                50003,
                Some(config_id),
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
            return Ok(DispatchOutcome::fail(
                50201,
                Some(config_id),
                format!("读取表结构失败：{}", failure.message),
            ))
        }
    };

    // ---- 回填列：配置存 `field_id`，而写接口按**列名**作键 ----
    //
    // `batch_update` 的 `records[].fields` 是按列名作键的 map（《数据结构概述》：
    // 「key 是多维表格数据表中的字段名称」），而配置里存的是 id（`insert_plan`
    // 刻意如此——改列名不该让配置失效）。这一步就是两条轴之间的桥，与读侧的
    // `rekey_cells_by_field_id` 正好是同一件事的两个方向。
    //
    // 与 worker 的 `process_one` 用**同一个** `bitable::resolve_field_name`：同名
    // 不唯一时它报错而不是挑一列——按名字写的接口在这种情况下会写到不确定的那一列
    // 上，那比「写不进去」更坏。
    let backfill_field_name = match bitable::resolve_field_name(&fields, &backfill_field) {
        Ok(name) => name,
        Err(error) => {
            return Ok(DispatchOutcome::fail(
                50004,
                Some(config_id),
                format!("回填列不可用：{error}"),
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
                    return Ok(DispatchOutcome::fail(
                        50201,
                        Some(config_id),
                        format!("解析记录字段失败：{error}"),
                    ))
                }
            },
            None => {
                return Ok(DispatchOutcome::fail(
                    40402,
                    Some(config_id),
                    "多维表格里找不到该记录",
                ))
            }
        },
        Err(failure) => {
            return Ok(DispatchOutcome::fail(
                50201,
                Some(config_id),
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
            backfill_field_name: &backfill_field_name,
            approval_code: &approval_code,
            widgets: &widgets,
            timezone_offset,
        },
    )
    .await;

    let outcome = outcome_for(&result);
    let (accepted, message, serial_number) = result.response_parts();
    Ok(DispatchOutcome::success(
        outcome,
        Some(config_id),
        accepted,
        message,
        serial_number,
    ))
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
            requested_by: Some("测试触发人".to_string()),
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
    fn requested_by_is_optional_and_blank_is_treated_as_missing() {
        // 未带（旧工作流报文）与 trim 后空串都合法——落库统一记 feishu-workflow。
        let mut input = valid_input();
        input.requested_by = None;
        assert!(input.validate().is_ok(), "未带 requested_by 合法");

        let mut input = valid_input();
        input.requested_by = Some("   ".to_string());
        assert!(input.validate().is_ok(), "trim 后空串按缺失处理，不报错");

        let mut input = valid_input();
        input.requested_by = Some("  张三  ".to_string());
        assert!(input.validate().is_ok(), "带空白的请求人合法");
    }

    #[test]
    fn overlong_requested_by_is_rejected() {
        // 列上限 varchar(128) 按字符数计——中文按字符而不是按字节。
        let mut input = valid_input();
        input.requested_by = Some("张".repeat(129));
        assert!(input.validate().is_err(), "129 个字符应被拒");

        let mut input = valid_input();
        input.requested_by = Some("张".repeat(128));
        assert!(input.validate().is_ok(), "恰好 128 个字符合法");
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

    #[test]
    fn failure_accepted_flag_is_forwarded_not_hardcoded() {
        // 回归（对抗性验证抓到）：重构把单条失败出口的 accepted 从 false 翻成 true，
        // 工作流会按 accepted 分支把失败当成功、Retryable 依赖 accepted 的重试不触发。
        // `response` 的 accepted 必须由调用方透传——失败（Terminal/Retryable）为 false，
        // 等待与受理（Waiting/批量）为 true。
        let failed = response(false, "暂时失败，可重试：飞书服务错误".to_string(), None)
            .unwrap_or_else(|error| panic!("应可构造: {error}"));
        let failed_data = failed
            .data
            .as_ref()
            .and_then(|value| value.as_object())
            .and_then(|object| object.get("accepted"))
            .cloned()
            .unwrap_or_else(|| panic!("accepted 字段必须在 data 里"));
        assert_eq!(
            failed_data,
            serde_json::json!(false),
            "失败出口 accepted 必须为 false"
        );

        let waiting = response(true, "本轮未处理：缺少必填字段".to_string(), None)
            .unwrap_or_else(|error| panic!("应可构造: {error}"));
        let waiting_data = waiting
            .data
            .as_ref()
            .and_then(|value| value.as_object())
            .and_then(|object| object.get("accepted"))
            .cloned()
            .unwrap_or_else(|| panic!("accepted 字段必须在 data 里"));
        assert_eq!(
            waiting_data,
            serde_json::json!(true),
            "等待出口 accepted 必须为 true"
        );
    }

    #[test]
    fn dispatch_outcome_success_forwards_the_accepted_flag() {
        // 回归的第二道防线：即使 response 本身正确，DispatchOutcome::success 若仍硬编码
        // `true` 也会在装配层把失败翻成成功——两处必须各自守住。
        let outcome =
            DispatchOutcome::success(Outcome::Failed, Some(1), false, "失败".to_string(), None);
        let response = outcome
            .response
            .unwrap_or_else(|error| panic!("应可构造: {error}"));
        let data = response
            .data
            .as_ref()
            .and_then(|value| value.as_object())
            .and_then(|object| object.get("accepted"))
            .cloned()
            .unwrap_or_else(|| panic!("accepted 字段必须在 data 里"));
        assert_eq!(
            data,
            serde_json::json!(false),
            "DispatchOutcome::success 必须透传 accepted=false"
        );
    }

    // -----------------------------------------------------------------------
    // 端到端 mock 实测
    // -----------------------------------------------------------------------
    //
    // 这一组用例用**合成**的审批定义 + **合成**的多维表格列，把整条默认链路真的走
    // 一遍：列出字段 → 取审批定义 → 按名匹配 → 读记录 → 重映射 → 组装 form →
    // 创建实例 → 取编号 → 回写。
    //
    // # 为什么必须有这一组
    //
    // 各模块的单测都是「喂一个函数」——它们证明每一段是对的，证明不了**接起来**是对的。
    // 这条链路上真正的风险全在接缝处，而且是静默的那一类：响应 `fields` 按列名作键
    // 而配置存 `field_id`（对不上的症状是「每条记录都缺少申请人」，不报错）、
    // `form` 必须是压缩后的 JSON **字符串**而不是对象、单选传字符串而多选传数组。
    // 这些只有把整条链跑通、再把**发给飞书的请求体**逐字段对一遍才验得到。
    //
    // # 唯一的替身是网络与数据库
    //
    // 传输换成 `ReplayTransport`（回放脚本化响应 + 记录每个真实请求体），数据库换成
    // 惰性连接池（这些用例根本不查库——`load_external_options` 在没有链接型控件时
    // 会短路返回空表）。其余全是生产代码，包括 `derive_uuid`、`rekey_cells_by_field_id`、
    // `BitableBackfill`、`build_form`。
    //
    // # 合成数据不是随手编的
    //
    // 每份响应都按**实测过的真实形状**造：`form` 是 JSON 字符串、`option` 的多种
    // 形态各出现一次、`property.options[].name` 才是选项所在、`batch_get` 的
    // `fields` 按列名作键。依据见 `approval_match` 模块文档。

    use crate::addon::feishu::domain::approval_convert::Converter;
    use crate::addon::feishu::domain::approval_dispatch::DispatchResult;
    use crate::addon::feishu::domain::approval_uuid::derive_uuid;
    use crate::addon::feishu::domain::outbound::{
        OutboundRequest, OutboundResponse, OutboundTransport, Sleeper,
    };
    use crate::addon::feishu::domain::tenant_token::{
        FeishuCredentials, TenantTokenCache, TenantTokenProvider,
    };
    use serde_json::{json, Value};
    use std::sync::Mutex;

    const E2E_BASE_TOKEN: &str = "appE2ETestToken";
    const E2E_TABLE_ID: &str = "tblE2ETestTable";
    const E2E_APPROVAL_CODE: &str = "CODE-E2E-TEST";
    const E2E_RECORD_ID: &str = "recE2E001";
    const E2E_APPLICANT_OPEN_ID: &str = "ou_applicant_e2e";
    /// `2026-09-28T00:00:00+08:00` 的毫秒时间戳。日期控件的输入形态就是这个。
    const E2E_PAY_DATE_MILLIS: i64 = 1_790_524_800_000;

    /// 一个被记录下来的出站请求。
    ///
    /// 记的是**完整请求体**而不只是方法与 URL：端到端要验的正是「发给飞书的那份
    /// JSON 对不对」（`form` 里的控件值、`batch_update` 的单元格形态），
    /// 只看 URL 等于什么都没验。
    #[derive(Debug, Clone)]
    struct SeenRequest {
        method: String,
        url: String,
        body: Option<Value>,
    }

    /// 回放式传输：按脚本依次吐响应，并把每个请求原样记下来。
    struct ReplayTransport {
        script: Mutex<std::collections::VecDeque<(u16, String)>>,
        seen: Mutex<Vec<SeenRequest>>,
    }

    impl ReplayTransport {
        fn new(responses: Vec<(u16, String)>) -> Self {
            Self {
                script: Mutex::new(responses.into_iter().collect()),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn seen(&self) -> Vec<SeenRequest> {
            self.seen
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default()
        }

        /// 按「方法 + 完整 URL」精确定位一次请求。
        ///
        /// 用完整 URL 而不是子串：创建实例与取实例详情的前缀相同
        /// （`/approval/v4/instances` 与 `/approval/v4/instances/:id`），子串匹配会把
        /// 两者混起来，而「创建请求体对不对」正是本组用例的核心断言。
        fn took(&self, method: &str, url: &str) -> SeenRequest {
            self.seen()
                .into_iter()
                .find(|request| request.method == method && request.url == url)
                .unwrap_or_else(|| {
                    panic!(
                        "必须向 {method} {url} 发过一次请求；实际收到：{:#?}",
                        self.seen()
                    )
                })
        }
    }

    #[async_trait::async_trait]
    impl OutboundTransport for ReplayTransport {
        async fn send(&self, request: OutboundRequest) -> Result<OutboundResponse, BaseError> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(SeenRequest {
                    method: format!("{:?}", request.method),
                    url: request.url.clone(),
                    body: request.json_body.clone(),
                });
            }
            let mut script = self
                .script
                .lock()
                .map_err(|error| BaseError::ConfigError(error.to_string()))?;
            let (status, body) = script.pop_front().ok_or_else(|| {
                BaseError::ConfigError(format!(
                    "回放脚本已耗尽，但代码又发了第 {} 个请求：{:?} {}",
                    self.seen().len() + 1,
                    request.method,
                    request.url
                ))
            })?;
            Ok(OutboundResponse {
                status,
                body,
                headers: std::collections::BTreeMap::new(),
            })
        }
    }

    struct NoSleep;

    #[async_trait::async_trait]
    impl Sleeper for NoSleep {
        async fn sleep(&self, _duration: std::time::Duration) {}
    }

    /// 恒命中的假 token 缓存——回放用例不该把配额花在换 token 上。
    ///
    /// 用 `TenantTokenProvider` 的真实构造而不是伪造它：它不是 trait，而是带
    /// 缓存/锁逻辑的结构体，伪造它会绕开「token 失效补救」那段真实逻辑。
    struct FakeCache;

    #[async_trait::async_trait]
    impl TenantTokenCache for FakeCache {
        async fn get(&self) -> anyhow::Result<Option<String>> {
            Ok(Some("t-e2e".to_string()))
        }
        async fn put(&self, _token: &str, _ttl_seconds: i64) -> anyhow::Result<()> {
            Ok(())
        }
        async fn invalidate(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn acquire_lock(&self, _owner: &str, _ttl_seconds: i64) -> anyhow::Result<bool> {
            Ok(true)
        }
        async fn release_lock(&self, _owner: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// token provider 只在缓存未命中时才会用它，而 `FakeCache` 恒命中。
    struct NeverCalledTransport;

    #[async_trait::async_trait]
    impl OutboundTransport for NeverCalledTransport {
        async fn send(&self, _request: OutboundRequest) -> Result<OutboundResponse, BaseError> {
            Err(BaseError::ConfigError(
                "FakeCache 恒命中，不应走到换 token 那一步".to_string(),
            ))
        }
    }

    fn e2e_tokens() -> TenantTokenProvider {
        TenantTokenProvider::new(
            Arc::new(FakeCache),
            Arc::new(NeverCalledTransport),
            Arc::new(NoSleep),
            FeishuCredentials {
                app_id: "cli_e2e".to_string(),
                app_secret: "secret".to_string(),
            },
            "e2e",
        )
        .unwrap_or_else(|error| panic!("应可构造 token provider: {error}"))
    }

    /// 惰性连接池的上下文。这些用例**不查库**——`load_external_options` 在没有
    /// 链接型控件时会短路返回空表，所以不需要真的数据库。
    ///
    /// `connect_lazy` 不建立任何连接，但它要求一个 Tokio 上下文。
    fn e2e_context() -> FeishuContext {
        use crate::addon::feishu::domain::repository::Repository;
        use yang_base::definition::TableSpec;

        let pool = Arc::new(
            sqlx::MySqlPool::connect_lazy("mysql://user:pass@localhost:3306/yang")
                .unwrap_or_else(|error| panic!("惰性连接池应可构造: {error}")),
        );
        let definition = |spec: Result<TableSpec, _>| {
            spec.unwrap_or_else(|error| panic!("{error}"))
                .table_definition()
                .unwrap_or_else(|error| panic!("应可编译为表定义: {error}"))
        };
        let repository =
            |spec: Result<TableSpec, _>| Repository::new(definition(spec), Arc::clone(&pool));
        FeishuContext::new(
            repository(crate::addon::feishu::datasource::table::table_spec()),
            repository(crate::addon::feishu::datasource::domain::field_table::table_spec()),
            repository(crate::addon::feishu::option::table::table_spec()),
            repository(crate::addon::feishu::approval::table::table_spec()),
            repository(crate::addon::feishu::approval::domain::field_map_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::task_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::request_log_table::table_spec()),
            None,
        )
    }

    fn e2e_coordinates() -> BitableCoordinates {
        BitableCoordinates {
            app_token: E2E_BASE_TOKEN.to_string(),
            table_id: E2E_TABLE_ID.to_string(),
            view_id: None,
        }
    }

    /// 合成审批定义：6 个控件，覆盖「无 `option` 键 / `option: null` / 固定选项数组 /
    /// 可选控件」几种形态，类型跨 文本 / 数字 / 日期 / 单选 / 多选。
    fn e2e_form() -> Value {
        json!([
            {"id": "w1", "name": "公司名称", "type": "input", "required": true},
            {"id": "w2", "name": "付款金额", "type": "number", "required": true, "option": null},
            {"id": "w3", "name": "付款日期", "type": "date", "required": true},
            {"id": "w4", "name": "收款方类型", "type": "radioV2", "required": true,
             "option": [{"value": "opt-person", "text": "个人"}, {"value": "opt-corp", "text": "企业"}]},
            {"id": "w5", "name": "费用标签", "type": "checkboxV2", "required": false,
             "option": [{"value": "opt-travel", "text": "差旅"}, {"value": "opt-meal", "text": "餐饮"}]},
            {"id": "w6", "name": "备注", "type": "input", "required": false}
        ])
    }

    /// 合成多维表格字段：**列名与控件名严格对应**——这正是默认链路（免人工配映射）
    /// 的前置条件。另外两列（申请人 / 审批编号）是使用方自己的列，与控件名不同名。
    ///
    /// `total` 必须有：`list_all_fields` 末尾的 `assert_converged` 拿它证明快照完整。
    fn e2e_fields_body() -> String {
        json!({
            "code": 0,
            "msg": "success",
            "data": {
                "has_more": false,
                "total": 8,
                "items": [
                    {"field_id": "fldCompany", "field_name": "公司名称", "type": 1, "ui_type": "Text"},
                    {"field_id": "fldAmount", "field_name": "付款金额", "type": 2, "ui_type": "Number"},
                    {"field_id": "fldPayDate", "field_name": "付款日期", "type": 5, "ui_type": "DateTime"},
                    // 选项在 `property.options[].name`——不在字段顶层。少了这一层，
                    // 单选控件的选项一个都派生不出来，而列里明明有选项。
                    {"field_id": "fldPayeeType", "field_name": "收款方类型", "type": 3,
                     "ui_type": "SingleSelect", "property": {"options": [{"name": "个人"}, {"name": "企业"}]}},
                    {"field_id": "fldTags", "field_name": "费用标签", "type": 4,
                     "ui_type": "MultiSelect", "property": {"options": [{"name": "差旅"}, {"name": "餐饮"}]}},
                    {"field_id": "fldRemark", "field_name": "备注", "type": 1, "ui_type": "Text"},
                    {"field_id": "fldApplicant", "field_name": "申请人", "type": 11, "ui_type": "User"},
                    {"field_id": "fldSerial", "field_name": "审批编号", "type": 1, "ui_type": "Text"}
                ]
            }
        })
        .to_string()
    }

    /// 合成审批定义响应。
    ///
    /// `form` 是 **JSON 字符串**（真实形态是字符串里再套一层 JSON 数组），
    /// 且**不返回 `is_external`**——实测 `approvals get` 就是没有这一位，缺字段时
    /// 必须落到「非三方定义」。这里刻意不给，把那个默认值钉住。
    fn e2e_definition_body(form: &Value) -> String {
        json!({
            "code": 0,
            "msg": "success",
            "data": {
                "approval_name": "端到端测试审批",
                "form": serde_json::to_string(form).unwrap_or_else(|error| panic!("{error}"))
            }
        })
        .to_string()
    }

    /// 合成 `records/batch_get` 响应。
    ///
    /// **`fields` 按列名作键**——这是实测出来的真实形态，也是整条链路上最容易静默
    /// 出错的一处：按 `field_id` 去查这个 map 永远查不到，症状是每条记录都
    /// 「缺少申请人」而不报任何错。
    fn e2e_record_body() -> String {
        json!({
            "code": 0,
            "msg": "success",
            "data": {
                "records": [{
                    "record_id": E2E_RECORD_ID,
                    "fields": {
                        "公司名称": "成都某某科技有限公司",
                        "付款金额": 1234.56,
                        "付款日期": E2E_PAY_DATE_MILLIS,
                        "收款方类型": "个人",
                        "费用标签": ["差旅", "餐饮"],
                        // 可选控件留空：整个控件 JSON 都不该出现在 form 里。
                        "备注": "",
                        "申请人": [{"id": E2E_APPLICANT_OPEN_ID}],
                        // 回填列此刻是空的——它正是「没有审批编号」的判据。
                        "审批编号": ""
                    }
                }]
            }
        })
        .to_string()
    }

    fn e2e_ok_body(data: Value) -> String {
        json!({"code": 0, "msg": "success", "data": data}).to_string()
    }

    /// 端到端（一）：合成定义 + 合成列名 → 自动建配置 → 单条派发 → 回填。
    ///
    /// 断言分两层：**建出来的配置**（列名解析成 id、转换器、选项映射）与
    /// **发给飞书的两个请求体**（`form` 的控件值、`batch_update` 的单元格）。
    /// 后者是这一组用例存在的理由——它逐字段钉住了格式约定。
    #[tokio::test]
    async fn end_to_end_provision_then_dispatch_then_backfill() {
        let transport = Arc::new(ReplayTransport::new(vec![
            // 建配置：先列字段，再取定义。
            (200, e2e_fields_body()),
            (200, e2e_definition_body(&e2e_form())),
            // 派发：`dispatch_single` 会**再列一次**字段（读侧重映射要用）。
            (200, e2e_fields_body()),
            (200, e2e_record_body()),
            (200, e2e_ok_body(json!({"instance_code": "INST-E2E-001"}))),
            (
                200,
                e2e_ok_body(json!({
                    "instance_code": "INST-E2E-001",
                    "serial_number": "202609280001",
                    "status": "PENDING"
                })),
            ),
            (200, e2e_ok_body(json!({}))),
        ]));
        let tokens = e2e_tokens();
        let sleeper = NoSleep;
        let context = e2e_context();

        // ---- 第一段：自动建配置 ----
        let plan = build_plan(
            transport.as_ref(),
            &sleeper,
            &tokens,
            &context,
            &ProvisionInput {
                base_token: E2E_BASE_TOKEN,
                table_id: E2E_TABLE_ID,
                approval_code: E2E_APPROVAL_CODE,
                // 调用方写的是**列名**（工作流里人就这么写），要解析成 id 存起来。
                applicant_field: "申请人",
                backfill_field: "审批编号",
                base_timezone: "Asia/Shanghai",
            },
        )
        .await
        .unwrap_or_else(|error| panic!("自动建配置应当成功: {error}"));

        assert_eq!(
            plan.applicant_field_id, "fldApplicant",
            "存的是 field_id 而不是列名——改列名不该让配置失效"
        );
        assert_eq!(plan.backfill_field_id, "fldSerial");
        assert_eq!(plan.approval_name, "端到端测试审批");
        assert_eq!(plan.widgets.len(), 6, "6 个控件都该按名匹配上");

        let widget = |id: &str| {
            plan.widgets
                .iter()
                .find(|widget| widget.widget_id == id)
                .unwrap_or_else(|| panic!("控件 {id} 未进映射：{:#?}", plan.widgets))
        };
        // 转换器在**配置期**就定好，运行期不再按 type 推断——少一处会漂移的地方。
        assert_eq!(widget("w1").converter, Converter::Direct);
        assert_eq!(
            widget("w3").converter,
            Converter::Date,
            "date 控件要折成 RFC3339"
        );
        assert_eq!(widget("w4").converter, Converter::Option);
        assert_eq!(widget("w5").converter, Converter::Option);
        assert_eq!(widget("w4").bitable_field, "fldPayeeType");
        assert_eq!(
            widget("w4").option_map.get("个人").map(String::as_str),
            Some("opt-person"),
            "选项映射派生的是**审批控件的 option value**，不是列表里的文案"
        );
        assert_eq!(
            widget("w5").option_map.get("餐饮").map(String::as_str),
            Some("opt-meal")
        );

        // ---- 第二段：派发 ----
        //
        // 复刻 `dispatch_single` 的接线（它本身要 `ActionContext` 才能读配置行，而配置
        // 行在库里，mock 用例够不着）。这里少掉的只有「从库里读配置行」，
        // 其余每一步都是生产函数。
        let coordinates = e2e_coordinates();
        let fields = bitable::list_all_fields(transport.as_ref(), &sleeper, &tokens, &coordinates)
            .await
            .unwrap_or_else(|failure| panic!("列出字段应成功: {}", failure.message));
        let records = bitable::get_records_by_ids(
            transport.as_ref(),
            &sleeper,
            &tokens,
            &coordinates,
            &[E2E_RECORD_ID.to_string()],
        )
        .await
        .unwrap_or_else(|failure| panic!("取记录应成功: {}", failure.message));
        let cells = bitable::rekey_cells_by_field_id(
            &fields,
            &records
                .first()
                .unwrap_or_else(|| panic!("应取回一条记录"))
                .fields,
        )
        .unwrap_or_else(|error| panic!("按 id 重映射应成功: {error}"));
        assert!(
            cells.contains_key("fldApplicant"),
            "重映射后必须能按 field_id 取到申请人，否则会静默停在「缺少申请人」"
        );

        // 与 `dispatch_single` 同一步：配置里存的是 `field_id`，而写接口按列名作键。
        // 这里刻意走**同一个**解析器（而不是直接写死字面量），否则「配置里存的到底是
        // id 还是名」这条接线断了也没人发现。
        let backfill_field_name = bitable::resolve_field_name(&fields, &plan.backfill_field_id)
            .unwrap_or_else(|error| panic!("回填列应可解析成当前列名: {error}"));
        assert_eq!(backfill_field_name, "审批编号");

        let backfill = BitableBackfill {
            transport: transport.as_ref(),
            sleeper: &sleeper,
            tokens: &tokens,
            coordinates: &coordinates,
        };
        let result = dispatch_one(
            transport.as_ref(),
            &sleeper,
            &tokens,
            &backfill,
            &OrchestrationInput {
                coordinates: &coordinates,
                record_id: E2E_RECORD_ID,
                cells: &cells,
                applicant_field: &plan.applicant_field_id,
                backfill_field_name: &backfill_field_name,
                approval_code: E2E_APPROVAL_CODE,
                widgets: &plan.widgets,
                timezone_offset: FixedOffset::from_iana("Asia/Shanghai")
                    .unwrap_or_else(|error| panic!("{error}")),
            },
        )
        .await;
        assert_eq!(
            result,
            DispatchResult::Backfilled {
                serial_number: "202609280001".to_string()
            },
            "整条链应跑通并回填编号"
        );

        // ---- 第三段：逐字段核对发给飞书的请求体 ----

        let create = transport.took(
            "Post",
            &crate::addon::feishu::domain::approval::create_instance_url(),
        );
        let body = create
            .body
            .unwrap_or_else(|| panic!("创建实例请求必须带 body"));
        assert_eq!(body["approval_code"], json!(E2E_APPROVAL_CODE));
        assert_eq!(
            body["open_id"],
            json!(E2E_APPLICANT_OPEN_ID),
            "发起人取的是申请人列里的第一个 open_id"
        );
        assert_eq!(
            body["uuid"],
            json!(derive_uuid(
                E2E_BASE_TOKEN,
                E2E_TABLE_ID,
                E2E_APPROVAL_CODE,
                E2E_RECORD_ID
            )),
            "uuid 必须由 (表格坐标, 定义, 记录) 派生——它是唯一的服务端幂等键"
        );

        // `form` 是**压缩后的 JSON 数组字符串**，不是 JSON 对象。
        let form_text = body["form"]
            .as_str()
            .unwrap_or_else(|| panic!("form 必须是字符串，实际是 {}", body["form"]));
        let form: Value = serde_json::from_str(form_text)
            .unwrap_or_else(|error| panic!("form 应是可解析的 JSON: {error}"));
        let item = |id: &str| {
            form.as_array()
                .and_then(|items| items.iter().find(|item| item["id"] == json!(id)))
                .cloned()
        };
        assert_eq!(
            item("w1").unwrap_or_else(|| panic!("w1 应在 form 里：{form}"))["value"],
            json!("成都某某科技有限公司")
        );
        assert!(
            item("w2").unwrap_or_else(|| panic!("w2 应在 form 里：{form}"))["value"].is_number(),
            "数字控件必须传 JSON 数字；传字符串会被 1390001 拒掉整单"
        );
        assert_eq!(
            item("w3").unwrap_or_else(|| panic!("w3 应在 form 里：{form}"))["value"],
            json!("2026-09-28T00:00:00+08:00"),
            "毫秒时间戳要按时区折成带偏移量的 RFC3339"
        );
        assert_eq!(
            item("w4").unwrap_or_else(|| panic!("w4 应在 form 里：{form}"))["value"],
            json!("opt-person"),
            "单选控件传**单个字符串**"
        );
        assert_eq!(
            item("w5").unwrap_or_else(|| panic!("w5 应在 form 里：{form}"))["value"],
            json!(["opt-travel", "opt-meal"]),
            "多选控件传数组，且值是 option value"
        );
        assert!(
            item("w6").is_none(),
            "可选控件在空值时**整个 JSON 都不能传**——传了就必须给 value，否则接口报错"
        );

        let update = transport.took(
            "Post",
            &bitable::batch_update_records_url(E2E_BASE_TOKEN, E2E_TABLE_ID)
                .unwrap_or_else(|error| panic!("{error}")),
        );
        assert_eq!(
            update.body.unwrap_or_else(|| panic!("回写必须带 body")),
            json!({
                "records": [{
                    "record_id": E2E_RECORD_ID,
                    "fields": {"审批编号": "202609280001"}
                }]
            }),
            "回写的是编号文本，键是回填列的**列名**（batch_update 按名作键）"
        );
    }

    /// 端到端（二）：配置校验不过时，原因要**点名**，并且真的写进多维表格。
    ///
    /// 「配置期就报错」这句话的落点是回填列而不是 HTTP 响应：点按钮的人在表格里，
    /// 而工作流的 `response_value` 只有 `accepted` / `message` / `serial_number`
    /// 三个字段，页面按钮连这三个都拿不到。所以这条用例验两件事——**报什么**与
    /// **写哪儿**。
    #[tokio::test]
    async fn end_to_end_config_failure_is_written_back_to_the_table() {
        // 最常见的配置错误：定义里有个必填控件「部门」，而表里没有同名列。
        let form = json!([
            {"id": "w1", "name": "公司名称", "type": "input", "required": true},
            {"id": "w9", "name": "部门", "type": "input", "required": true}
        ]);
        let transport = Arc::new(ReplayTransport::new(vec![
            (200, e2e_fields_body()),
            (200, e2e_definition_body(&form)),
            (200, e2e_ok_body(json!({}))),
        ]));
        let tokens = e2e_tokens();
        let sleeper = NoSleep;
        let context = e2e_context();

        let failure = build_plan(
            transport.as_ref(),
            &sleeper,
            &tokens,
            &context,
            &ProvisionInput {
                base_token: E2E_BASE_TOKEN,
                table_id: E2E_TABLE_ID,
                approval_code: E2E_APPROVAL_CODE,
                applicant_field: "申请人",
                backfill_field: "审批编号",
                base_timezone: "Asia/Shanghai",
            },
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("缺必填列必须建不出配置"));

        let ProvisionError::Invalid(reasons) = &failure else {
            panic!("应是校验失败（可修），实际是 {failure:?}");
        };
        assert!(
            reasons.iter().any(|reason| reason.contains("部门")),
            "原因必须点名是哪个控件，否则使用方无从下手：{reasons:?}"
        );

        // 写回回填列——走的是生产代码里 `backfill_provision_failure` 调用的同一个函数。
        let coordinates = e2e_coordinates();
        let backfill = BitableBackfill {
            transport: transport.as_ref(),
            sleeper: &sleeper,
            tokens: &tokens,
            coordinates: &coordinates,
        };
        write_config_failure(&backfill, E2E_RECORD_ID, "审批编号", &failure)
            .await
            .unwrap_or_else(|error| panic!("回填应成功: {}", error.message));

        let update = transport.took(
            "Post",
            &bitable::batch_update_records_url(E2E_BASE_TOKEN, E2E_TABLE_ID)
                .unwrap_or_else(|error| panic!("{error}")),
        );
        let cell = update.body.unwrap_or_else(|| panic!("回写必须带 body"));
        let text = cell["records"][0]["fields"]["审批编号"]
            .as_str()
            .unwrap_or_else(|| panic!("写进单元格的应是文本：{cell}"))
            .to_string();
        assert!(
            text.starts_with("[配置]"),
            "必须带 `[配置]` 前缀，否则表格里的人分不开「去补数据」与「去改配置」：{text}"
        );
        assert!(
            text.contains("部门"),
            "写进表格的原因同样要点名缺的是哪个控件：{text}"
        );

        // 校验不过时**一个字节都不该落库**：配置与映射必须同生同死，半套配置
        // （有配置没映射）会让之后每一次派发都卡在「该配置没有字段映射」，
        // 而配置行看着是好的。所以这里只该有「列字段 → 取定义 → 写错误」三次调用。
        assert_eq!(
            transport.seen().len(),
            3,
            "校验不过时不该再发别的请求（尤其不该有创建实例）：{:#?}",
            transport.seen()
        );
    }
}

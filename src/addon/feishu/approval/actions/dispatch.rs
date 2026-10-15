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
//!   单条完成后结果会回写 `feishu_approval_task`（与 worker 同口径）——
//!   worker 因此不再把同一条记录完整重跑一遍（见 `record_dispatch_task`）。
//! - 全表可能有几十到几百条，受创建接口 **100 次/分钟** 限制，同步必然超时。
//!
//! **两种 button_type 的输出能力不同**：`buttonField` 能引用记录的字段与属性
//! （所以传得出 `record_id`），`buttonElement` **仅基础触发属性**——这是工作流侧的
//! 硬约束，不是本端点的设计选择。
//!
//! # 鉴权
//!
//! **每条配置一个 key + 一个专属 Token**（与外部数据源同一套，2026-10-13 起）：
//! 路由是 `POST /api/v1/feishu/approval/dispatch/{config_key}`，Action `public`
//! （飞书工作流没有 JWT），handler 内按 `config_key` 查配置行、用请求体 `token`
//! 的 sha256 摘要比对 `token_hash`（`verify_token` 常数时间比较），**先验 Token
//! 再报停用**——与 datasource 的 `verify_source` 同一条不变量。
//!
//! 旧形态（全局单值 `management_api_token` + 请求体 `(base_token, table_id)`
//! 白名单 + 三件套自动建配置）已废弃：管理 Token 是全局单值、不按配置绑定，
//! 复用它等于把「以任意用户身份创建审批实例」开放给所有持 Token 者（设计 §9.3
//! 记录的决策，本端点执行之）。`config_key` 本身就是路由与白名单，Token 是凭据，
//! 两者合一取代了那两道分散的防线。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::approval::domain::request_log_writer::{
    normalized_requested_by, outcome_for, Outcome, RequestLog,
};
use crate::addon::feishu::domain::approval::{resolve_applicant, ApplicantResolution};
use crate::addon::feishu::domain::approval_convert::FixedOffset;
use crate::addon::feishu::domain::approval_dispatch::{
    applicant_open_id, dispatch_one, widget_maps_from_rows, BitableBackfill,
    DispatchInput as OrchestrationInput, DispatchResult as OrchestrationResult,
};
use crate::addon::feishu::domain::approval_feedback::{self, FeedbackStatus};
use crate::addon::feishu::domain::approval_notification::{
    self, FeedbackReference, NotificationRecipient, NotificationResult, NotificationStatus,
    ReceiveIdType,
};
use crate::addon::feishu::domain::approval_uuid::derive_uuid;
use crate::addon::feishu::domain::bitable::{self, BitableCoordinates};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound::{
    FailureKind, OutboundFailure, OutboundTransport, Sleeper,
};
use crate::addon::feishu::domain::outbound_setup;
use crate::addon::feishu::domain::tenant_token::FeishuCredentials;
use crate::addon::feishu::domain::token::verify_token;
use crate::addon::feishu::domain::user_token::user_access_token;
use crate::config::FeishuSettings;
use crate::feishu_approval_worker::ApprovalDispatchHandle;
// `Backfill` / `ProvisionError` 只被 `write_config_failure` 与测试用到，而那个函数
// 现在是 `#[cfg(test)]`（生产调用点随自动建配置一起删了）。
#[cfg(test)]
use crate::addon::feishu::domain::approval_dispatch::Backfill;
#[cfg(test)]
use crate::addon::feishu::domain::approval_provision::{
    build_plan, insert_plan, ProvisionError, ProvisionInput,
};

/// 发起人姓名解析失败（未命中 / 重名多命中 / 通讯录出站失败）的业务码。
///
/// 与 `option/actions/approval_options.rs` 的 `codes` 模块同域不重号。
const APPLICANT_UNRESOLVED: i32 = 40005;

/// 配置不存在：`config_key` 没有对应的已启用配置行（40401）。
///
/// key 本身就是白名单——没命中即不存在，不存在即拒绝；不再有「请求体带坐标」的
/// 第二种定位方式（旧形态废弃，见模块文档）。
const CONFIG_NOT_FOUND: i32 = 40401;

/// 该配置尚未签发访问 Token（`token_hash` 为空；40402）。
///
/// 存量行在升级后处于这个状态：四列是新增的可空列（框架对「已有数据的表加必填列」
/// 拒绝启动），签发在控制台做，这里只负责可行动地拒。
const TOKEN_NOT_ISSUED: i32 = 40402;

/// 访问 Token 校验失败：请求体 `token` 与该配置的摘要不符（40301）。
const TOKEN_MISMATCH: i32 = 40301;

/// 该配置已停用（`enabled=false`；40302）。
const CONFIG_DISABLED: i32 = 40302;

/// 用户凭证缺通讯录权限（飞书对 `user_access_token` 的权限不足码 99991679）。
///
/// 与 99991672（应用级缺 scope）分开：那一条是「后台还没开通」，这一条是
/// 「用户授权时没给」，处置是让管理员**重新授权**而不是等审核。
const CODE_USER_TOKEN_SCOPE_MISSING: i32 = 99991679;

/// 用户凭证形态非法（请求里没带 / 类型错），重新授权才能修。
const CODES_USER_TOKEN_MALFORMED: &[i32] = &[99991668, 99991671];

/// 通讯录 scope 未开通的错误码（真机实测 99991672）。
///
/// `outbound::fatal_hint` 未收录它（那是共享文件的错误码登记点，本批不改），
/// 而这是本期最可能的真实失败——scope 正在等审核，所以在这里点名。
const CODE_CONTACT_SCOPE_MISSING: i32 = 99991672;

/// 派发端点的输入契约。
///
/// `Serialize` 是请求记录落库需要的：`request_body` 列存的就是本结构的序列化
/// 原文（见 `domain/request_log_writer.rs`）。
///
/// # 为什么没有 `base_token` / `table_id` / `approval_code` 等配置字段
///
/// 配置只在控制台建（`create_config` 向导）。请求体只带运行时参数：`token`（鉴权）、
/// `record_id`（可选）、`applicant`（可选）、`requested_by`（可选）。工作流不再
/// 携带任何配置数据——那是本次契约改形的核心（见模块文档）。
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct DispatchInput {
    /// 访问 Token：与 URL 里 `config_key` 那行配置的 `token_hash` 摘要比对。
    /// 由控制台「签发/轮换 Token」产出，工作流 `raw_body` 里填。
    pub(super) token: String,
    /// 单条处理时的记录 id；省略表示处理全表待处理记录。
    #[serde(default)]
    pub(super) record_id: Option<String>,
    /// 请求人（工作流触发人）。工作流模板在 `raw_body` 里带 `$.step_btn.user`；
    /// 未带或 trim 后为空时落库统一记 `feishu-workflow`。仅记录用，不影响派发。
    #[serde(default)]
    pub(super) requested_by: Option<String>,
    /// 发起人（工作流触发人）。真机 `$.step_btn.user` 渲染成**姓名**（设计 M11 定论），
    /// 所以这里是**姓名**，由本端点经 contact 搜索解析成 open_id 后使用（§6.2）；
    /// 解析失败一律 fail（可行动文案），不落等待态。传了才走新路径并回写
    /// applicant_field 列（D2 缺省回退旧行为）。
    #[serde(default)]
    pub(super) applicant: Option<String>,
    /// 明确指定通知接收人的 ID 类型与 ID/邮箱；不从姓名或申请人推断。
    #[serde(default)]
    pub(super) notification_recipient: Option<NotificationRecipient>,
}

impl ParamInput for DispatchInput {
    fn params() -> Params {
        Params::new()
    }
}

impl DispatchInput {
    /// 进入处理前的入参校验。
    ///
    /// 只剩三件运行时参数：`token` 必填（鉴权），`record_id` 传了必须非空，
    /// `requested_by` 长度受控（列上限 128 字符）。
    fn validate(&self) -> Result<(), BaseError> {
        if self.token.trim().is_empty() {
            return Err(BaseError::ParamInvalid(
                "token".to_string(),
                "不能为空".to_string(),
            ));
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
        Ok(())
    }
}

/// URL 路径参数 `config_key` 的校验：进 `where_eq` 前 trim；空值直接拒。
///
/// 抽成纯函数是为了让「空 key 必须在查库之前被拒」可离线钉住。
fn validate_config_key(raw: &str) -> Result<String, BaseError> {
    let key = raw.trim().to_string();
    if key.is_empty() {
        return Err(BaseError::ParamInvalid(
            "config_key".to_string(),
            "不能为空".to_string(),
        ));
    }
    Ok(key)
}

/// 从路径参数取 `config_key`。
///
/// 取不到（路由没带参数）按内部错误折算成失败记录——空值也会被
/// `validate_config_key` 在查库前拒掉，这里只负责「有没有给」。
fn resolve_config_key(ctx: &ActionContext) -> Result<String, BaseError> {
    ctx.request
        .get_path_param("config_key")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            BaseError::ParamInvalid("config_key".to_string(), "缺少派发配置标识".to_string())
        })
}

/// 鉴权判定（纯函数）：按已读出的配置行校验访问 Token 与启用位。
///
/// 抽成纯函数是为了能被单测直接驱动——四类出口（未签发 / Token 不符 / 停用 /
/// 通过）的**顺序**必须钉住：先验 Token 再报停用，与 datasource 的
/// `verify_source` 同一条不变量。
// `DispatchOutcome` 载着整个 `Result<ApiResponse, BaseError>`，装箱只是为一个
// perf lint 把 7 处构造点全改成 `Box::new`；本函数每次请求只调一次，收益为零。
// 照 `bootstrap.rs` 对 `too_many_arguments` 的同款处置。
#[allow(clippy::result_large_err)]
fn check_config_auth(
    row: &yang_base::table::Record,
    presented_token: &str,
) -> Result<(i64, String, String), DispatchOutcome> {
    let config_id: i64 = row.require("id").map_err(|error| {
        DispatchOutcome::internal(None, BaseError::ConfigError(error.to_string()))
    })?;
    // `token_hash` 可空（存量行在升级后处于「未签发」状态）。**必须在 Token
    // 校验之前**判：没有摘要时 `verify_token` 会把任何出示都拒掉，那条消息
    // 会把「还没签发」误导成「Token 错了」。
    let stored_hash: Option<String> = row.optional("token_hash").map_err(|error| {
        DispatchOutcome::internal(Some(config_id), BaseError::ConfigError(error.to_string()))
    })?;
    let Some(stored_hash) = stored_hash
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Err(DispatchOutcome::fail(
            TOKEN_NOT_ISSUED,
            Some(config_id),
            "该配置尚未签发访问 Token，请在控制台「审批配置」页签发后再调用派发",
        ));
    };
    if !verify_token(presented_token.trim(), &stored_hash) {
        return Err(DispatchOutcome::fail(
            TOKEN_MISMATCH,
            Some(config_id),
            "访问 Token 无效，请在控制台重新签发后更新工作流",
        ));
    }
    let enabled: bool = row
        .optional("enabled")
        .map_err(|error| {
            DispatchOutcome::internal(Some(config_id), BaseError::ConfigError(error.to_string()))
        })?
        .unwrap_or(false);
    if !enabled {
        return Err(DispatchOutcome::fail(
            CONFIG_DISABLED,
            Some(config_id),
            "该审批派发配置已停用，请在控制台启用后再调用",
        ));
    }
    let base_token: String = row.require("base_token").map_err(|error| {
        DispatchOutcome::internal(Some(config_id), BaseError::ConfigError(error.to_string()))
    })?;
    let table_id: String = row.require("table_id").map_err(|error| {
        DispatchOutcome::internal(Some(config_id), BaseError::ConfigError(error.to_string()))
    })?;
    Ok((config_id, base_token, table_id))
}

/// 把「配置校验不过」写进回填列。
///
/// # 为什么单独抽出来
///
/// 这是「配置期报错」**唯一的用户可见出口**：HTTP 响应只有工作流日志看得到，而
/// 点按钮的人在多维表格里。整条链路——文案前缀、写哪一列、单元格形态——都只有在
/// 真的写一次之后才知道对不对，所以它必须能被端到端用例直接驱动。
///
/// # 为什么保留（尽管 dispatch 的自动建配置入口已删）
///
/// `create_config`（控制台建配置）仍复用同一条 `build_plan`/`insert_plan` 校验链路
/// （设计 §5.1：手动建与自动建必须给同一套保证）。建配置失败时把原因写回回填列
/// 的这个落点，正是那条链路的唯一用户可见出口——所以这个函数与它的端到端测试
/// 必须留着，只是**调用方从 dispatch 的自动建配置换成了 create_config 的手动建**。
#[cfg(test)]
async fn write_config_failure(
    backfill: &dyn Backfill,
    record_id: &str,
    backfill_field: &str,
    failure: &ProvisionError,
) -> Result<(), OutboundFailure> {
    backfill
        .write(
            record_id,
            backfill_field,
            &serde_json::json!(format!("[配置] {failure}")),
        )
        .await
}

/// 派发结果。
///
/// **注意**：`notification` 是嵌套对象，打破了原有的「扁平」不变量。工作流仍然只引用
/// `accepted`/`serial_number`/`message` 等标量字段，`notification` 和 `status` 是给
/// HTTP 调用方直接读的（不通过工作流 response_value 引用）。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct DispatchResultBody {
    /// 是否成功（单条）或是否受理（全表）。
    accepted: bool,
    /// 反馈状态（`FeedbackStatus` 的字符串形态，如 `"succeeded"`, `"waiting"`, `"failed"` 等）。
    status: String,
    /// 结果说明。单条失败时是**可行动的原因**，不是原始响应体。
    message: String,
    /// 单条成功时的审批单编号。
    #[serde(skip_serializing_if = "Option::is_none")]
    serial_number: Option<String>,
    /// 通知结果（嵌套对象，HTTP 调用方可读；工作流不引用此字段）。
    notification: serde_json::Value,
}

/// 注册派发端点。
///
/// 路由带 `{config_key}` 路径参数（与外部数据源的 `{source_key}` 同一形态）。
/// **不挂** `ManagementTokenMiddleware`：鉴权在 handler 内按 `config_key` 查到的
/// 那一行的 `token_hash` 做摘要比对（见 `authenticate_config`），Token 从此按配置
/// 隔离而不是全局单值。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("dispatch_approval"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/dispatch/{config_key}",
        )
        .display_name("创建飞书审批实例")
        .description("多维表格按钮入口：按 config_key 定位配置并校验访问 Token，单条同步创建或受理全表待处理记录")
        // public：飞书工作流直连，不经框架 JWT；按配置签发的访问 Token 在 handler 内校验。
        .public()
        .register()
}

/// 一次派发请求的结果记录事实：所有出口（含 `validate()` 失败）都收敛成它，
/// 外层**唯一一处**组装响应并落库——散落的出口各写一遍必漏。
///
/// `Debug` 是测试断言需要的（`{outcome:?}` 在鉴权不变量测试里用它打印诊断）。
#[derive(Debug)]
struct DispatchOutcome {
    /// 结果四桶。
    outcome: Outcome,
    /// 反馈状态。
    status: FeedbackStatus,
    /// 结果说明（复用响应 message；失败时为可行动原因）。
    message: String,
    /// 单条成功时的审批单编号。
    serial_number: Option<String>,
    /// 仅在创建或按原 UUID 恢复实例得到确认后提供。
    instance_code: Option<String>,
    /// 鉴权通过后关联；鉴权前失败的请求为空。
    config_id: Option<i64>,
    /// 配置行里的多维表格坐标（鉴权通过后才有）；日志表 `base_token`/`table_id`
    /// 必填列的取值来源。旧形态从请求体拿，改形后请求体不再携带配置数据。
    base_token: Option<String>,
    table_id: Option<String>,
    /// 记录摘要（用于通知展示）。
    record_summary: Option<String>,
    /// 通知的兜底接收人：派发时解析出的申请人 open_id（传参优先、回退表格列，
    /// 与 `dispatch_one` 内部的判定同源）。`complete_feedback` 在请求未显式
    /// 指定 `notification_recipient` 时用它兜底——「创建完成就通知触发人」的落点。
    applicant_open_id: Option<String>,
    /// 通知的**最后兜底**接收人：记录创建人 open_id（`automatic_fields` 取回）。
    ///
    /// 申请人列空着时（waiting 的主因）它是唯一还能定位到的触发人——
    /// 「用户发起过请求就必须收到反馈」这条原则不能因为申请人没填而断掉。
    creator_open_id: Option<String>,
    /// 通知结果（默认 `{"status":"not_requested"}`）。
    notification: serde_json::Value,
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
            status: FeedbackStatus::Failed,
            message: message.clone(),
            serial_number: None,
            instance_code: None,
            config_id,
            base_token: None,
            table_id: None,
            record_summary: None,
            applicant_open_id: None,
            creator_open_id: None,
            notification: serde_json::json!({"status":"not_requested"}),
            response: Ok(ApiResponse::fail(code, message)),
        }
    }

    /// 内部错误冒泡出口：`Err(BaseError)` 原样保留，文案用错误的 Display。
    fn internal(config_id: Option<i64>, error: BaseError) -> Self {
        Self {
            outcome: Outcome::Failed,
            status: FeedbackStatus::Failed,
            message: error.to_string(),
            serial_number: None,
            instance_code: None,
            config_id,
            base_token: None,
            table_id: None,
            record_summary: None,
            applicant_open_id: None,
            creator_open_id: None,
            notification: serde_json::json!({"status":"not_requested"}),
            response: Err(error),
        }
    }

    /// 成功 / 等待 / 受理出口（响应走 `success` 通道）。
    ///
    /// 坐标（`base_token` / `table_id`）由调用方从已鉴权的配置行带上——日志表
    /// 这两列必填，改形后请求体不再携带它们。
    #[allow(clippy::too_many_arguments)] // 8 个参数：结果字段逐个平铺，31 个调用点全走它，收结构体只是把字段挪个位置。
    fn success(
        outcome: Outcome,
        config_id: Option<i64>,
        base_token: Option<String>,
        table_id: Option<String>,
        accepted: bool,
        status: FeedbackStatus,
        message: String,
        serial_number: Option<String>,
    ) -> Self {
        Self {
            outcome,
            status,
            message: message.clone(),
            serial_number: serial_number.clone(),
            instance_code: None,
            config_id,
            base_token,
            table_id,
            record_summary: None,
            applicant_open_id: None,
            creator_open_id: None,
            notification: serde_json::json!({"status":"not_requested"}),
            response: response(accepted, status, message, serial_number),
        }
    }
}

/// 处理派发请求。
#[tracing::instrument(name = "approval_request", skip_all, fields(
    config_key = tracing::field::Empty, record_id = tracing::field::Empty
))]
pub(super) async fn handle(
    ctx: ActionContext,
    input: DispatchInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let started = std::time::Instant::now();
    let mode = if input.record_id.is_some() {
        "single"
    } else {
        "all"
    };
    tracing::info!(stage = "request", mode, "收到审批派发请求");
    // 业务主体抽成内部函数、返回结构化结果而不是直接 return：每个出口
    // （含 `validate()` 失败——它也要落一条 failed 记录，且要落在还能拿到
    // `request_body` 的位置）都收敛成一条可落库的记录事实。落库用独立连接
    // （`TableQuery::insert` 不经事务自动提交），写失败只降级日志。
    let mut outcome = match resolve_config_key(&ctx) {
        Err(error) => DispatchOutcome::internal(None, error),
        Ok(config_key) => {
            tracing::Span::current().record("config_key", config_key.as_str());
            tracing::Span::current()
                .record("record_id", input.record_id.as_deref().unwrap_or_default());
            handle_dispatch(&ctx, &input, &context, &config_key)
                .await
                .unwrap_or_else(|error| DispatchOutcome::internal(None, error))
        }
    };
    // 收尾反馈：渲染展示文案 + 发送 IM 通知（若有接收人）。通知失败不影响审批结论。
    // 鉴权前失败的请求（无 config_id）跳过反馈——没有配置行可读。
    if let Some(config_id) = outcome.config_id {
        let base_token = outcome.base_token.clone().unwrap_or_default();
        let table_id = outcome.table_id.clone().unwrap_or_default();
        let record_id = input.record_id.as_deref();
        complete_feedback(
            &ctx,
            &context,
            config_id,
            &base_token,
            &table_id,
            record_id,
            &input,
            &mut outcome,
        )
        .await;
    }
    // 单条派发的结果回写认领队列：worker 不再把同一条记录完整重跑一遍
    // （只为到达通知步骤）。放在 `complete_feedback` 之后是因为
    // `outcome.message` 此刻已渲染定稿——last_error 与请求记录同源。
    if let Some(record_id) = input.record_id.as_deref() {
        if let (Some(config_id), Some(base_token), Some(table_id)) = (
            outcome.config_id,
            outcome.base_token.as_deref(),
            outcome.table_id.as_deref(),
        ) {
            record_dispatch_task(
                &context, config_id, base_token, table_id, record_id, &outcome,
            )
            .await;
        }
    }
    write_request_log(&context, &input, &outcome).await;
    tracing::info!(
        stage = "response",
        config_id = outcome.config_id,
        mode,
        outcome = outcome.outcome.as_str(),
        error_code = outcome
            .response
            .as_ref()
            .map_or_else(|error| error.code(), |response| response.code),
        duration_ms = started.elapsed().as_millis() as u64,
        "审批派发请求处理结束"
    );
    outcome.response
}

/// 派发业务主体。
///
/// 返回 `Err` 的只有 `config_key` 校验与配置查询这类「连 config_id 都拿不到」的
/// 内部错误；已知 `config_id` 的鉴权/单条/受理出口都在内部折算成
/// [`DispatchOutcome`]（见各分支）。
async fn handle_dispatch(
    ctx: &ActionContext,
    input: &DispatchInput,
    context: &FeishuContext,
    config_key: &str,
) -> Result<DispatchOutcome, BaseError> {
    // 校验失败**不冒泡**——它同样要落记录，这里兜住再折算成失败出口。
    if let Err(error) = input.validate() {
        tracing::warn!(stage = "validate", "审批派发请求参数校验失败");
        return Ok(DispatchOutcome::internal(None, error));
    }
    // 空 key 必须在查库之前被拒（`validate_config_key` 的注释）。
    let config_key = validate_config_key(config_key)?;

    // ---- 鉴权：按 config_key 查配置行，校验访问 Token ----
    //
    // 查询**不带 `enabled` 条件**——停用要返回「已停用」而不是「不存在」，
    // 且顺序固定为「先验 Token 再报停用」（与 datasource 的 `verify_source`
    // 同一不变量：`token_is_checked_before_status`，由 `check_config_auth` 的
    // 单测钉住）。
    let config = context
        .approval_configs()
        .query()
        .select_fields(&["id", "base_token", "table_id", "token_hash", "enabled"])?
        .where_eq("config_key", serde_json::json!(&config_key))?
        .optional()
        .await?;
    let Some(config) = config else {
        tracing::warn!(stage = "auth", "审批派发未匹配任何配置");
        return Ok(DispatchOutcome::fail(
            CONFIG_NOT_FOUND,
            None,
            "该派发链接未配置审批派发，请先在控制台「审批配置」页创建配置",
        ));
    };
    let (config_id, base_token, table_id) = match check_config_auth(&config, &input.token) {
        Ok(verified) => verified,
        Err(outcome) => return Ok(outcome),
    };
    tracing::info!(stage = "config", config_id, "审批派发已匹配启用配置");

    match input.record_id.as_deref() {
        // ---- 单条：同步处理 ----
        Some(record_id) => {
            match dispatch_single(
                ctx,
                context,
                config_id,
                &base_token,
                &table_id,
                record_id,
                input.applicant.clone(),
            )
            .await
            {
                Ok(outcome) => Ok(outcome),
                // 单条内部的 `?` 出口：config_id 在调用点已知，失败记录带上它。
                Err(error) => Ok(DispatchOutcome::internal(Some(config_id), error)),
            }
        }
        // ---- 全表：异步受理 ----
        //
        // 受理**必须在鉴权通过之后**：Token 校验失败一律不出站、不入队。
        None => {
            // 受理前先解析发起人（若有）：值形态是**姓名**（M11），要解析成 open_id
            // 才能随信号交给 worker 落库（设计 §6.3）。未传/空白 → 现状，不建出站栈。
            let resolved = if input
                .applicant
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .is_some()
            {
                let Some(settings) = context.settings().filter(|value| value.can_pull()) else {
                    return Ok(DispatchOutcome::fail(
                        50301,
                        Some(config_id),
                        "飞书出站凭证未配置",
                    ));
                };
                let outbound = match outbound_setup::build(ctx, settings) {
                    Ok(outbound) => outbound,
                    Err(error) => return Ok(DispatchOutcome::internal(Some(config_id), error)),
                };
                match resolve_optional_applicant(
                    context,
                    settings,
                    &outbound,
                    input.applicant.as_deref(),
                )
                .await
                {
                    Ok(open_id) => open_id,
                    Err(message) => {
                        return Ok(DispatchOutcome::fail(
                            APPLICANT_UNRESOLVED,
                            Some(config_id),
                            message,
                        ))
                    }
                }
            } else {
                None
            };

            // 拿不到句柄说明 worker 没起（凭证缺失）。**不返回 accepted**——
            // 那会让工作流显示成功而实际什么都没发生。
            let handle = match ctx.tools().extension::<ApprovalDispatchHandle>() {
                Ok(handle) => handle,
                Err(error) => return Ok(DispatchOutcome::internal(Some(config_id), error)),
            };
            if let Err(error) = handle.request_dispatch(config_id, resolved) {
                return Ok(DispatchOutcome::internal(Some(config_id), error));
            }
            tracing::info!(
                stage = "enqueue",
                config_id,
                scope = "all_enabled_configs",
                "审批派发已发送 Worker 唤醒信号"
            );

            // 语义是「跑一轮全队列」，**不是**只跑本表。
            //
            // 用方原意就是「处理所有没有审批编号的数据」，而这三张表是按
            // `config_key` 定位的；按表分流会让同一队列出现「点了 A 的
            // 按钮却不处理 B 的数据」这种反直觉行为。多表并发时谁先点谁先跑整批，
            // 结果仍是一致的（uuid 幂等兜底）。
            Ok(DispatchOutcome::success(
                Outcome::Accepted,
                Some(config_id),
                Some(base_token),
                Some(table_id),
                true,
                FeedbackStatus::Accepted,
                "已受理，处理结果稍后回填至表格".to_string(),
                None,
            ))
        }
    }
}

/// 把 HTTP 单条派发的结果回写认领队列（`feishu_approval_task`）。
///
/// 与 worker 的 `record_outcome` **同口径**（状态映射逐项对齐）：
/// - `Succeeded` → `backfilled` + instance_code + serial_number
/// - `Failed` → `terminal` + last_error
/// - `Waiting` / `Retryable` / `CreatedPendingBackfill` → `pending` + last_error
///   （worker 兜底重试；CreatedPendingBackfill 同时落 instance_code）
/// - `Accepted`（全表受理）→ 不写：不是单条结果，worker 处理每条时自行落状态。
///
/// # 为什么 HTTP 路径也要写
///
/// 不写的话 task 行停在 `pending`，worker 会把同一条记录**完整重跑一遍**
/// （读表结构/读记录/重填 8 个字段）只为到达通知步骤——实测整链路约 40 秒
/// 且白耗飞书配额。写了之后 `backfilled`/`terminal` 的行不再被认领；
/// `pending` 的行由 worker 兜底（与「清空回填字段重试」同一条通道）。
///
/// # upsert
///
/// 行通常已由 worker 的 seed 建立（→ update），也可能不存在——记录在 worker
/// 首扫前就被派发（→ insert，uuid 与派发路径**同源**派生，顺带修掉「事件订阅
/// 反查不到 task 行」的缺口）。并发下查询与插入之间 seed 建行会撞 record_id
/// 唯一索引（1062），捕获后转为 update。
///
/// `last_error` 用 `outcome.message`（此刻已由 `complete_feedback` 渲染定稿，
/// 含配置/表名定位）——比 worker 的简短原因长，但排查信息更全，与请求记录同源。
///
/// 写失败只 `warn`，绝不改变派发结果（与 [`write_request_log`] 同纪律）。
async fn record_dispatch_task(
    context: &FeishuContext,
    config_id: i64,
    base_token: &str,
    table_id: &str,
    record_id: &str,
    outcome: &DispatchOutcome,
) {
    let (state, reason) = match outcome.status {
        FeedbackStatus::Succeeded => ("backfilled", None),
        FeedbackStatus::Failed => ("terminal", Some(outcome.message.as_str())),
        FeedbackStatus::Waiting
        | FeedbackStatus::Retryable
        | FeedbackStatus::CreatedPendingBackfill => ("pending", Some(outcome.message.as_str())),
        FeedbackStatus::Accepted => return,
    };

    let mut row = Record::new();
    row.insert("state", serde_json::json!(state));
    row.insert(
        "last_error",
        reason.map_or(serde_json::Value::Null, |value| serde_json::json!(value)),
    );
    // 清租约：状态已是稳定状态（与 worker 的 `outcome_record` 同口径）。
    row.insert("lease_until", serde_json::Value::Null);
    if let Some(instance_code) = outcome.instance_code.as_deref() {
        row.insert("instance_code", serde_json::json!(instance_code));
    }
    if let Some(serial_number) = outcome.serial_number.as_deref() {
        row.insert("serial_number", serde_json::json!(serial_number));
    }

    // 已有行（seed 建过的常见路径）：按主键 update。
    let existing = match context
        .approval_tasks()
        .query()
        .select_fields(&["id"])
        .and_then(|query| query.where_eq("record_id", serde_json::json!(record_id)))
    {
        Ok(query) => match query.optional().await {
            Ok(row) => row,
            Err(error) => {
                tracing::warn!(error = %error, record_id, "派发结果回写：查询任务行失败");
                return;
            }
        },
        Err(error) => {
            tracing::warn!(error = %error, record_id, "派发结果回写：构造查询失败");
            return;
        }
    };
    if let Some(existing_row) = existing {
        let id: i64 = match existing_row.require("id") {
            Ok(id) => id,
            Err(error) => {
                tracing::warn!(error = %error, record_id, "派发结果回写：任务行缺 id");
                return;
            }
        };
        let updated = match context
            .approval_tasks()
            .query()
            .where_primary_key_eq(serde_json::json!(id))
        {
            Ok(query) => query.update(row).await,
            Err(error) => Err(error),
        };
        match updated {
            Ok(_) => tracing::info!(task_id = id, state, record_id, "派发结果已回写任务行"),
            Err(error) => {
                tracing::warn!(error = %error, task_id = id, "派发结果回写任务行失败");
            }
        }
        return;
    }

    // 行不存在（记录在 worker 首扫前被派发）：insert。
    // uuid 与派发路径**同源**派生（seed 同款）：60012 对账与事件订阅都靠它。
    let approval_code: String = match context
        .approval_configs()
        .query()
        .select_fields(&["approval_code"])
        .and_then(|query| query.where_primary_key_eq(serde_json::json!(config_id)))
    {
        Ok(query) => match query.optional().await {
            Ok(Some(config)) => match config.require("approval_code") {
                Ok(code) => code,
                Err(error) => {
                    tracing::warn!(error = %error, config_id, "派发结果回写：配置缺 approval_code");
                    return;
                }
            },
            Ok(None) => {
                tracing::warn!(config_id, "派发结果回写：配置不存在");
                return;
            }
            Err(error) => {
                tracing::warn!(error = %error, config_id, "派发结果回写：读配置失败");
                return;
            }
        },
        Err(error) => {
            tracing::warn!(error = %error, config_id, "派发结果回写：构造配置查询失败");
            return;
        }
    };
    let uuid = derive_uuid(base_token, table_id, &approval_code, record_id);
    let mut insert_row = row.clone();
    insert_row.insert("config_id", serde_json::json!(config_id));
    insert_row.insert("record_id", serde_json::json!(record_id));
    insert_row.insert("uuid", serde_json::json!(uuid));
    match context.approval_tasks().query().insert(insert_row).await {
        Ok(_) => tracing::info!(record_id, state, "派发结果已建任务行"),
        Err(error) => {
            let text = error.to_string();
            if text.contains("Duplicate") || text.contains("1062") {
                // 并发竞态：查询与插入之间 seed 建了行 → 转为按 record_id update。
                let raced = match context
                    .approval_tasks()
                    .query()
                    .where_eq("record_id", serde_json::json!(record_id))
                {
                    Ok(query) => query.update(row).await,
                    Err(error) => Err(error),
                };
                match raced {
                    Ok(_) => {
                        tracing::info!(
                            record_id,
                            state,
                            "派发结果已回写任务行（并发建行后转 update）"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            record_id,
                            "派发结果回写任务行失败（竞态转 update 亦失败）"
                        );
                    }
                }
            } else {
                tracing::warn!(error = %error, record_id, "派发结果建任务行失败");
            }
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
        // 坐标来自配置行（`config_key` 定位），不再取自请求体。取不到（鉴权前的
        // 失败出口）落空串——`base_token`/`table_id` 是日志表的必填列，不能 NULL。
        base_token: outcome
            .base_token
            .as_deref()
            .unwrap_or_default()
            .to_string(),
        table_id: outcome.table_id.as_deref().unwrap_or_default().to_string(),
        config_id: outcome.config_id,
        record_id: input
            .record_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        request_body,
        outcome: outcome.outcome,
        message: approval_feedback::truncate(&outcome.message, 255),
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
    } else {
        tracing::info!(
            stage = "persist_request",
            config_id = outcome.config_id,
            outcome = outcome.outcome.as_str(),
            "审批派发请求记录写入成功"
        );
    }
}

/// 请求里的 `applicant`（**姓名**）→ 供编排使用的 open_id。
///
/// 返回 `Ok(Some(open_id))`（唯一命中）/ `Ok(None)`（未传或 trim 后为空——完全走
/// 现状读表格列）/ `Err(可行动文案)`（未命中、重名多命中、或通讯录出站失败）。
///
/// 抽成独立函数是为了能被单测直接驱动：`dispatch_single` 本身要 `ActionContext`
/// 才能读配置行，而这条解析链路的三个出口（命中/未命中/空白回退）正是本期新增、
/// 最需要钉住的部分。两条路径（单条同步、全表受理）共用它，口径不会漂。
async fn resolve_optional_applicant(
    context: &FeishuContext,
    settings: &FeishuSettings,
    outbound: &outbound_setup::Outbound,
    applicant: Option<&str>,
) -> Result<Option<String>, String> {
    // 未传 / trim 后为空 → 返回 None（编排层回退读表格列），且**不打任何请求**：
    // 这是「不传 applicant 就零行为变化」那条承诺的落点，也顺带避免了为一次
    // 用不上的解析去取用户凭证（未授权时那会变成一条莫名其妙的失败）。
    let Some(name) = trimmed_applicant(applicant) else {
        return Ok(None);
    };
    let credentials = FeishuCredentials {
        app_id: settings.app_id.clone().unwrap_or_default(),
        app_secret: settings.app_secret.clone().unwrap_or_default(),
    };
    let bearer = user_access_token(
        context.user_tokens(),
        outbound.transport(),
        outbound.sleeper(),
        &credentials,
    )
    .await
    .map_err(|error| format!("发起人姓名解析失败：{}", error.message()))?;
    resolve_applicant_param(outbound.transport(), outbound.sleeper(), &bearer, name)
        .await
        .map(Some)
}

/// 归一姓名：未传 / trim 后为空 → `None`。
///
/// 单独成函数是为了让「空白姓名不打任何请求」这条承诺可离线钉住（见测试）。
fn trimmed_applicant(applicant: Option<&str>) -> Option<&str> {
    applicant.map(str::trim).filter(|name| !name.is_empty())
}

/// 姓名 → open_id（凭证已取好）。可行动文案的**唯一**落点。
async fn resolve_applicant_param(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    bearer_token: &str,
    name: &str,
) -> Result<String, String> {
    match resolve_applicant(transport, sleeper, bearer_token, name).await {
        Ok(ApplicantResolution::Resolved { open_id }) => Ok(open_id),
        Ok(ApplicantResolution::NotFound) => Err(format!(
            "发起人姓名解析失败：未找到「{name}」，请检查工作流变量或改用唯一姓名"
        )),
        Ok(ApplicantResolution::Ambiguous { candidates }) => Err(format!(
            "发起人姓名解析失败：存在多个同名「{name}」（{}），请检查工作流变量或改用唯一姓名",
            candidates.join("、")
        )),
        Err(failure) => Err(applicant_failure_message(&failure)),
    }
}

/// 姓名解析的**出站失败** → 可行动文案（含错误码）。
///
/// scope 未开通（99991672）单列：它不是「名字写错了」，而是「应用没这个权限」，
/// 处置是去开发者后台开通并等审核，与改工作流变量是两回事。
fn applicant_failure_message(failure: &OutboundFailure) -> String {
    match failure.kind {
        FailureKind::Fatal { code } if code == CODE_CONTACT_SCOPE_MISSING => {
            format!("发起人姓名解析失败：通讯录权限未开通，等待审核（错误码 {code}）")
        }
        FailureKind::Fatal { code } if code == CODE_USER_TOKEN_SCOPE_MISSING => {
            format!(
                "发起人姓名解析失败：用户授权缺少通讯录权限，请重新授权并在授权页补充权限（错误码 {code}）"
            )
        }
        FailureKind::Fatal { code } if CODES_USER_TOKEN_MALFORMED.contains(&code) => {
            format!("发起人姓名解析失败：飞书用户凭证无效，请在「飞书审批配置」页重新授权（错误码 {code}）")
        }
        FailureKind::Fatal { code } => {
            format!("发起人姓名解析失败（错误码 {code}）：{}", failure.message)
        }
        _ => format!("发起人姓名解析失败：{}", failure.message),
    }
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
    base_token: &str,
    table_id: &str,
    record_id: &str,
    applicant: Option<String>,
) -> Result<DispatchOutcome, BaseError> {
    tracing::info!(
        stage = "config",
        config_id,
        record_id,
        "审批派发开始同步处理单条记录"
    );
    let Some(settings) = context.settings().filter(|value| value.can_pull()) else {
        tracing::warn!(
            stage = "config",
            "审批派发无法处理单条记录：飞书出站凭证未配置"
        );
        return Ok(DispatchOutcome::fail(
            50301,
            Some(config_id),
            "飞书出站凭证未配置",
        ));
    };

    // ---- 配置（坐标由调用方 `handle_dispatch` 已鉴权读出，这里只读审批定义那几列）----
    let config_row = context
        .approval_configs()
        .query()
        .where_primary_key_eq(serde_json::json!(config_id))?
        .optional()
        .await?
        .ok_or_else(|| BaseError::ConfigError("审批配置在读取前被删除".to_string()))?;

    let approval_code: String = config_row.require("approval_code")?;
    let applicant_field: String = config_row.require("applicant_field")?;
    let backfill_field: String = config_row.require("backfill_field")?;
    let base_timezone: String = config_row.require("base_timezone")?;
    // 先留一份给日志/回执用（`BitableCoordinates` 会在下面把它们 move 走）。
    let base_token = base_token.to_string();
    let table_id = table_id.to_string();

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
        tracing::warn!(stage = "config", "审批字段映射配置损坏，转换器标识非法");
        return Ok(DispatchOutcome::fail(
            50001,
            Some(config_id),
            "字段映射配置损坏（转换器标识非法）",
        ));
    };
    if widgets.is_empty() {
        tracing::warn!(stage = "config", "审批配置没有字段映射");
        return Ok(DispatchOutcome::fail(
            50002,
            Some(config_id),
            "该配置没有字段映射",
        ));
    }
    let timezone_offset = match FixedOffset::from_iana(base_timezone.trim()) {
        Ok(offset) => offset,
        Err(error) => {
            tracing::warn!(stage = "config", "审批配置的 Base 时区不可用");
            return Ok(DispatchOutcome::fail(
                50003,
                Some(config_id),
                format!("Base 时区不可用：{error}"),
            ));
        }
    };

    let outbound = outbound_setup::build(ctx, settings)?;

    // ---- 发起人姓名解析（§6.2）----
    //
    // 请求里的 applicant 是**姓名**（真机 M11：`$.step_btn.user` 渲染成 name），
    // 要经通讯录搜索解析成 open_id 才能提单。未传/空白 → 完全走现状（读表格列），
    // 零行为变化；解析失败一律 fail 并可行动文案——**不落等待态**，因为要改的是
    // 工作流变量，而不是等用户往表格里补数据。
    let resolved_applicant = match resolve_optional_applicant(
        context,
        settings,
        &outbound,
        applicant.as_deref(),
    )
    .await
    {
        Ok(open_id) => open_id,
        Err(message) => {
            return Ok(DispatchOutcome::fail(
                APPLICANT_UNRESOLVED,
                Some(config_id),
                message,
            ))
        }
    };

    let coordinates = BitableCoordinates {
        app_token: base_token.clone(),
        table_id: table_id.clone(),
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
        Ok(fields) => {
            tracing::info!(stage = "fields", config_id, applicant_field = %applicant_field, backfill_field = %backfill_field, widget_count = widgets.len(), "审批派发开始读取多维表格结构");
            tracing::info!(
                stage = "record",
                field_count = fields.len(),
                "审批派发开始读取源记录"
            );
            fields
        }
        Err(failure) => {
            tracing::warn!(stage = "fields", kind = ?failure.kind, "审批派发读取表结构失败");
            return Ok(DispatchOutcome::fail(
                50201,
                Some(config_id),
                format!("读取表结构失败：{}", failure.message),
            ));
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
            tracing::warn!(stage = "fields", backfill_field = %backfill_field, "审批派发回填列不可用");
            return Ok(DispatchOutcome::fail(
                50004,
                Some(config_id),
                format!("回填列不可用：{error}"),
            ));
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
    let (cells, creator_open_id): (serde_json::Map<String, serde_json::Value>, Option<String>) =
        match found {
            Ok(items) => match items.into_iter().next() {
                Some(record) => {
                    // 记录创建人 = 通知接收人的最后兜底（与 worker 的
                    // `process_one` 同一来源、同一用途）。
                    let creator_open_id = record
                        .created_by
                        .as_ref()
                        .map(|person| person.id.trim().to_string())
                        .filter(|id| !id.is_empty());
                    match bitable::rekey_cells_by_field_id(&fields, &record.fields) {
                        Ok(cells) => {
                            tracing::info!(
                                stage = "record",
                                cell_count = cells.len(),
                                "审批派发源记录字段映射完成"
                            );
                            (cells, creator_open_id)
                        }
                        Err(error) => {
                            tracing::warn!(
                                stage = "record",
                                "审批派发解析记录字段失败，详情见派发记录"
                            );
                            return Ok(DispatchOutcome::fail(
                                50201,
                                Some(config_id),
                                format!("解析记录字段失败：{error}"),
                            ));
                        }
                    }
                }
                None => {
                    tracing::warn!(stage = "record", "多维表格里找不到审批源记录");
                    return Ok(DispatchOutcome::fail(
                        40402,
                        Some(config_id),
                        "多维表格里找不到该记录",
                    ));
                }
            },
            Err(failure) => {
                tracing::warn!(stage = "record", kind = ?failure.kind, "审批派发读取源记录失败");
                return Ok(DispatchOutcome::fail(
                    50201,
                    Some(config_id),
                    format!("读取记录失败：{}", failure.message),
                ));
            }
        };

    // ---- 编排 ----
    let backfill = BitableBackfill {
        transport: outbound.transport(),
        sleeper: outbound.sleeper(),
        tokens: &outbound.tokens,
        coordinates: &coordinates,
        fields: &fields,
    };
    // 通知兜底接收人：与 `dispatch_one` 内部同一判定（传参优先、回退表格列，
    // 两处必须同源，否则会出现「派发用了 A 的 open_id、通知却发给 B」）。
    // 在 cells / resolved_applicant 还在手的这里算好，随 outcome 带到
    // complete_feedback——请求未显式指定 notification_recipient 时用它兜底。
    let notify_open_id = resolved_applicant
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| applicant_open_id(&cells, &applicant_field));

    let result = dispatch_one(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        &backfill,
        &OrchestrationInput {
            fields: &fields,
            context: Some(context),
            coordinates: &coordinates,
            record_id,
            cells: &cells,
            applicant_field: &applicant_field,
            // 已在上面解析成 open_id（空/未传时为 None → 编排层回退读表格列）。
            applicant: resolved_applicant,
            backfill_field_name: &backfill_field_name,
            approval_code: &approval_code,
            widgets: &widgets,
            form_snapshot: config_row.optional("form_snapshot")?.unwrap_or_default(),
            timezone_offset,
            status_field: config_row
                .optional::<String>("status_field")?
                .filter(|v| !v.trim().is_empty())
                .as_deref(),
            flow_field: config_row
                .optional::<String>("flow_field")?
                .filter(|v| !v.trim().is_empty())
                .as_deref(),
            start_time_field: config_row
                .optional::<String>("start_time_field")?
                .filter(|v| !v.trim().is_empty())
                .as_deref(),
            end_time_field: config_row
                .optional::<String>("end_time_field")?
                .filter(|v| !v.trim().is_empty())
                .as_deref(),
            department_field: config_row
                .optional::<String>("department_field")?
                .filter(|v| !v.trim().is_empty())
                .as_deref(),
            handler_field: config_row
                .optional::<String>("handler_field")?
                .filter(|v| !v.trim().is_empty())
                .as_deref(),
        },
    )
    .await;

    let outcome_kind = outcome_for(&result);
    let status = result.feedback_status();
    let (accepted, message, serial_number) = result.response_parts();
    let instance_code = match &result {
        // CreatedPendingBackfill 也带实例坐标（实例已创建、编号/回填未完成）——
        // 与 worker 的 `record_outcome` 同口径落 task 行，并让通知里的审批链接可用。
        OrchestrationResult::Backfilled { instance_code, .. }
        | OrchestrationResult::CreatedPendingBackfill { instance_code, .. } => {
            Some(instance_code.clone())
        }
        _ => None,
    };
    let record_summary = approval_feedback::record_summary(&fields, &cells);
    let mut outcome = DispatchOutcome::success(
        outcome_kind,
        Some(config_id),
        Some(base_token),
        Some(table_id),
        accepted,
        status,
        message,
        serial_number,
    );
    outcome.instance_code = instance_code;
    outcome.record_summary = record_summary;
    outcome.applicant_open_id = notify_open_id;
    outcome.creator_open_id = creator_open_id;
    Ok(outcome)
}

/// 组一个响应体。
///
/// 用 `ApiResponse::success` 的 `data` 通道而不是把结果塞进 `message`：工作流的
/// `response_value` 只能引用**声明过的字段**，所以结果要落成具名字段（`accepted`
/// / `serial_number`）才好引用。
///
/// 失败（`accepted = false`）仍走 `success` 通道：这是**业务结果**不是接口错误，
/// 工作流要能同时读到成败与原因。接口级错误（鉴权失败、坐标未配置）才用 `fail`。
///
/// `notification` 默认 `NotificationResult::not_requested()` 的序列化值；
/// `complete_feedback` 会在返回前覆写 `data` 里的 `notification` 和 `message`。
fn response(
    accepted: bool,
    status: FeedbackStatus,
    message: String,
    serial_number: Option<String>,
) -> Result<ApiResponse, BaseError> {
    ApiResponse::success(
        DispatchResultBody {
            accepted,
            status: status.as_str().to_string(),
            message: message.clone(),
            serial_number,
            notification: serde_json::json!({"status":"not_requested"}),
        },
        message,
    )
}

/// 请求未显式指定接收人时的兜底链：申请人 open_id → 记录创建人 open_id。
///
/// 兜底对**除受理外的任何状态**都给（2026-10-15 原则：用户发起请求就必须收到
/// 反馈，成功失败都要，失败附原因）。旧口径「只在终态兜底、中间态交给 worker」
/// 在「申请人列空着」时两头都发不出去——waiting 拿不到接收人，worker 又只认
/// 终态，正是「创建失败没有任何通知」的根因。
///
/// `Accepted`（全表受理）除外：它只是「已入队」，真正的结果由 worker 逐条通知
/// ——这里再发一次的话，受理 N 条会先来一条「已受理」再跟 N 条结果。
fn fallback_recipient_of(outcome: &DispatchOutcome) -> Option<NotificationRecipient> {
    match outcome.status {
        FeedbackStatus::Accepted => None,
        _ => outcome
            .applicant_open_id
            .as_deref()
            .or(outcome.creator_open_id.as_deref())
            .map(|open_id| NotificationRecipient {
                receive_id_type: ReceiveIdType::OpenId,
                receive_id: open_id.to_string(),
            }),
    }
}

/// 收尾反馈：渲染展示文案 + 发送 IM 通知（若有接收人）。
///
/// 通知失败**不影响**审批结论——`outcome.response` 不变，只有 `outcome.message` 和
/// `outcome.notification` 被更新。名称补充和通知均发生在派发之后，不重跑审批。
#[allow(clippy::too_many_arguments)] // 8 个参数：handler 级上下文逐个传入，收结构体无实质收益。
async fn complete_feedback(
    ctx: &ActionContext,
    context: &FeishuContext,
    config_id: i64,
    base_token: &str,
    table_id: &str,
    record_id: Option<&str>,
    input: &DispatchInput,
    outcome: &mut DispatchOutcome,
) {
    // 读配置行取展示名（2s 预算）
    let config_lookup = async {
        context
            .approval_configs()
            .query()
            .where_primary_key_eq(serde_json::json!(config_id))?
            .optional()
            .await
    };
    let config = match tokio::time::timeout(std::time::Duration::from_secs(2), config_lookup).await
    {
        Ok(Ok(row)) => row,
        _ => {
            tracing::warn!(config_id, "反馈展示名称读取失败，跳过直接通知");
            None
        }
    };
    let name = |field: &str| {
        let value = config
            .as_ref()
            .and_then(|row| row.optional::<String>(field).ok().flatten());
        approval_feedback::display_name(value.as_deref())
    };
    let names = format!(
        "配置：{}\n多维表：{}\n数据表：{}\n审批：{}",
        name("title"),
        name("base_name"),
        name("table_name"),
        name("approval_name")
    );
    outcome.message = approval_feedback::render(
        outcome.status,
        &outcome.message,
        &names,
        outcome.record_summary.as_deref(),
        record_id,
        base_token,
        table_id,
    );
    // 接收人优先级：显式 `notification_recipient` > 申请人 open_id > 记录创建人。
    let recipient = input
        .notification_recipient
        .clone()
        .or_else(|| fallback_recipient_of(outcome));
    let notification = match &recipient {
        None => NotificationResult::not_requested(),
        Some(_) if config.is_none() => NotificationResult::skipped(),
        Some(recipient) => match context.settings().filter(|settings| settings.can_pull()) {
            Some(settings) => match outbound_setup::build(ctx, settings) {
                Ok(outbound) => {
                    let record_url = feedback_record_url(
                        outbound.transport(),
                        outbound.sleeper(),
                        &outbound.tokens,
                        base_token,
                        table_id,
                        record_id,
                    )
                    .await;
                    let references = feedback_references(
                        record_id,
                        outcome,
                        base_token,
                        table_id,
                        &name("base_name"),
                        &name("table_name"),
                        &name("approval_name"),
                        record_url,
                    );
                    approval_notification::send_feedback(
                        outbound.transport(),
                        outbound.sleeper(),
                        &outbound.tokens,
                        recipient,
                        &outcome.message,
                        &references,
                    )
                    .await
                }
                Err(_) => NotificationResult::unavailable(),
            },
            None => NotificationResult::unavailable(),
        },
    };
    // 通知结果落日志流：response_body 里虽有记录，但那要按行查库才能看到——
    // 通知是外部副作用，日志里必须有痕迹（与 worker 路径同一口径）。
    match &notification.status {
        NotificationStatus::Sent => tracing::info!(
            config_id,
            message_id = ?notification.message_id,
            "HTTP 派发通知：IM 消息发送成功"
        ),
        NotificationStatus::NotRequested => tracing::info!(
            config_id,
            "HTTP 派发通知：无接收人（未指定 notification_recipient 且无申请人兜底），跳过通知"
        ),
        _ => tracing::warn!(
            config_id,
            status = ?notification.status,
            message = %notification.message,
            "HTTP 派发通知：消息未发送"
        ),
    }
    outcome.notification = serde_json::to_value(&notification)
        .unwrap_or_else(|_| serde_json::json!({"status":"failed","message":"通知结果序列化失败"}));
    // 同步更新 response 里的 data（工作流读这里）
    if let Ok(response) = &mut outcome.response {
        response.message.clone_from(&outcome.message);
        if let Some(data) = response.data.as_mut() {
            data["message"] = serde_json::json!(outcome.message);
            data["status"] = serde_json::json!(outcome.status.as_str());
            data["notification"] = outcome.notification.clone();
        }
    }
}

/// 消息末尾的业务定位链接；URL 来自已核对的坐标、记录接口或已确认的审批实例。
#[allow(clippy::too_many_arguments)] // 8 个参数：定位链接的各坐标来源逐个平铺，收结构体无实质收益。
fn feedback_references(
    record_id: Option<&str>,
    outcome: &DispatchOutcome,
    base_token: &str,
    table_id: &str,
    base_name: &str,
    table_name: &str,
    approval_name: &str,
    record_url: Option<String>,
) -> Vec<FeedbackReference> {
    let table_url = (|| -> anyhow::Result<String> {
        bitable::validate_path_segment("base_token", base_token)?;
        bitable::validate_path_segment("table_id", table_id)?;
        Ok(format!(
            "https://www.feishu.cn/base/{}?table={table_id}",
            base_token
        ))
    })()
    .ok();
    let mut references = vec![FeedbackReference {
        label: "多维表",
        text: format!("{base_name} / {table_name}"),
        href: table_url,
    }];
    if record_id.is_some() {
        references.push(FeedbackReference {
            label: "原记录",
            text: outcome
                .record_summary
                .clone()
                .unwrap_or_else(|| "本次触发的记录（摘要暂不可用）".into()),
            href: record_url,
        });
    }
    let (text, href) = match outcome.instance_code.as_deref() {
        Some(code) => (
            format!(
                "{approval_name} / {}",
                outcome.serial_number.as_deref().unwrap_or("编号待生成")
            ),
            crate::addon::feishu::domain::approval::instance_applink(code).ok(),
        ),
        None => (approval_name.to_string(), None),
    };
    references.push(FeedbackReference {
        label: "审批",
        text,
        href,
    });
    references
}

/// 可选的链接补充总预算两秒；缺权限、超时或链接缺失均不改变审批结果。
async fn feedback_record_url(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &crate::addon::feishu::domain::tenant_token::TenantTokenProvider,
    base_token: &str,
    table_id: &str,
    record_id: Option<&str>,
) -> Option<String> {
    let record_id = record_id?;
    let coordinates = BitableCoordinates {
        app_token: base_token.to_string(),
        table_id: table_id.to_string(),
        view_id: None,
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        bitable::get_record_shared_url(transport, sleeper, tokens, &coordinates, record_id),
    )
    .await
    .ok()?
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::addon::feishu::domain::token::hash_token;
    use yang_base::table::Record;

    /// 构造一行带 `id` / `enabled` 的配置行（供 `check_config_auth` 用）。
    fn config_row(id: i64, enabled: bool) -> Record {
        let mut row = Record::new();
        row.insert("id", json!(id));
        row.insert("base_token", json!("appTestBase"));
        row.insert("table_id", json!("tblTest"));
        row.insert("enabled", json!(enabled));
        row
    }

    /// 已签发 + 已启用的配置行（Token 正确）。
    fn issued_config_row(plaintext_token: &str) -> Record {
        let mut row = config_row(7, true);
        row.insert("token_hash", serde_json::json!(hash_token(plaintext_token)));
        row
    }

    /// 正常请求：`token` 必填（鉴权），其余可选。
    fn valid_input() -> DispatchInput {
        DispatchInput {
            token: "issued-access-token".to_string(),
            record_id: Some("rec001".to_string()),
            requested_by: Some("测试触发人".to_string()),
            applicant: None,
            notification_recipient: None,
        }
    }

    #[test]
    fn blank_token_is_rejected() {
        // `token` 是**唯一**必填字段（鉴权凭据）；空串与「没给」一样必须被拒。
        let mut input = valid_input();
        input.token = "   ".to_string();
        assert!(input.validate().is_err(), "空 token 应被拒");
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

    /// 兜底接收人链：申请人优先 → 记录创建人兜底 → 都没有则不发；全表受理不发。
    ///
    /// 「中间态也要兜底」是 2026-10-15 的原则变更点：旧实现只在
    /// `Succeeded | Failed` 给兜底，waiting 时两层都发不出消息。
    #[test]
    fn fallback_recipient_covers_interim_states_and_falls_back_to_the_creator() {
        let mut outcome = DispatchOutcome::fail(1, Some(7), "等待补齐数据");
        outcome.status = FeedbackStatus::Waiting;
        outcome.applicant_open_id = Some("ou_applicant".into());
        outcome.creator_open_id = Some("ou_creator".into());
        assert_eq!(
            fallback_recipient_of(&outcome).map(|recipient| recipient.receive_id),
            Some("ou_applicant".to_string()),
            "申请人存在时必须优先发给申请人"
        );

        // 申请人列空着（waiting 主因）→ 记录创建人兜底。
        outcome.applicant_open_id = None;
        assert_eq!(
            fallback_recipient_of(&outcome).map(|recipient| recipient.receive_id),
            Some("ou_creator".to_string()),
            "申请人缺失时必须退到记录创建人"
        );

        // 两者都无 → 不发（此时「发给谁」在事实上无解）。
        outcome.creator_open_id = None;
        assert!(fallback_recipient_of(&outcome).is_none());

        // 全表受理不发：结果由 worker 逐条通知，避免 1 + N 条轰炸。
        let mut accepted = DispatchOutcome::fail(0, Some(7), "已受理");
        accepted.status = FeedbackStatus::Accepted;
        accepted.applicant_open_id = Some("ou_applicant".into());
        accepted.creator_open_id = Some("ou_creator".into());
        assert!(
            fallback_recipient_of(&accepted).is_none(),
            "受理不是结果，不该发通知"
        );
    }

    /// `check_config_auth` 的失败出口。仓库 deny `unwrap_used`/`unwrap_err`，
    /// 所以这里自己匹配——成功即测试失败。
    fn expect_auth_failure(
        outcome: Result<(i64, String, String), DispatchOutcome>,
    ) -> DispatchOutcome {
        match outcome {
            Err(outcome) => outcome,
            Ok(_) => panic!("应当失败"),
        }
    }

    /// 鉴权四类出口：未签发 / Token 不符 / 停用 / 通过。
    ///
    /// 这是本次契约改形的**核心不变量**——「先验 Token，再报停用」必须钉住：
    /// Token 不符时一律返回 `TOKEN_MISMATCH`，不泄露「配置是否存在 / 是否停用」
    /// 这类旁路信息。
    #[test]
    fn check_config_auth_covers_all_four_exits_in_order() {
        // (1) 未签发（token_hash 空 / NULL）：必须在 Token 校验**之前**判，
        //     报「还没签发」而不是「Token 错了」。
        let unissued = config_row(7, true);
        let outcome = expect_auth_failure(check_config_auth(&unissued, "any-token"));
        assert!(
            matches!(outcome.response, Ok(ref r) if r.code == TOKEN_NOT_ISSUED),
            "未签发应报 TOKEN_NOT_ISSUED: {outcome:?}"
        );

        // (2) Token 不符：停用位为假，但**必须先**返回 TOKEN_MISMATCH 而不是
        //     CONFIG_DISABLED——这就是「先验 Token 再报停用」的落点。
        let wrong_token = issued_config_row("correct-token");
        let outcome = expect_auth_failure(check_config_auth(&wrong_token, "wrong-token"));
        assert!(
            matches!(outcome.response, Ok(ref r) if r.code == TOKEN_MISMATCH),
            "Token 不符应报 TOKEN_MISMATCH（即使已停用）: {outcome:?}"
        );

        // (3) Token 对但已停用：报 CONFIG_DISABLED。
        let disabled = issued_config_row("correct-token");
        let mut disabled = disabled;
        disabled.insert("enabled", serde_json::json!(false));
        let outcome = expect_auth_failure(check_config_auth(&disabled, "correct-token"));
        assert!(
            matches!(outcome.response, Ok(ref r) if r.code == CONFIG_DISABLED),
            "Token 对但已停用应报 CONFIG_DISABLED: {outcome:?}"
        );

        // (4) Token 对且启用：通过，返回 (config_id, base_token, table_id)。
        let ok = issued_config_row("correct-token");
        let (config_id, base_token, table_id) = check_config_auth(&ok, "correct-token")
            .unwrap_or_else(|outcome| panic!("应通过: {outcome:?}"));
        assert_eq!(
            (config_id, base_token.as_str(), table_id.as_str()),
            (7, "appTestBase", "tblTest")
        );
    }

    #[test]
    fn blank_config_key_is_rejected_before_db() {
        // 空 key 必须在查库之前被拒（`validate_config_key` 的注释）。
        assert!(validate_config_key("   ").is_err(), "空 key 应被拒");
        assert!(
            validate_config_key("  ok  ").is_ok(),
            "两侧空白 trim 后应通过"
        );
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
            status: "succeeded".to_string(),
            message: "ok".to_string(),
            serial_number: Some("202609280001".to_string()),
            notification: json!({"status":"not_requested"}),
        };
        let json = serde_json::to_value(&body).unwrap_or_else(|error| panic!("{error}"));
        let object = json.as_object().unwrap_or_else(|| panic!("应是对象"));
        assert!(object.contains_key("accepted"));
        assert!(object.contains_key("message"));
        assert!(object.contains_key("serial_number"));
        assert!(object.contains_key("status"));
        // 工作流引用的标量字段（accepted/message/serial_number/status）必须不是嵌套结构。
        // notification 是嵌套对象，但工作流不引用它——它是给 HTTP 调用方直接读的。
        for key in ["accepted", "message", "serial_number", "status"] {
            let value = &object[key];
            assert!(
                !value.is_object() && !value.is_array(),
                "工作流引用的字段 {key} 不能是嵌套结构"
            );
        }
        // notification 允许是嵌套对象。
        assert!(object.contains_key("notification"));
    }

    #[test]
    fn serial_number_is_omitted_when_absent() {
        // 全表受理的响应没有编号；省略而不是给 null，让工作流的字段声明更简单。
        let body = DispatchResultBody {
            accepted: true,
            status: "accepted".to_string(),
            message: "已受理".to_string(),
            serial_number: None,
            notification: serde_json::json!({"status":"not_requested"}),
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
        let failed = response(
            false,
            FeedbackStatus::Retryable,
            "暂时失败，可重试：飞书服务错误".to_string(),
            None,
        )
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

        let waiting = response(
            true,
            FeedbackStatus::Waiting,
            "本轮未处理：缺少必填字段".to_string(),
            None,
        )
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
        let outcome = DispatchOutcome::success(
            Outcome::Failed,
            Some(1),
            None,
            None,
            false,
            FeedbackStatus::Failed,
            "失败".to_string(),
            None,
        );
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
    // 发起人姓名 → open_id 解析（§6.2）
    // -----------------------------------------------------------------------

    /// 合成一次通讯录搜索响应（`items` 是逗号分隔的命中对象）。
    fn search_body(items: &str) -> String {
        format!(r#"{{"code":0,"msg":"ok","data":{{"items":[{items}]}}}}"#)
    }

    #[tokio::test]
    async fn applicant_param_resolves_a_unique_name_to_open_id() {
        // 请求里的 applicant 是**姓名**，解析成 open_id 才交给编排层（进 create 体）。
        let transport = ReplayTransport::new(vec![(
            200,
            search_body(r#"{"open_id":"ou_resolved","name":"杨佳俊"}"#),
        )]);
        let result = resolve_applicant_param(&transport, &NoSleep, "u-test", " 杨佳俊 ").await;
        assert_eq!(result, Ok("ou_resolved".to_string()));
        assert!(
            transport.seen().iter().any(|request| request.url
                == crate::addon::feishu::domain::approval::applicant_search_url()),
            "必须走 GET /open-apis/search/v1/user：{:#?}",
            transport.seen()
        );
    }

    #[tokio::test]
    async fn applicant_param_reports_not_found_with_an_actionable_message() {
        let transport = ReplayTransport::new(vec![(200, search_body(""))]);
        let message =
            match resolve_applicant_param(&transport, &NoSleep, "u-test", "查无此人").await {
                Err(message) => message,
                Ok(_) => panic!("未命中必须失败"),
            };
        assert!(message.contains("未找到"), "{message}");
        assert!(message.contains("请检查工作流变量"), "{message}");
    }

    #[tokio::test]
    async fn applicant_param_reports_ambiguous_names_with_candidates() {
        let transport = ReplayTransport::new(vec![(
            200,
            search_body(r#"{"open_id":"ou_a","name":"张三"},{"open_id":"ou_b","name":"张三"}"#),
        )]);
        let message = match resolve_applicant_param(&transport, &NoSleep, "u-test", "张三").await
        {
            Err(message) => message,
            Ok(_) => panic!("重名必须失败"),
        };
        assert!(message.contains("存在多个同名"), "{message}");
        assert!(message.contains("ou_a"), "{message}");
    }

    #[tokio::test]
    async fn applicant_param_maps_missing_scope_to_a_named_hint() {
        // scope 未开通（等审核）：文案要点名原因与错误码，而不是甩一坨裸响应。
        let transport = ReplayTransport::new(vec![(
            200,
            r#"{"code":99991672,"msg":"no scope"}"#.to_string(),
        )]);
        let message = match resolve_applicant_param(&transport, &NoSleep, "u-test", "杨佳俊").await
        {
            Err(message) => message,
            Ok(_) => panic!("缺 scope 必须失败"),
        };
        assert!(message.contains("通讯录权限未开通，等待审核"), "{message}");
        assert!(message.contains("99991672"), "{message}");
    }

    #[test]
    fn blank_or_absent_applicant_is_normalized_away() {
        // 未传 / trim 后为空 → None（编排层回退读表格列）。这是「空白姓名不打任何
        // 请求」的落点：`resolve_optional_applicant` 在这一步就返回，取凭证与出站
        // 都到不了。
        assert_eq!(trimmed_applicant(None), None);
        assert_eq!(trimmed_applicant(Some("   ")), None);
        assert_eq!(trimmed_applicant(Some(" 杨佳俊 ")), Some("杨佳俊"));
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
                bytes: None,
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

    #[tokio::test]
    async fn base_display_name_uses_the_envelope_and_rejects_unsafe_coordinates() {
        use crate::addon::feishu::domain::bitable::get_base_name;
        for (envelope, expected) in [
            (
                json!({"code":0,"data":{"app":{"name":"财务协作"}}}),
                Some("财务协作"),
            ),
            (json!({"code":0,"data":{"app":{}}}), None),
            (json!({"code":0,"data":{"app":{"name":"  "}}}), None),
        ] {
            let transport = ReplayTransport::new(vec![(200, envelope.to_string())]);
            let name = get_base_name(&transport, &NoSleep, &e2e_tokens(), "appDisplay")
                .await
                .unwrap_or_else(|error| panic!("读取名称失败: {error:?}"));
            assert_eq!(name.as_deref(), expected);
            transport.took(
                "Get",
                "https://open.feishu.cn/open-apis/bitable/v1/apps/appDisplay",
            );
        }
        let transport = ReplayTransport::new(vec![(
            200,
            json!({"code":99991672,"msg":"permission denied"}).to_string(),
        )]);
        assert!(
            get_base_name(&transport, &NoSleep, &e2e_tokens(), "appDisplay")
                .await
                .is_err()
        );
        let transport = ReplayTransport::new(vec![]);
        for coordinate in ["", "bad/token", "bad?query=1"] {
            assert!(
                get_base_name(&transport, &NoSleep, &e2e_tokens(), coordinate)
                    .await
                    .is_err()
            );
        }
        assert!(transport.seen().is_empty());
    }

    /// 惰性连接池的上下文。这些用例**不查库**——`load_external_options` 在没有
    /// 链接型控件时会短路返回空表，所以不需要真的数据库。
    ///
    /// `connect_lazy` 不建立任何连接，但它要求一个 Tokio 上下文。
    fn e2e_context() -> FeishuContext {
        let pool = Arc::new(
            sqlx::MySqlPool::connect_lazy("mysql://user:pass@localhost:3306/yang")
                .unwrap_or_else(|error| panic!("惰性连接池应可构造: {error}")),
        );
        e2e_context_with_pool(pool)
    }

    fn e2e_context_with_pool(pool: Arc<sqlx::MySqlPool>) -> FeishuContext {
        use crate::addon::feishu::domain::repository::Repository;
        use yang_base::definition::TableSpec;

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
            repository(crate::addon::feishu::approval::domain::task_applicant_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::user_token_table::table_spec()),
            repository(crate::addon::feishu::portal::table::table_spec()),
            repository(crate::addon::feishu::portal::domain::log_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::event_table::table_spec()),
            Arc::new(
                yang_base::action::auth::PasswordEngine::new(1)
                    .unwrap_or_else(|e| panic!("PasswordEngine 应可构造: {e}")),
            ),
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

    #[tokio::test]
    async fn invalid_manual_source_is_rejected_before_loading_bindings() -> anyhow::Result<()> {
        let form = json!([{"id":"company","name":"公司名称","type":"radioV2","externalData":{"externalDataLinkage":true}}]);
        let maps: Vec<crate::addon::feishu::domain::approval_match::FieldMapping> =
            serde_json::from_value(json!([
                {"widget_id":"company","bitable_field":"fldCompany","option_source_key":" "}
            ]))?;
        let transport = ReplayTransport::new(vec![
            (200, e2e_fields_body()),
            (200, e2e_definition_body(&form)),
        ]);
        let result = build_plan(
            &transport,
            &NoSleep,
            &e2e_tokens(),
            &e2e_context(),
            &ProvisionInput {
                base_token: E2E_BASE_TOKEN,
                table_id: E2E_TABLE_ID,
                approval_code: E2E_APPROVAL_CODE,
                applicant_field: "fldApplicant",
                backfill_field: "fldSerial",
                base_timezone: "Asia/Shanghai",
                maps: Some(&maps),
                status_field: None,
                flow_field: None,
                start_time_field: None,
                end_time_field: None,
                department_field: None,
                handler_field: None,
            },
        )
        .await;
        assert!(
            matches!(result, Err(ProvisionError::Invalid(_))),
            "{result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn unnamed_linked_widget_returns_invalid_without_querying_database() {
        let snapshot =
            json!([{"id":"w","type":"radioV2","externalData":{"externalDataLinkage":true}}])
                .to_string();
        let mut widgets = vec![crate::addon::feishu::domain::approval_convert::WidgetMap {
            widget_id: "w".to_string(),
            widget_type: "radioV2".to_string(),
            bitable_field: "fldTest".to_string(),
            required: false,
            converter: crate::addon::feishu::domain::approval_convert::Converter::Option,
            option_map: Default::default(),
            option_source_key: None,
            currency: None,
        }];
        let error = crate::addon::feishu::domain::approval_provision::resolve_external_options(
            &e2e_context(),
            &snapshot,
            &mut widgets,
            &serde_json::Map::new(),
            &[],
        )
        .await;
        assert!(
            matches!(error, Err(ProvisionError::Invalid(_))),
            "{error:?}"
        );
        let omitted = crate::addon::feishu::domain::approval_provision::resolve_external_options(
            &e2e_context(),
            &snapshot,
            &mut [],
            &serde_json::Map::new(),
            &[],
        )
        .await;
        assert!(omitted.is_ok(), "未映射的可选外部控件不应查库: {omitted:?}");
    }

    #[tokio::test]
    #[ignore = "需要真实 MySQL 测试库"]
    async fn real_mysql_resolves_detail_options_and_waits_on_ambiguous_labels() -> anyhow::Result<()>
    {
        use crate::addon::feishu::domain::approval_convert::WidgetMap;
        use crate::addon::feishu::domain::approval_dispatch::DispatchInput;
        use crate::addon::feishu::domain::approval_provision::resolve_external_options;
        let database =
            yang_db::Database::connect(&std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")?).await?;
        let name: String = sqlx::query_scalar("SELECT DATABASE()")
            .fetch_one(database.pool())
            .await?;
        anyhow::ensure!(name.ends_with("_test"), "拒绝在非测试库运行");
        crate::schema::sync_with_database(
            database,
            yang_db::DatabaseConfig::default(),
            Arc::new(crate::config::SecuritySettings::default()),
        )
        .await?;
        let database =
            yang_db::Database::connect(&std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")?).await?;
        let pool = Arc::new(database.pool().clone());
        let context = e2e_context_with_pool(pool.clone());
        let source = format!("att_{}", uuid::Uuid::new_v4().simple());
        let name = format!("附件验证选项_{source}");
        let mut transaction = database.transaction().await?;
        let (_, datasource_id) = context
            .datasources()
            .query()
            .insert_returning_id_in_tx(
                &mut transaction,
                yang_base::table::Record::new().set("title", name.clone()),
            )
            .await?;
        transaction.commit().await?;
        let binding = yang_base::table::Record::new()
            .set("datasource_id", datasource_id as i64)
            .set("field_id", "fldRegression")
            .set("field_name", name.clone())
            .set("source_key", source.clone())
            .set("token_hash", source.clone());
        context.datasource_fields().query().insert(binding).await?;
        let mut saved_config_id = None;
        let outcome: anyhow::Result<()> = async {
            for (suffix, row_source, label, enabled) in [
                ("good", source.clone(), "正确", true),
                ("disabled", source.clone(), "正确", false),
                ("other", format!("{source}_other"), "正确", true),
                ("different", source.clone(), "别的文案", true),
                ("dup1", source.clone(), "重名", true),
                ("dup2", source.clone(), "重名", true),
                ("dup3", source.clone(), "重名", true),
            ] {
                context.options().query().insert(yang_base::table::Record::new().set("option_id", format!("{source}_{suffix}")).set("source_key", row_source).set("label", label).set("enabled", enabled)).await?;
            }
            let snapshot = json!([{"id":"detail","type":"fieldList","children":[{"id":"linked","name":name,"type":"radioV2","externalData":{"externalDataLinkage":true}}]}]).to_string();
            let mut widgets = vec![WidgetMap { widget_id: "linked".into(), widget_type: "radioV2".into(), bitable_field: "fldRegression".into(), required: true, converter: Converter::Option, option_map: Default::default(), option_source_key: None, currency: None }];
            let mut cells = serde_json::Map::from_iter([("fldRegression".into(), json!("正确")), ("fldApplicant".into(), json!([{"id":E2E_APPLICANT_OPEN_ID}]))]);
            resolve_external_options(&context, &snapshot, &mut widgets, &cells, &[]).await?;
            assert_eq!(widgets[0].option_map, std::collections::BTreeMap::from([("正确".into(), format!("{source}_good"))]));

            // 三处名称都不同，手选来源必须贯穿建计划、真实落库与派发读取。
            let form = json!([{"id":"detail","name":"明细","type":"fieldList","children":[{"id":"linked","name":"付款公司名称/Company name","type":"radioV2","required":true,"externalData":{"externalDataLinkage":true}}]}]);
            let maps = serde_json::from_value::<Vec<crate::addon::feishu::domain::approval_match::FieldMapping>>(json!([
                {"widget_id":"linked","bitable_field":"fldCompany","option_source_key":source}
            ]))?;
            let transport = ReplayTransport::new(vec![(200, e2e_fields_body()), (200, e2e_definition_body(&form))]);
            let tokens = e2e_tokens();
            let plan = build_plan(&transport, &NoSleep, &tokens, &context, &ProvisionInput {
                base_token: &source, table_id: E2E_TABLE_ID, approval_code: E2E_APPROVAL_CODE,
                applicant_field: "fldApplicant", backfill_field: "fldSerial", base_timezone: "Asia/Shanghai", maps: Some(&maps),
                status_field: None, flow_field: None, start_time_field: None, end_time_field: None, department_field: None, handler_field: None,
            }).await?;
            let mut transaction = database.transaction().await?;
            let config_id = insert_plan(&context, &mut transaction, &plan).await?;
            transaction.commit().await?;
            saved_config_id = Some(config_id);
            let rows = context.approval_field_maps().query().where_eq("config_id", json!(config_id))?.all().await?;
            let rows: Vec<_> = rows.into_iter().map(yang_base::table::Record::into_map).collect();
            widgets = widget_maps_from_rows(&rows, |_| None).ok_or_else(|| anyhow::anyhow!("保存的映射无法恢复"))?;
            assert_eq!(widgets[0].option_source_key.as_deref(), Some(source.as_str()));
            context.datasource_fields().query().where_eq("source_key", json!(source))?
                .update(yang_base::table::Record::new().set("field_name", "")).await?;
            cells.insert("fldCompany".into(), json!("正确"));
            resolve_external_options(&context, &plan.form_snapshot, &mut widgets, &cells, &[]).await?;
            assert_eq!(widgets[0].option_map.get("正确"), Some(&format!("{source}_good")));
            let mut unnamed = form.clone();
            unnamed[0]["children"][0]["name"] = json!("");
            resolve_external_options(&context, &unnamed.to_string(), &mut widgets, &cells, &[]).await?;
            let coordinates = e2e_coordinates();
            let fields: Value = serde_json::from_str(&e2e_fields_body())?;
            let fields: Vec<bitable::FieldItem> =
                serde_json::from_value(fields["data"]["items"].clone())?;
            for label in ["正确", "重名", "不存在"] {
                cells.insert("fldCompany".into(), json!(label));
                let transport = ReplayTransport::new(if label == "正确" { vec![
                    (200, e2e_ok_body(json!({"instance_code":"INST-MANUAL"}))),
                    (200, e2e_ok_body(json!({"instance_code":"INST-MANUAL","serial_number":"MANUAL-001","status":"PENDING"}))),
                    (200, e2e_ok_body(json!({}))),
                ] } else { vec![] });
                let tokens = e2e_tokens();
                let backfill = BitableBackfill { transport: &transport, sleeper: &NoSleep, tokens: &tokens, coordinates: &coordinates, fields: &fields };
                let input = DispatchInput { fields: &fields, context: Some(&context), coordinates: &coordinates, record_id: E2E_RECORD_ID, cells: &cells, applicant_field: "fldApplicant", applicant: None, backfill_field_name: "审批编号", status_field: None, flow_field: None, start_time_field: None, end_time_field: None, department_field: None, handler_field: None, approval_code: E2E_APPROVAL_CODE, form_snapshot: plan.form_snapshot.clone(), widgets: &widgets, timezone_offset: FixedOffset::from_iana("Asia/Shanghai")? };
                let result = dispatch_one(&transport, &NoSleep, &tokens, &backfill, &input).await;
                if label == "正确" {
                    assert_eq!(result, DispatchResult::Backfilled { serial_number: "MANUAL-001".into(), instance_code: "INST-MANUAL".into() });
                    let create = transport.took("Post", &crate::addon::feishu::domain::approval::create_instance_url());
                    let body = create.body.ok_or_else(|| anyhow::anyhow!("缺少提单请求体"))?;
                    let sent: Value = serde_json::from_str(body["form"].as_str().ok_or_else(|| anyhow::anyhow!("缺少表单"))?)?;
                    assert_eq!(sent[0]["value"][0][0]["value"], json!(format!("{source}_good")));
                } else {
                    assert!(matches!(result, DispatchResult::Waiting { .. }), "{result:?}");
                    assert!(transport.seen().is_empty(), "等待态不得创建或回填");
                }
            }
            cells.insert("fldCompany".into(), json!("正确"));
            context.datasource_fields().query().where_eq("source_key", json!(source))?
                .update(yang_base::table::Record::new().set("enabled", false)).await?;
            assert!(resolve_external_options(&context, &plan.form_snapshot, &mut widgets, &cells, &fields).await.is_err());
            context.datasource_fields().query().where_eq("source_key", json!(source))?
                .update(yang_base::table::Record::new().set("enabled", true)).await?;
            context.datasources().query().where_eq("id", json!(datasource_id))?
                .update(yang_base::table::Record::new().set("status", "disabled")).await?;
            assert!(resolve_external_options(&context, &plan.form_snapshot, &mut widgets, &cells, &fields).await.is_err());
            Ok(())
        }.await;
        // tenant-boundary: raw-sql attachment-option-regression-cleanup
        sqlx::query("DELETE FROM feishu_option WHERE source_key IN (?, ?)")
            .bind(&source)
            .bind(format!("{source}_other"))
            .execute(pool.as_ref())
            .await?;
        // tenant-boundary: raw-sql attachment-binding-regression-cleanup
        sqlx::query("DELETE FROM feishu_datasource_field WHERE source_key = ?")
            .bind(&source)
            .execute(pool.as_ref())
            .await?;
        // tenant-boundary: raw-sql manual-option-datasource-cleanup
        sqlx::query("DELETE FROM feishu_datasource WHERE id = ?")
            .bind(datasource_id)
            .execute(pool.as_ref())
            .await?;
        if let Some(config_id) = saved_config_id {
            // tenant-boundary: raw-sql manual-option-maps-cleanup
            sqlx::query("DELETE FROM feishu_approval_field_map WHERE config_id = ?")
                .bind(config_id)
                .execute(pool.as_ref())
                .await?;
            // tenant-boundary: raw-sql manual-option-config-cleanup
            sqlx::query("DELETE FROM feishu_approval_config WHERE id = ?")
                .bind(config_id)
                .execute(pool.as_ref())
                .await?;
        }
        outcome
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
                maps: None,
                status_field: None,
                flow_field: None,
                start_time_field: None,
                end_time_field: None,
                department_field: None,
                handler_field: None,
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
            fields: &fields,
        };
        let result = dispatch_one(
            transport.as_ref(),
            &sleeper,
            &tokens,
            &backfill,
            &OrchestrationInput {
                fields: &[],
                context: Some(&context),
                coordinates: &coordinates,
                record_id: E2E_RECORD_ID,
                cells: &cells,
                applicant_field: &plan.applicant_field_id,
                applicant: None,
                backfill_field_name: &backfill_field_name,
                approval_code: E2E_APPROVAL_CODE,
                widgets: &plan.widgets,
                form_snapshot: plan.form_snapshot.clone(),
                status_field: None,
                flow_field: None,
                start_time_field: None,
                end_time_field: None,
                department_field: None,
                handler_field: None,
                timezone_offset: FixedOffset::from_iana("Asia/Shanghai")
                    .unwrap_or_else(|error| panic!("{error}")),
            },
        )
        .await;
        assert_eq!(
            result,
            DispatchResult::Backfilled {
                serial_number: "202609280001".to_string(),
                instance_code: "INST-E2E-001".to_string(),
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
                maps: None,
                status_field: None,
                flow_field: None,
                start_time_field: None,
                end_time_field: None,
                department_field: None,
                handler_field: None,
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
        let fields = e2e_fields_body();
        let fields: Value = serde_json::from_str(&fields).unwrap_or_else(|e| panic!("{e}"));
        let fields: Vec<bitable::FieldItem> =
            serde_json::from_value(fields["data"]["items"].clone())
                .unwrap_or_else(|e| panic!("{e}"));
        let backfill = BitableBackfill {
            transport: transport.as_ref(),
            sleeper: &sleeper,
            tokens: &tokens,
            coordinates: &coordinates,
            fields: &fields,
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

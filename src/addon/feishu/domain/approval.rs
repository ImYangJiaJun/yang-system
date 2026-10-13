//! 飞书审批（approval v4）的出站客户端。
//!
//! 三个接口：
//!
//! - `POST /open-apis/approval/v4/instances` —— 创建审批实例。
//! - `GET /open-apis/approval/v4/instances/:instance_id` —— 取实例详情。
//! - `GET /open-apis/approval/v4/approvals/:approval_code` —— 取审批定义。
//!
//! # 创建接口不返回 `serial_number`
//!
//! 创建成功只回 `instance_code`。审批单编号 `serial_number` 必须再调一次实例详情
//! 才能拿到。所以每条记录是**两次**调用：创建（100 次/分钟，瓶颈）与详情
//! （1000 次/分钟、50 次/秒，非瓶颈）。
//!
//! # `instance_id` 位置可以直接传 `uuid`
//!
//! 官方明文：「如果在创建的时候传了 uuid 参数，则本参数也可以通过传 uuid 获取
//! 指定审批实例详情」。这是 `60012`（uuid 冲突）唯一的恢复路径——冲突的响应体
//! **不含** `instance_code`，所以除了用 uuid 反查，没有别的办法拿回实例。
//!
//! # 创建请求必须标幂等
//!
//! `uuid` 是服务端幂等键（「同一个 uuid 只能用于创建一个审批实例」），所以重发
//! 安全。这条不是可选的优化：`outbound::disposition` 在 `idempotent == false`
//! 时对 Retry 类失败**直接 Give**，标 false 等于零重试层。

#![allow(dead_code)] // 客户端先落地并自带测试；消费者（派发编排）在后续任务接入。

use anyhow::ensure;
use serde::Deserialize;

use super::outbound::{
    send_with_retry, FailureKind, OutboundFailure, OutboundMethod, OutboundRequest,
    OutboundTransport, Sleeper, PULL_RETRY,
};
use super::tenant_token::{TenantTokenProvider, FEISHU_OPEN_BASE};

/// 创建实例的请求超时（秒）。比拉取宽松：单次调用要写库并触发审批流。
pub(crate) const APPROVAL_REQUEST_TIMEOUT_SECS: u64 = 20;

/// 路径段长度上限，与 `bitable` 同源。
const MAX_PATH_SEGMENT_LEN: usize = 128;

/// 创建审批实例的 URL。
pub(crate) fn create_instance_url() -> String {
    format!("{FEISHU_OPEN_BASE}/open-apis/approval/v4/instances")
}

/// 审批实例详情 / 审批定义的 URL。
///
/// `instance_id` 既可以是 `instance_code`，**也可以是创建时传入的 `uuid``** ——
/// 后者是 `60012` 冲突后唯一的反查手段。
pub(crate) fn instance_detail_url(instance_id: &str) -> anyhow::Result<String> {
    validate_path_segment("instance_id", instance_id)?;
    Ok(format!(
        "{FEISHU_OPEN_BASE}/open-apis/approval/v4/instances/{instance_id}"
    ))
}

/// 官方审批 AppLink：PC/手机路径分别编码，实例 Code 与数字 ID 都受支持。
pub(crate) fn instance_applink(instance_code: &str) -> anyhow::Result<String> {
    super::bitable::validate_path_segment("instance_code", instance_code)?;
    Ok(format!(
        "https://applink.feishu.cn/client/mini_program/open?appId=cli_9cb844403dbb9108&mode=appCenter&path_pc=pc%2Fpages%2Fin-process%2Findex%3FinstanceId%3D{instance_code}%26source%3Dbitable&path=pages%2Fdetail%2Findex%3FinstanceId%3D{instance_code}%26source%3Dbitable"
    ))
}

/// 审批定义详情的 URL。
pub(crate) fn approval_definition_url(approval_code: &str) -> anyhow::Result<String> {
    validate_path_segment("approval_code", approval_code)?;
    Ok(format!(
        "{FEISHU_OPEN_BASE}/open-apis/approval/v4/approvals/{approval_code}"
    ))
}

/// 与 `bitable` 同一份校验：路径段里混进斜杠或查询串会让请求打到别的端点。
fn validate_path_segment(name: &str, value: &str) -> anyhow::Result<()> {
    ensure!(!value.is_empty(), "{name} 不能为空");
    ensure!(
        value.len() <= MAX_PATH_SEGMENT_LEN,
        "{name} 超长（{} > {MAX_PATH_SEGMENT_LEN}）",
        value.len()
    );
    ensure!(
        !value.contains(['/', '?', '#', ' ', '\\']),
        "{name} 含非法字符：{value}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 创建审批实例
// ---------------------------------------------------------------------------

/// 创建实例的结果。
///
/// `UuidConflict` **不是失败**：它的语义是「该 uuid 已创建过实例，只是上一次的
/// 响应丢了」。处置是按 uuid 反查实例详情取回 `instance_code`，而不是写错误信息
/// ——后者会把一条真实存在的审批单钉死成失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CreateOutcome {
    /// 创建成功，返回 `instance_code`。
    Created { instance_code: String },
    /// uuid 已被占用（`60012`）：实例此前已建成功，需按 uuid 反查。
    UuidConflict,
}

/// 创建审批实例。
///
/// `form` 必须是**压缩后的 JSON 数组字符串**（由 `approval_convert::build_form`
/// 产出），不是 JSON 对象。
pub(crate) async fn create_instance(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    approval_code: &str,
    form: &str,
    open_id: &str,
    uuid: &str,
) -> Result<CreateOutcome, OutboundFailure> {
    let body = serde_json::json!({
        "approval_code": approval_code,
        "open_id": open_id,
        "form": form,
        // uuid 是服务端幂等键。**不要删**：崩在「create 成功、本地未落库」这条
        // 缝里时，它是唯一能把实例捞回来的东西（配合 instance_detail_url 传 uuid）。
        "uuid": uuid,
    });

    let request = OutboundRequest {
        method: OutboundMethod::Post,
        url: create_instance_url(),
        query: Vec::new(),
        bearer_token: None, // 下面逐次取 token 后填入
        json_body: Some(body),
        raw_body: None,
        binary_limit: None,
        timeout_secs: Some(APPROVAL_REQUEST_TIMEOUT_SECS),
        // **必须为 true**：唯一依据是 uuid 的服务端幂等（官方明文「同一个 uuid
        // 只能用于创建一个审批实例，如果冲突则创建失败并返回错误码 60012」）。
        //
        // 不要因为「POST 有副作用」把它改回 false——`disposition` 在
        // `idempotent == false` 时对 Retry 类失败直接 Give，那样 429/超时/5xx
        // 一次都不会退避，与「系统内部退避重试」的设计正好相反。
        // `approval::tests` 有一条正向断言钉住这条不变式。
        idempotent: true,
    };

    match send_approval(transport, sleeper, tokens, request).await {
        Ok(response) => {
            let data: CreateInstanceData =
                serde_json::from_str(&response).map_err(|error| OutboundFailure {
                    kind: FailureKind::Fatal { code: 0 },
                    message: format!("创建审批实例响应解析失败: {error}"),
                })?;
            Ok(CreateOutcome::Created {
                instance_code: data.instance_code,
            })
        }
        // 60012 不走 Err：它由 classify 单列成 `UuidConflict`，这里映射成
        // 「需反查」的结果，让调用方走恢复路径而不是失败路径。
        Err(failure) if failure.kind == FailureKind::UuidConflict => {
            Ok(CreateOutcome::UuidConflict)
        }
        Err(failure) => Err(failure),
    }
}

#[derive(Debug, Deserialize)]
struct CreateInstanceData {
    instance_code: String,
}

// ---------------------------------------------------------------------------
// 实例详情
// ---------------------------------------------------------------------------

/// 审批实例详情里本模块关心的字段。
///
/// # 两形态并存（bot / user）
///
/// 生产（tenant token / bot）与 lark-cli（user token）走**同一个**
/// `GET /approval/v4/instances/:instance_id`，却返回**两套并存的字段形态**
/// （真机已核，设计文档 `2026-10-12-feishu-approval-backfill-initiator-tracking-design.md`
/// §4.1）。**bot 形态是生产实际在用的，本结构体以 bot 形态为准**：
///
/// - bot：`approval_code` / `approval_name` / `task_list[]` / `timeline[]` /
///   `uuid` / `user_id`（工号）/ 独立 `open_id` / `department_id`（**非 `od-`
///   前缀**）……；**无** `current_nodes` / `tasks` / `operation_records` /
///   `definition_code`。
/// - user：`definition_code` / `tasks[]` / `current_nodes[]` /
///   `operation_records[]` / `department_id`（`od-` 前缀）……。
///
/// 两套形态里属于另一套的键在解析时缺省为 `None`（`serde(default)`），
/// 互不干扰——即「两形态都能解析不炸」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstanceDetail {
    pub(crate) instance_code: String,
    pub(crate) approval_code: Option<String>,
    /// 审批单编号。**可能为空**——文档未承诺创建后立即可查，所以用 `Option`
    /// 而不是报错。
    pub(crate) serial_number: Option<String>,
    /// 审批实例状态（`PENDING` / `APPROVED` / …）。当前不回流多维表格，
    /// 但落库留痕便于将来加回流时不必回补历史。
    pub(crate) status: Option<String>,
    /// 发起时间：**字符串**毫秒时间戳（如 `"1790750409857"`）。两形态同形。
    pub(crate) start_time: Option<String>,
    /// 完成时间：未完成是字符串 `"0"`（不是 null、不是缺字段、不是数字 0）。
    /// 回填时须按 `!= "0"` 判空——直接写 0 会在日期列显示 1970。
    pub(crate) end_time: Option<String>,
    /// 发起人 open_id（bot 形态的独立 `open_id` 字段）。
    pub(crate) open_id: Option<String>,
    /// 发起人工号（bot 形态）。**注意**：user 形态此键也是 `user_id`，但值是
    /// `ou_` 前缀 open_id，语义与 bot 形态不同。
    pub(crate) user_id: Option<String>,
    /// 发起人部门 ID。**格式两形态不一致**：bot 形态**非** `od-` 前缀
    /// （实测 `"db59a45c851464gf"`），user 形态是 `od-` 前缀。能否直接喂
    /// contact 部门接口未验证（设计 §4.3）。
    pub(crate) department_id: Option<String>,
    /// 创建时传入的幂等键 `uuid`（bot 形态回显）。
    pub(crate) uuid: Option<String>,
    /// 退回标记（独立布尔；撤回走 `status="CANCELED"`，不是 `reverted`）。
    pub(crate) reverted: Option<bool>,
    /// 任务/节点列表（bot 形态键名 `task_list`）。`status=PENDING` 即当前卡点。
    pub(crate) task_list: Option<Vec<TaskNode>>,
    /// 流程动态（bot 形态键名 `timeline`）。
    pub(crate) timeline: Option<Vec<TimelineRecord>>,
}

/// `task_list[]` 的一个节点/任务（bot 形态）。
///
/// 字段名对应 JSON 键；`type` 是 Rust 关键字，用 `r#type`（serde 自动映射回
/// `"type"`）。全部 `Option`：两形态字段集不同，缺省即 `None`，不让解析失败。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct TaskNode {
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) node_id: Option<String>,
    #[serde(default)]
    pub(crate) node_name: Option<String>,
    #[serde(default)]
    pub(crate) user_id: Option<String>,
    #[serde(default)]
    pub(crate) open_id: Option<String>,
    /// 节点级状态枚举比实例级宽（实测见 `DONE`）。
    #[serde(default)]
    pub(crate) status: Option<String>,
    #[serde(default)]
    pub(crate) r#type: Option<String>,
    #[serde(default)]
    pub(crate) start_time: Option<String>,
    #[serde(default)]
    pub(crate) end_time: Option<String>,
}

/// `timeline[]` 的一条流程动态（bot 形态）。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct TimelineRecord {
    /// 事件类型（`START` / `CANCEL` / …）。
    #[serde(default)]
    pub(crate) r#type: Option<String>,
    #[serde(default)]
    pub(crate) create_time: Option<String>,
    #[serde(default)]
    pub(crate) user_id: Option<String>,
    #[serde(default)]
    pub(crate) open_id: Option<String>,
    /// 扩展信息（各事件类型不同，统一按原始字符串收）。
    #[serde(default)]
    pub(crate) ext: Option<String>,
}

/// 按 `instance_code` **或 `uuid`** 取实例详情的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GetOutcome {
    // `Box` 装箱：`InstanceDetail` 含两个 `Vec<TaskNode/TimelineRecord>` 与
    // 十余 `Option<String>`，与 `NotFound` 单元变体体量差超 clippy
    // `large_enum_variant` 阈值（本仓库 deny 告警）。装箱后调用点统一
    // `*detail` 解引用，语义不变。
    Found(Box<InstanceDetail>),
    /// 实例不存在（`1390003`）。调用方据此区分「并发窗口，实例还没建好」
    /// 与「实例已建但查询失败」——这两者的处置相反。
    NotFound,
}

/// 取审批实例详情。
///
/// `instance_id` 传 `uuid` 时即「反查」——这是 `60012` 冲突后的恢复路径。
pub(crate) async fn get_instance(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    instance_id: &str,
) -> Result<GetOutcome, OutboundFailure> {
    let url = instance_detail_url(instance_id).map_err(|error| OutboundFailure {
        kind: FailureKind::Fatal { code: 0 },
        message: error.to_string(),
    })?;

    let request = OutboundRequest {
        method: OutboundMethod::Get,
        url,
        query: detail_query(),
        bearer_token: None,
        json_body: None,
        raw_body: None,
        binary_limit: None,
        timeout_secs: Some(APPROVAL_REQUEST_TIMEOUT_SECS),
        idempotent: true, // 只读
    };

    match send_approval(transport, sleeper, tokens, request).await {
        Ok(response) => {
            let data: InstanceDetailData =
                serde_json::from_str(&response).map_err(|error| OutboundFailure {
                    kind: FailureKind::Fatal { code: 0 },
                    message: format!("审批实例详情响应解析失败: {error}"),
                })?;
            // 新字段全部来自这一份响应，**零额外 API 调用**。
            Ok(GetOutcome::Found(Box::new(InstanceDetail {
                instance_code: data.instance_code,
                approval_code: data.approval_code,
                serial_number: data.serial_number.filter(|value| !value.is_empty()),
                status: data.status,
                start_time: data.start_time,
                end_time: data.end_time,
                open_id: data.open_id,
                user_id: data.user_id,
                department_id: data.department_id,
                uuid: data.uuid,
                reverted: data.reverted,
                task_list: data.task_list,
                timeline: data.timeline,
            })))
        }
        // 实例不存在：调用方据此决定「回到 create 重试」还是「退避」。
        Err(failure) if failure.kind == FailureKind::InstanceNotFound => Ok(GetOutcome::NotFound),
        Err(failure) => Err(failure),
    }
}

#[derive(Debug, Deserialize)]
struct InstanceDetailData {
    instance_code: String,
    // `approval_code` / `serial_number` / `status` 在 bot 形态里都有值，保留；
    // user 形态缺省即 `None`。
    #[serde(default)]
    approval_code: Option<String>,
    #[serde(default)]
    serial_number: Option<String>,
    #[serde(default)]
    status: Option<String>,
    // 以下均为 bot 形态字段，两形态中另一套的键缺省为 None。
    #[serde(default)]
    start_time: Option<String>,
    #[serde(default)]
    end_time: Option<String>,
    #[serde(default)]
    open_id: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    department_id: Option<String>,
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    reverted: Option<bool>,
    #[serde(default)]
    task_list: Option<Vec<TaskNode>>,
    #[serde(default)]
    timeline: Option<Vec<TimelineRecord>>,
}

// ---------------------------------------------------------------------------
// 审批定义
// ---------------------------------------------------------------------------

/// 审批定义里本模块关心的部分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApprovalDefinition {
    pub(crate) approval_name: String,
    /// 控件结构快照（原始 JSON），存进配置表供保存期校验用。
    pub(crate) form: Option<serde_json::Value>,
    /// 是否为三方审批定义。三方定义不能用 `instances create` 提单。
    pub(crate) is_external: bool,
}

/// 取审批定义详情。
pub(crate) async fn get_approval_definition(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    approval_code: &str,
) -> Result<ApprovalDefinition, OutboundFailure> {
    let url = approval_definition_url(approval_code).map_err(|error| OutboundFailure {
        kind: FailureKind::Fatal { code: 0 },
        message: error.to_string(),
    })?;

    let request = OutboundRequest {
        method: OutboundMethod::Get,
        url,
        query: Vec::new(),
        bearer_token: None,
        json_body: None,
        raw_body: None,
        binary_limit: None,
        timeout_secs: Some(APPROVAL_REQUEST_TIMEOUT_SECS),
        idempotent: true, // 只读
    };

    let response = send_approval(transport, sleeper, tokens, request).await?;
    let data: ApprovalDefinitionData =
        serde_json::from_str(&response).map_err(|error| OutboundFailure {
            kind: FailureKind::Fatal { code: 0 },
            message: format!("审批定义响应解析失败: {error}"),
        })?;
    Ok(ApprovalDefinition {
        approval_name: data.approval_name,
        form: data.form,
        is_external: data.is_external.unwrap_or(false),
    })
}

#[derive(Debug, Deserialize)]
struct ApprovalDefinitionData {
    approval_name: String,
    #[serde(default)]
    form: Option<serde_json::Value>,
    #[serde(default)]
    is_external: Option<bool>,
}

// ---------------------------------------------------------------------------
// 共用：发请求 + 取 token + token 失效补救
// ---------------------------------------------------------------------------

/// 取 token、发请求、解码信封里的 `data`。
///
/// 与 `bitable::send_json` 同一套补救（「token 失效 → 清缓存强制刷新一次」），
/// 但**不复用那个函数**——它是 `bitable` 的私有实现且绑定多维表格的语义。
/// 两处共用的部分目前只有这十几行，且各自的错误文案不同（一处说「拉取」，
/// 一处说「审批」），强行抽公共层会让两边的排查线索都变模糊。
///
/// 返回**信封里的 `data` 序列化文本**，由各调用方自己反序列化成结构体。
async fn send_approval(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    request: OutboundRequest,
) -> Result<String, OutboundFailure> {
    let mut token_refresh_used = false;
    let mut request = request;
    loop {
        let token = tokens
            .tenant_access_token()
            .await
            .map_err(|error| OutboundFailure {
                kind: FailureKind::Fatal { code: 0 },
                message: error.to_string(),
            })?;
        request.bearer_token = Some(token);

        match send_with_retry(transport, sleeper, &request, PULL_RETRY).await {
            Ok(response) => return decode_data(&response.body),
            Err(failure) => {
                if failure.kind == FailureKind::TokenExpired && !token_refresh_used {
                    token_refresh_used = true;
                    tracing::warn!("飞书 tenant_access_token 失效，清缓存并强制刷新一次");
                    tokens
                        .invalidate_and_refresh()
                        .await
                        .map_err(|error| OutboundFailure {
                            kind: FailureKind::Fatal { code: 0 },
                            message: format!("tenant_access_token 失效后强制刷新失败: {error}"),
                        })?;
                    continue;
                }
                return Err(failure);
            }
        }
    }
}

/// 从信封里取出 `data` 并序列化成文本。
fn decode_data(response: &str) -> Result<String, OutboundFailure> {
    #[derive(Deserialize)]
    struct Envelope {
        data: Option<serde_json::Value>,
    }
    let envelope: Envelope = serde_json::from_str(response).map_err(|error| OutboundFailure {
        kind: FailureKind::Fatal { code: 0 },
        message: format!("响应信封解析失败: {error}"),
    })?;
    let data = envelope.data.ok_or_else(|| OutboundFailure {
        kind: FailureKind::Fatal { code: 0 },
        message: "响应信封里没有 data".to_string(),
    })?;
    serde_json::to_string(&data).map_err(|error| OutboundFailure {
        kind: FailureKind::Fatal { code: 0 },
        message: format!("data 序列化失败: {error}"),
    })
}

/// 实例详情的查询参数。
///
/// 显式要 `open_id` 形态：默认值也是 `open_id`，但写出来才能挡住「顺手改成
/// user_id」——那会多要一个通讯录字段权限（`contact:user.employee_id:readonly`），
/// 而本模块只用得到 `serial_number` 与 `status`。
fn detail_query() -> Vec<(String, String)> {
    vec![("user_id_type".to_string(), "open_id".to_string())]
}

// ---------------------------------------------------------------------------
// 姓名 → open_id 解析（通讯录搜索）
// ---------------------------------------------------------------------------

/// 搜索用户的 URL。
///
/// `POST /open-apis/contact/v3/users/search`（lark-cli `contact +search-user`
/// dry-run 实测的路径与请求体 `{"query": …}`）。**不要**与另一条也叫「搜索用户」
/// 的 `GET /open-apis/search/v1/user` 混用——两者路径、响应列表键与姓名键都不同，
/// 见下方结构体的容错说明。
pub(crate) fn applicant_search_url() -> String {
    format!("{FEISHU_OPEN_BASE}/open-apis/contact/v3/users/search")
}

/// 姓名 → open_id 的解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ApplicantResolution {
    /// 唯一命中。
    Resolved { open_id: String },
    /// 无命中；或模糊命中多于一个、不足以定人。
    NotFound,
    /// 多个同名命中，需人工消歧。`candidates` 为「姓名(open_id)」描述串。
    Ambiguous { candidates: Vec<String> },
}

/// 按姓名解析 open_id。
///
/// 只查第一页（`page_size=20`）：姓名精确匹配场景候选极少，翻页只会多打接口。
/// `ponytail:` 若同名者多到超过一页、且真正的同名命中落在第二页，本函数会漏判；
/// 届时再补分页遍历。
pub(crate) async fn resolve_applicant(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    name: &str,
) -> Result<ApplicantResolution, OutboundFailure> {
    let name = name.trim();
    // 空姓名不打接口：搜索接口对空 query 无意义，且会白烧一次出站配额。
    if name.is_empty() {
        return Ok(ApplicantResolution::NotFound);
    }

    let request = OutboundRequest {
        method: OutboundMethod::Post,
        url: applicant_search_url(),
        query: vec![("page_size".to_string(), "20".to_string())],
        bearer_token: None,
        json_body: Some(serde_json::json!({ "query": name })),
        raw_body: None,
        binary_limit: None,
        timeout_secs: Some(APPROVAL_REQUEST_TIMEOUT_SECS),
        idempotent: true, // 只读
    };

    let response = send_approval(transport, sleeper, tokens, request).await?;
    let data: ApplicantSearchData =
        serde_json::from_str(&response).map_err(|error| OutboundFailure {
            kind: FailureKind::Fatal { code: 0 },
            message: format!("搜索用户响应解析失败: {error}"),
        })?;
    Ok(classify_applicants(name, &data.items))
}

/// 搜索接口 `data` 部分的解析。
///
/// **列表键两种形态都收**：本仓库核对到的两条「搜索用户」接口返回的列表键不同
/// ——`contact/v3/users/search` 与同族的 `contact/v3/departments/search` 用
/// `items`，而 `search/v1/user` 用 `users`。用 `alias` 同时接受，避免上游换接口时
/// 静默解析成空列表（那会把「有这人」误判成 NotFound）。
#[derive(Debug, Deserialize)]
struct ApplicantSearchData {
    #[serde(default, alias = "users")]
    items: Vec<SearchUser>,
}

/// 搜索命中的单个用户，只取解析所需的两字段。
///
/// 姓名键同样两种形态都收：`name`（`search/v1/user` / `departments/search` 用）
/// 与 `localized_name`（lark-cli `searchUserAPIItem` 的展示名字段）。
#[derive(Debug, Deserialize)]
struct SearchUser {
    #[serde(default)]
    open_id: String,
    #[serde(default, alias = "localized_name")]
    name: String,
}

/// 命中收敛规则（纯函数，便于单测）。
///
/// 1. 精确等值（trim 后 ==）恰 1 个 → `Resolved`；
/// 2. 精确等值 > 1 个 → `Ambiguous`（重名，需消歧）；
/// 3. 无精确命中但整页恰 1 个模糊命中 → `Resolved`；
/// 4. 其余（完全无命中 / 模糊命中多于一个）→ `NotFound`。
fn classify_applicants(name: &str, users: &[SearchUser]) -> ApplicantResolution {
    let exact: Vec<&SearchUser> = users
        .iter()
        .filter(|user| user.name.trim() == name)
        .collect();
    match exact.as_slice() {
        [only] => ApplicantResolution::Resolved {
            open_id: only.open_id.clone(),
        },
        [] => match users {
            [only] => ApplicantResolution::Resolved {
                open_id: only.open_id.clone(),
            },
            _ => ApplicantResolution::NotFound,
        },
        many => ApplicantResolution::Ambiguous {
            candidates: many
                .iter()
                .map(|user| format!("{}({})", user.name.trim(), user.open_id))
                .collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use yang_base::BaseError;

    use super::super::outbound::OutboundResponse;
    use super::super::tenant_token::{FeishuCredentials, TenantTokenCache};

    #[test]
    fn create_url_is_the_official_path() {
        assert!(create_instance_url().ends_with("/open-apis/approval/v4/instances"));
    }

    #[test]
    fn detail_url_accepts_a_uuid() {
        // 官方明文：创建时传了 uuid，则 instance_id 位置也可以传 uuid。
        // 这是 60012 冲突后唯一的反查手段。
        let url = instance_detail_url("7C468A54-8745-2245-9675-08B7C63E7A87")
            .unwrap_or_else(|error| panic!("应可构造: {error}"));
        assert!(
            url.ends_with("/instances/7C468A54-8745-2245-9675-08B7C63E7A87"),
            "{url}"
        );
    }

    #[test]
    fn path_segments_are_validated() {
        assert!(instance_detail_url("bad/id").is_err());
        assert!(approval_definition_url("bad?id=1").is_err());
        assert!(instance_detail_url("").is_err());
    }

    #[test]
    fn definition_url_is_the_official_path() {
        let url = approval_definition_url("4202AD96-9EC1-4284-9C48-B923CDC4F30B")
            .unwrap_or_else(|error| panic!("应可构造: {error}"));
        assert!(
            url.ends_with("/approvals/4202AD96-9EC1-4284-9C48-B923CDC4F30B"),
            "{url}"
        );
    }

    #[test]
    fn detail_request_asks_for_open_id_shape() {
        // 不请求 user_id 形态：那会多要一个通讯录字段权限
        // （contact:user.employee_id:readonly），本模块用不到。
        assert_eq!(
            detail_query(),
            vec![("user_id_type".to_string(), "open_id".to_string())]
        );
    }

    #[test]
    fn instance_detail_parses_serial_number_and_status() {
        let json = r#"{
            "instance_code": "81D31358-93AF-92D6-7425-01A5D67C4E71",
            "serial_number": "202609280001",
            "status": "APPROVED",
            "approval_name": "报销申请"
        }"#;
        let data: InstanceDetailData =
            serde_json::from_str(json).unwrap_or_else(|error| panic!("应可解析: {error}"));
        assert_eq!(data.instance_code, "81D31358-93AF-92D6-7425-01A5D67C4E71");
        assert_eq!(data.serial_number.as_deref(), Some("202609280001"));
        assert_eq!(data.status.as_deref(), Some("APPROVED"));
    }

    #[test]
    fn instance_detail_tolerates_missing_serial_number() {
        // 文档未承诺创建后 serial_number 立即可查，所以缺字段不能是解析错误——
        // 那会把「稍后再查」误报成「响应坏掉」。
        let json = r#"{"instance_code": "abc"}"#;
        let data: InstanceDetailData =
            serde_json::from_str(json).unwrap_or_else(|error| panic!("缺字段应可解析: {error}"));
        assert_eq!(data.serial_number, None);
        assert_eq!(data.status, None);
    }

    #[test]
    fn instance_detail_parses_bot_shape_task_list_and_timeline() {
        // 合成 bot（tenant token）形态响应：生产实际在用的形态，字段以它为准。
        // task_list 两个节点（DONE / PENDING），timeline 一条 START 一条 CANCEL，
        // end_time 是毫秒串（区别于未完成的 "0"）。
        let json = r#"{
            "instance_code": "9B0F4A3E-DC7F-4CF5-BE87-75EFD3D7DA67",
            "approval_code": "D0557DA6-CC9A-4B6B-BDF2-8DC675D2DD6E",
            "approval_name": "技术测试专用",
            "serial_number": "202609300082",
            "status": "PENDING",
            "uuid": "7C468A54-8745-2245-9675-08B7C63E7A87",
            "user_id": "eabadgcg",
            "open_id": "ou_aad06cf4f5c2c081cf273829cb9d080d",
            "department_id": "db59a45c851464gf",
            "start_time": "1790750409857",
            "end_time": "1790751000000",
            "reverted": false,
            "task_list": [
                {
                    "id": "1", "node_id": "n1", "node_name": "审批",
                    "user_id": "eabadgcg", "open_id": "ou_a", "status": "DONE",
                    "type": "AND", "start_time": "1790750410128", "end_time": "1790750500000"
                },
                {
                    "id": "2", "node_id": "n2", "node_name": "复核",
                    "user_id": "eabadgcg", "open_id": "ou_b", "status": "PENDING",
                    "type": "AND", "start_time": "1790750500000", "end_time": "0"
                }
            ],
            "timeline": [
                { "type": "START", "create_time": "1790750409857", "user_id": "eabadgcg", "open_id": "ou_a", "ext": "" },
                { "type": "CANCEL", "create_time": "1790751000000", "user_id": "eabadgcg", "open_id": "ou_a", "ext": "" }
            ]
        }"#;
        let data: InstanceDetailData =
            serde_json::from_str(json).unwrap_or_else(|error| panic!("应可解析: {error}"));

        assert_eq!(data.instance_code, "9B0F4A3E-DC7F-4CF5-BE87-75EFD3D7DA67");
        assert_eq!(
            data.approval_code.as_deref(),
            Some("D0557DA6-CC9A-4B6B-BDF2-8DC675D2DD6E")
        );
        assert_eq!(data.serial_number.as_deref(), Some("202609300082"));
        assert_eq!(data.status.as_deref(), Some("PENDING"));
        assert_eq!(data.start_time.as_deref(), Some("1790750409857"));
        assert_eq!(data.end_time.as_deref(), Some("1790751000000"));
        assert_eq!(
            data.open_id.as_deref(),
            Some("ou_aad06cf4f5c2c081cf273829cb9d080d")
        );
        assert_eq!(data.user_id.as_deref(), Some("eabadgcg"));
        assert_eq!(data.department_id.as_deref(), Some("db59a45c851464gf"));
        assert_eq!(
            data.uuid.as_deref(),
            Some("7C468A54-8745-2245-9675-08B7C63E7A87")
        );
        assert_eq!(data.reverted, Some(false));

        let tasks = data.task_list.as_deref().unwrap_or_else(|| panic!("应有 task_list"));
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].node_name.as_deref(), Some("审批"));
        assert_eq!(tasks[0].r#type.as_deref(), Some("AND"));
        assert_eq!(tasks[0].status.as_deref(), Some("DONE"));
        assert_eq!(tasks[0].end_time.as_deref(), Some("1790750500000"));
        assert_eq!(tasks[1].status.as_deref(), Some("PENDING"));
        assert_eq!(tasks[1].end_time.as_deref(), Some("0"));

        let timeline = data.timeline.as_deref().unwrap_or_else(|| panic!("应有 timeline"));
        assert_eq!(timeline.len(), 2);
        assert_eq!(timeline[0].r#type.as_deref(), Some("START"));
        assert_eq!(timeline[0].create_time.as_deref(), Some("1790750409857"));
        assert_eq!(timeline[1].r#type.as_deref(), Some("CANCEL"));
    }

    #[test]
    fn instance_detail_parses_user_shape_without_bot_keys() {
        // user（lark-cli）形态：键是 definition_code / tasks / current_nodes /
        // operation_records，department_id 带 `od-` 前缀。这些 bot 专属键
        // （approval_code / task_list / timeline / uuid / open_id）应落 None，
        // 而两形态共有的键照常解析——钉住「两形态都能解析不炸」。
        let json = r#"{
            "instance_code": "9B0F4A3E-DC7F-4CF5-BE87-75EFD3D7DA67",
            "definition_code": "D0557DA6-CC9A-4B6B-BDF2-8DC675D2DD6E",
            "definition_name": "技术测试专用",
            "serial_number": "202609300082",
            "status": "PENDING",
            "start_time": "1790750409857",
            "end_time": "0",
            "reverted": false,
            "department_id": "od-e2ebe89e1e313c2a3bf5fc064bfa6f4c",
            "user_id": "ou_aad06cf4f5c2c081cf273829cb9d080d",
            "current_nodes": [
                { "node_id": "n1", "node_name": "审批", "type": "AND",
                  "approvers": [{ "task_id": "1", "user_id": "ou_a" }] }
            ],
            "tasks": [
                { "id": "1", "node_id": "n1", "node_name": "审批", "user_id": "ou_a",
                  "status": "PENDING", "type": "AND", "start_time": "1790750410128", "end_time": "0" }
            ],
            "operation_records": [
                { "type": "START", "create_time": "1790750409857", "user_id": "ou_a" }
            ],
            "comments": []
        }"#;
        let data: InstanceDetailData =
            serde_json::from_str(json).unwrap_or_else(|error| panic!("应可解析: {error}"));

        // bot 专属键在 user 形态下不存在 → None。
        assert_eq!(data.approval_code, None);
        assert_eq!(data.task_list, None);
        assert_eq!(data.timeline, None);
        assert_eq!(data.uuid, None);
        assert_eq!(data.open_id, None);
        // 两形态共有的键照常解析（user 形态 department_id 带 od- 前缀）。
        assert_eq!(data.instance_code, "9B0F4A3E-DC7F-4CF5-BE87-75EFD3D7DA67");
        assert_eq!(data.serial_number.as_deref(), Some("202609300082"));
        assert_eq!(data.status.as_deref(), Some("PENDING"));
        assert_eq!(data.start_time.as_deref(), Some("1790750409857"));
        assert_eq!(data.end_time.as_deref(), Some("0"));
        assert_eq!(data.reverted, Some(false));
        assert_eq!(
            data.department_id.as_deref(),
            Some("od-e2ebe89e1e313c2a3bf5fc064bfa6f4c")
        );
        assert_eq!(
            data.user_id.as_deref(),
            Some("ou_aad06cf4f5c2c081cf273829cb9d080d")
        );
    }

    #[test]
    fn empty_serial_number_is_normalised_to_none() {
        // 空串与缺失是同一件事：都表示「现在还没有编号」。
        // 直接用 `Some("")` 会让多维度表格被回填成一个空单元格。
        let detail = GetOutcome::Found(Box::new(InstanceDetail {
            instance_code: "abc".to_string(),
            approval_code: None,
            serial_number: Some(String::new()).filter(|value| !value.is_empty()),
            status: None,
            start_time: None,
            end_time: None,
            open_id: None,
            user_id: None,
            department_id: None,
            uuid: None,
            reverted: None,
            task_list: None,
            timeline: None,
        }));
        match detail {
            GetOutcome::Found(detail) => assert_eq!(detail.serial_number, None),
            GetOutcome::NotFound => panic!("不应是 NotFound"),
        }
    }

    #[test]
    fn approval_definition_parses_form_and_external_flag() {
        let json = r#"{
            "approval_name": "报销申请",
            "form": "[{\"id\":\"w1\",\"type\":\"input\",\"required\":true}]",
            "is_external": false
        }"#;
        let data: ApprovalDefinitionData =
            serde_json::from_str(json).unwrap_or_else(|error| panic!("应可解析: {error}"));
        assert_eq!(data.approval_name, "报销申请");
        assert!(data.form.is_some());
        assert_eq!(data.is_external, Some(false));
    }

    #[test]
    fn external_flag_defaults_to_false_when_absent() {
        // 缺字段时按「非三方定义」处理：三方定义要显式标 `is_external: true`。
        let json = r#"{"approval_name": "报销申请"}"#;
        let data: ApprovalDefinitionData =
            serde_json::from_str(json).unwrap_or_else(|error| panic!("应可解析: {error}"));
        assert_eq!(data.is_external, None);
    }

    #[test]
    fn create_response_parses_instance_code() {
        let json = r#"{"instance_code": "81D31358-93AF-92D6-7425-01A5D67C4E71"}"#;
        let data: CreateInstanceData =
            serde_json::from_str(json).unwrap_or_else(|error| panic!("应可解析: {error}"));
        assert_eq!(data.instance_code, "81D31358-93AF-92D6-7425-01A5D67C4E71");
    }

    #[test]
    fn decode_data_extracts_the_data_field() {
        let envelope = r#"{"code":0,"msg":"success","data":{"instance_code":"abc"}}"#;
        let data = decode_data(envelope).unwrap_or_else(|error| panic!("{error}"));
        assert!(data.contains("instance_code"), "{data}");
    }

    #[test]
    fn decode_data_errors_when_data_is_absent() {
        let envelope = r#"{"code":0,"msg":"success"}"#;
        match decode_data(envelope) {
            Ok(data) => panic!("缺 data 应失败，实际 {data}"),
            Err(failure) => assert!(failure.message.contains("没有 data"), "{failure:?}"),
        }
    }

    // ---- 姓名 → open_id 解析 ----

    /// 按脚本依次返回结果的假传输，并记录每次收到的请求。
    struct ScriptedTransport {
        responses: Mutex<VecDeque<Result<OutboundResponse, BaseError>>>,
        seen: Mutex<Vec<OutboundRequest>>,
    }

    impl ScriptedTransport {
        fn new(responses: Vec<Result<OutboundResponse, BaseError>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.seen.lock().map(|seen| seen.len()).unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl OutboundTransport for ScriptedTransport {
        async fn send(&self, request: OutboundRequest) -> Result<OutboundResponse, BaseError> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(request);
            }
            let mut responses = self
                .responses
                .lock()
                .map_err(|_| BaseError::ParamInvalid("test".to_string(), "锁中毒".to_string()))?;
            responses.pop_front().unwrap_or_else(|| {
                Err(BaseError::ParamInvalid(
                    "test".to_string(),
                    "脚本已耗尽".to_string(),
                ))
            })
        }
    }

    /// 只记录不真等。
    struct NoopSleeper;

    #[async_trait::async_trait]
    impl Sleeper for NoopSleeper {
        async fn sleep(&self, _duration: Duration) {}
    }

    /// 恒命中 token 的缓存：token 换取不落网络，出站只走被测的搜索请求。
    struct StaticTokenCache;

    #[async_trait::async_trait]
    impl TenantTokenCache for StaticTokenCache {
        async fn get(&self) -> anyhow::Result<Option<String>> {
            Ok(Some("t-test".to_string()))
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

    fn token_provider() -> TenantTokenProvider {
        TenantTokenProvider::new(
            Arc::new(StaticTokenCache),
            Arc::new(ScriptedTransport::new(Vec::new())),
            Arc::new(NoopSleeper),
            FeishuCredentials {
                app_id: "cli_test".to_string(),
                app_secret: "secret".to_string(),
            },
            "test-dep",
        )
        .unwrap_or_else(|error| panic!("provider 应可构建: {error}"))
    }

    /// 搜索成功信封（`items` 列表形态）。
    fn envelope(items: &str) -> OutboundResponse {
        OutboundResponse {
            status: 200,
            body: format!(r#"{{"code":0,"msg":"ok","data":{{"items":[{items}]}}}}"#),
            bytes: None,
            headers: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn resolve_applicant_matches_a_unique_exact_name() {
        let transport = ScriptedTransport::new(vec![Ok(envelope(
            r#"{"open_id":"ou_1","name":"杨佳俊"}"#,
        ))]);
        let provider = token_provider();

        let result = resolve_applicant(&transport, &NoopSleeper, &provider, " 杨佳俊 ")
            .await
            .unwrap_or_else(|error| panic!("应解析成功: {error}"));

        assert_eq!(
            result,
            ApplicantResolution::Resolved {
                open_id: "ou_1".to_string()
            }
        );
        assert_eq!(transport.call_count(), 1, "应恰发一次搜索请求");
    }

    #[tokio::test]
    async fn resolve_applicant_returns_not_found_when_nothing_matches() {
        let transport = ScriptedTransport::new(vec![Ok(envelope(""))]);
        let provider = token_provider();

        let result = resolve_applicant(&transport, &NoopSleeper, &provider, "查无此人")
            .await
            .unwrap_or_else(|error| panic!("应解析成功: {error}"));

        assert_eq!(result, ApplicantResolution::NotFound);
    }

    #[tokio::test]
    async fn resolve_applicant_reports_ambiguous_on_duplicate_names() {
        let transport = ScriptedTransport::new(vec![Ok(envelope(
            r#"{"open_id":"ou_a","name":"张三"},{"open_id":"ou_b","name":"张三"}"#,
        ))]);
        let provider = token_provider();

        let result = resolve_applicant(&transport, &NoopSleeper, &provider, "张三")
            .await
            .unwrap_or_else(|error| panic!("应解析成功: {error}"));

        assert_eq!(
            result,
            ApplicantResolution::Ambiguous {
                candidates: vec!["张三(ou_a)".to_string(), "张三(ou_b)".to_string()]
            }
        );
    }

    #[tokio::test]
    async fn resolve_applicant_accepts_a_single_fuzzy_hit() {
        // 无精确等值但整页恰一个候选：认它（如姓名带后缀/空格差异）。
        let transport = ScriptedTransport::new(vec![Ok(envelope(
            r#"{"open_id":"ou_1","name":"杨佳俊（成都）"}"#,
        ))]);
        let provider = token_provider();

        let result = resolve_applicant(&transport, &NoopSleeper, &provider, "杨佳俊")
            .await
            .unwrap_or_else(|error| panic!("应解析成功: {error}"));

        assert_eq!(
            result,
            ApplicantResolution::Resolved {
                open_id: "ou_1".to_string()
            }
        );
    }

    #[tokio::test]
    async fn resolve_applicant_maps_missing_scope_to_fatal() {
        // scope 未开通：99991672 是配置问题，不重试，落 Fatal。
        let transport = ScriptedTransport::new(vec![Ok(OutboundResponse {
            status: 200,
            body: r#"{"code":99991672,"msg":"no scope"}"#.to_string(),
            bytes: None,
            headers: BTreeMap::new(),
        })]);
        let provider = token_provider();

        let failure = resolve_applicant(&transport, &NoopSleeper, &provider, "杨佳俊")
            .await
            .err()
            .unwrap_or_else(|| panic!("缺 scope 必须失败"));

        assert_eq!(failure.kind, FailureKind::Fatal { code: 99991672 });
        assert_eq!(transport.call_count(), 1, "Fatal 不重试");
    }

    #[tokio::test]
    async fn resolve_applicant_skips_the_request_for_a_blank_name() {
        // 空脚本：若真发请求，脚本会耗尽并报错——用来钉住「空姓名不打接口」。
        let transport = ScriptedTransport::new(Vec::new());
        let provider = token_provider();

        let result = resolve_applicant(&transport, &NoopSleeper, &provider, "   ")
            .await
            .unwrap_or_else(|error| panic!("空姓名应直接返回 NotFound: {error}"));

        assert_eq!(result, ApplicantResolution::NotFound);
        assert_eq!(transport.call_count(), 0, "空姓名不得发出站请求");
    }
}

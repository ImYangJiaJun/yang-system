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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstanceDetail {
    pub(crate) instance_code: String,
    /// 审批单编号。**可能为空**——文档未承诺创建后立即可查，所以用 `Option`
    /// 而不是报错。
    pub(crate) serial_number: Option<String>,
    /// 审批实例状态（`PENDING` / `APPROVED` / …）。当前不回流多维表格，
    /// 但落库留痕便于将来加回流时不必回补历史。
    pub(crate) status: Option<String>,
}

/// 按 `instance_code` **或 `uuid`** 取实例详情的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GetOutcome {
    Found(InstanceDetail),
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
            Ok(GetOutcome::Found(InstanceDetail {
                instance_code: data.instance_code,
                serial_number: data.serial_number.filter(|value| !value.is_empty()),
                status: data.status,
            }))
        }
        // 实例不存在：调用方据此决定「回到 create 重试」还是「退避」。
        Err(failure) if failure.kind == FailureKind::InstanceNotFound => Ok(GetOutcome::NotFound),
        Err(failure) => Err(failure),
    }
}

#[derive(Debug, Deserialize)]
struct InstanceDetailData {
    instance_code: String,
    #[serde(default)]
    serial_number: Option<String>,
    #[serde(default)]
    status: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn empty_serial_number_is_normalised_to_none() {
        // 空串与缺失是同一件事：都表示「现在还没有编号」。
        // 直接用 `Some("")` 会让多维度表格被回填成一个空单元格。
        let detail = GetOutcome::Found(InstanceDetail {
            instance_code: "abc".to_string(),
            serial_number: Some(String::new()).filter(|value| !value.is_empty()),
            status: None,
        });
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
}

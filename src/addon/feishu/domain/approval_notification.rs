//! 明确指定接收人后的本轮结果通知；重试只重发消息，绝不重跑审批。

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use yang_base::BaseError;

use super::outbound::{
    send_with_retry, FailureKind, OutboundFailure, OutboundMethod, OutboundRequest,
    OutboundTransport, RetryPolicy, Sleeper,
};
use super::tenant_token::TenantTokenProvider;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReceiveIdType {
    OpenId,
    UserId,
    UnionId,
    Email,
}

impl ReceiveIdType {
    fn as_str(&self) -> &'static str {
        match self {
            Self::OpenId => "open_id",
            Self::UserId => "user_id",
            Self::UnionId => "union_id",
            Self::Email => "email",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct NotificationRecipient {
    pub(crate) receive_id_type: ReceiveIdType,
    pub(crate) receive_id: String,
}

impl NotificationRecipient {
    pub(crate) fn validate(&self) -> Result<(), BaseError> {
        let id = self.receive_id.trim();
        let valid_type = match self.receive_id_type {
            ReceiveIdType::OpenId => id.is_ascii() && id.starts_with("ou_") && id.len() > 3,
            ReceiveIdType::UnionId => id.is_ascii() && id.starts_with("on_") && id.len() > 3,
            ReceiveIdType::UserId => id.is_ascii(),
            ReceiveIdType::Email => id.split_once('@').is_some_and(|(local, domain)| {
                !local.is_empty() && domain.contains('.') && !domain.contains('@')
            }),
        };
        if id.is_empty()
            || id.len() > 320
            || id.chars().any(|c| c.is_whitespace() || c.is_control())
            || !valid_type
        {
            return Err(BaseError::ParamInvalid(
                "notification_recipient".into(),
                "请提供与 receive_id_type 对应的有效 ID 或邮箱，不能传姓名".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NotificationStatus {
    NotRequested,
    Skipped,
    Sent,
    Failed,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct NotificationResult {
    pub(crate) status: NotificationStatus,
    pub(crate) message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) message_id: Option<String>,
    /// 用于本次调用内的去重，飞书只保证一小时；不是审批 UUID。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) uuid: Option<String>,
}

impl NotificationResult {
    pub(crate) fn not_requested() -> Self {
        Self {
            status: NotificationStatus::NotRequested,
            message: "未指定接收人，完整反馈已通过 HTTP 返回。".into(),
            message_id: None,
            uuid: None,
        }
    }
    /// 本层**刻意**不发，但**不是**「无人可发」：中间态（waiting / retryable /
    /// created_pending_backfill）与全表受理的结论由 worker 轮询通知。
    ///
    /// 与 [`Self::not_requested`] 分开的原因：请求里传了发起人、发起人也解析成功时，
    /// 仍可能走到这里（`fallback_recipient_of` 的状态门只放终态），那时回「未指定
    /// 接收人」是与事实相反的结论——2026-10-16 真机：`applicant=杨佳俊` 解析成功、
    /// 结论是 waiting，响应里却写着「未指定接收人」。
    pub(crate) fn deferred_to_worker() -> Self {
        Self {
            status: NotificationStatus::NotRequested,
            message:
                "本轮结论由后台轮询发送通知（中间态结论在变化时发；全表受理逐条发），本层未发。"
                    .into(),
            message_id: None,
            uuid: None,
        }
    }
    pub(crate) fn skipped() -> Self {
        Self {
            status: NotificationStatus::Skipped,
            message: "请求参数或启用配置未通过校验，未发送通知。".into(),
            message_id: None,
            uuid: None,
        }
    }
    pub(crate) fn unavailable() -> Self {
        Self {
            status: NotificationStatus::Failed,
            message: "消息未发送：飞书出站配置不可用；审批结果不受影响。".into(),
            message_id: None,
            uuid: None,
        }
    }
}

const MESSAGE_RETRY: RetryPolicy = RetryPolicy {
    max_attempts: 3,
    base_backoff_ms: 250,
    max_backoff_ms: 1000,
    max_retry_after_ms: 1000,
};

/// 链接地址由坐标校验或飞书记录接口提供，不能从业务正文解析。
pub(crate) struct FeedbackReference {
    pub(crate) label: &'static str,
    pub(crate) text: String,
    pub(crate) href: Option<String>,
}

pub(crate) async fn send_feedback(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    recipient: &NotificationRecipient,
    text: &str,
    references: &[FeedbackReference],
) -> NotificationResult {
    let uuid = uuid::Uuid::new_v4().to_string();
    // shortcut: 只在本轮内有限重试；需要进程重启后补发时再引入持久通知队列。
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        send_post(
            transport, sleeper, tokens, recipient, text, references, &uuid,
        ),
    )
    .await;
    let reason = match result {
        Ok(Ok(message_id)) => {
            return NotificationResult {
                status: NotificationStatus::Sent,
                message: "飞书已接受消息。".into(),
                message_id: Some(message_id),
                uuid: Some(uuid),
            }
        }
        Ok(Err(failure)) => match failure.kind {
            FailureKind::Fatal { code: 0 } => {
                "消息调用未获得有效确认，请管理员检查消息接口与应用凭证".into()
            }
            FailureKind::Fatal { code } => format!(
                "飞书未确认发送，请检查机器人发布、发送权限与接收人可用范围（错误码：{code}）"
            ),
            _ => "飞书暂未确认发送，请稍后检查消息".into(),
        },
        Err(_) => "通知等待超时，送达情况未确认，请先检查消息".into(),
    };
    NotificationResult {
        status: NotificationStatus::Failed,
        message: format!("{reason}；审批结果不受影响。"),
        message_id: None,
        uuid: Some(uuid),
    }
}

// 业务正文只用 text 节点；末尾仅为明确传入的可信地址生成 a 节点。
fn post_content(text: &str, references: &[FeedbackReference]) -> serde_json::Value {
    use super::approval_feedback::FeedbackStatus;
    use serde_json::json;

    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    let (title, body) = match first.strip_prefix('【').and_then(|s| s.strip_suffix('】')) {
        Some(title) => (title, rest),
        None => ("审批派发结果", text),
    };
    let mut record = None;
    let mut table = None;
    let mut clicked_at = None;
    let mut current_value = None;
    let mut specific_step = None;
    let mut general_step = None;
    let mut reasons = Vec::new();
    let mut context = Vec::new();
    for line in body.lines() {
        let line = line.strip_prefix("说明：").unwrap_or(line);
        let line = line
            .strip_prefix("本轮未处理：")
            .or_else(|| line.strip_prefix("暂时失败，可重试："))
            .unwrap_or(line);
        if line.trim().is_empty() {
            continue;
        }
        let (label, value) = line.split_once('：').unwrap_or(("", line));
        match label {
            "记录" => record = Some(value),
            "数据表" => table = Some(value),
            "点击时间" => clicked_at = Some(value),
            "配置" | "多维表" | "审批" | "技术定位" | "控件定位" => {}
            "当前值" => current_value = Some(value),
            "处理" => specific_step = Some(value),
            "下一步" => general_step = Some(value),
            "审批控件" | "来源字段" | "选项来源" => context.push(line),
            "原因" => reasons.push(value),
            _ => reasons.push(line),
        }
    }
    let mut content = Vec::new();
    let mut push = |label: &str, value: String| {
        content.push(vec![
            json!({"tag":"text","text":format!("{label}："),"style":["bold"]}),
            json!({"tag":"text","text":value}),
        ]);
    };
    match (record, table) {
        (Some(record), Some(table)) => push("记录", format!("{record}（{table}）")),
        (Some(record), None) => push("记录", record.into()),
        (None, Some(table)) => push("数据表", table.into()),
        (None, None) => {}
    }
    if let Some(value) = clicked_at {
        push("点击时间", value.into());
    }
    if !reasons.is_empty() {
        let label = if title == FeedbackStatus::Succeeded.heading()
            || title == FeedbackStatus::Accepted.heading()
            || title == FeedbackStatus::Queued.heading()
        {
            "结果"
        } else {
            "原因"
        };
        push(label, reasons.join("；"));
    }
    if !context.is_empty() {
        push("涉及", context.join("；"));
    }
    if let Some(value) = current_value.filter(|value| Some(*value) != record) {
        push("当前值", value.into());
    }
    if let Some(step) = specific_step.or(general_step) {
        push("下一步", step.into());
    }
    for reference in references {
        let node = match &reference.href {
            Some(href) => json!({"tag":"a","text":reference.text,"href":href}),
            None => json!({"tag":"text","text":reference.text}),
        };
        content.push(vec![
            json!({"tag":"text","text":format!("{}：", reference.label),"style":["bold"]}),
            node,
        ]);
    }
    json!({"zh_cn":{"title":title,"content":content}})
}

async fn send_post(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    recipient: &NotificationRecipient,
    text: &str,
    references: &[FeedbackReference],
    uuid: &str,
) -> Result<String, OutboundFailure> {
    recipient.validate().map_err(|_| invalid_response())?;
    let content =
        serde_json::to_string(&post_content(text, references)).map_err(|_| invalid_response())?;
    let mut request = OutboundRequest {
        method: OutboundMethod::Post,
        url: "https://open.feishu.cn/open-apis/im/v1/messages".into(),
        query: vec![(
            "receive_id_type".into(),
            recipient.receive_id_type.as_str().into(),
        )],
        bearer_token: Some(
            tokens
                .tenant_access_token()
                .await
                .map_err(|_| invalid_response())?,
        ),
        json_body: Some(
            serde_json::json!({"receive_id":recipient.receive_id.trim(),"msg_type":"post","content":content,"uuid":uuid}),
        ),
        raw_body: None,
        binary_limit: None,
        timeout_secs: Some(2),
        idempotent: true,
    };
    let mut refreshed = false;
    loop {
        match send_with_retry(transport, sleeper, &request, MESSAGE_RETRY).await {
            Ok(response) => {
                let value: serde_json::Value =
                    serde_json::from_str(&response.body).map_err(|_| invalid_response())?;
                if value["code"].as_i64() != Some(0) {
                    return Err(invalid_response());
                }
                return value["data"]["message_id"]
                    .as_str()
                    .filter(|id| !id.trim().is_empty())
                    .map(str::to_owned)
                    .ok_or_else(invalid_response);
            }
            Err(failure) if failure.kind == FailureKind::TokenExpired && !refreshed => {
                refreshed = true;
                request.bearer_token = Some(
                    tokens
                        .invalidate_and_refresh()
                        .await
                        .map_err(|_| invalid_response())?,
                );
            }
            Err(failure) => return Err(failure),
        }
    }
}

fn invalid_response() -> OutboundFailure {
    OutboundFailure {
        kind: FailureKind::Fatal { code: 0 },
        message: "消息调用未获得有效确认".into(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{Arc, Mutex};

    use super::super::outbound::OutboundResponse;
    use super::super::tenant_token::{FeishuCredentials, TenantTokenCache};
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn post_reference_footer_uses_native_links_after_next_step() {
        let text =
            "【审批实例已确认，编号已回填】\n记录：付款申请\n说明：编号已回填\n下一步：查看审批";
        let references = [
            FeedbackReference {
                label: "多维表",
                text: "财务协作 / 付款申请表".into(),
                href: Some("https://www.feishu.cn/base/app?table=tbl".into()),
            },
            FeedbackReference {
                label: "原记录",
                text: "**公司** [网址](https://evil.example) <at>😀".into(),
                href: None,
            },
            FeedbackReference {
                label: "审批",
                text: "付款审批 / SN-001".into(),
                href: Some(
                    "https://applink.feishu.cn/client/mini_program/open?appId=test&path=example"
                        .into(),
                ),
            },
        ];
        let post = post_content(text, &references);
        let rows = post["zh_cn"]["content"]
            .as_array()
            .unwrap_or_else(|| panic!("缺少段落"));
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[2][0]["text"], "下一步：");
        assert_eq!(
            rows[3],
            json!([
                {"tag":"text","text":"多维表：","style":["bold"]},
                {"tag":"a","text":"财务协作 / 付款申请表","href":"https://www.feishu.cn/base/app?table=tbl"}
            ])
        );
        assert_eq!(
            rows[4],
            json!([
                {"tag":"text","text":"原记录：","style":["bold"]},
                {"tag":"text","text":"**公司** [网址](https://evil.example) <at>😀"}
            ])
        );
        assert_eq!(
            rows[5],
            json!([
                {"tag":"text","text":"审批：","style":["bold"]},
                {"tag":"a","text":"付款审批 / SN-001","href":"https://applink.feishu.cn/client/mini_program/open?appId=test&path=example"}
            ])
        );
    }

    #[test]
    fn post_content_is_compact_and_keeps_named_error_context() {
        let text = "【本轮未创建审批，需要修正数据】\n配置：付款申请\n多维表：财务协作\n数据表：付款表\n审批：付款审批\n记录：公司\"A\"😀：子公司\n说明：本轮未处理：审批控件：明细 / 付款公司\n来源字段：实际付款公司\n选项来源：合作企业 / 企业名称\n当前值：公司\"A\"😀：子公司\n原因：没有唯一且完全匹配的启用选项\n\n技术定位：source_key=fldExample\n处理：清理同名重复项\n下一步：检查文案和启用状态\n控件定位：widgetExample；字段定位：fldCompany\n技术定位：Base=appExample；表=tblExample";
        assert_eq!(
            post_content(text, &[]),
            json!({"zh_cn":{
                "title":"本轮未创建审批，需要修正数据",
                "content":[
                    [{"tag":"text","text":"记录：","style":["bold"]},{"tag":"text","text":"公司\"A\"😀：子公司（付款表）"}],
                    [{"tag":"text","text":"原因：","style":["bold"]},{"tag":"text","text":"没有唯一且完全匹配的启用选项"}],
                    [{"tag":"text","text":"涉及：","style":["bold"]},{"tag":"text","text":"审批控件：明细 / 付款公司；来源字段：实际付款公司；选项来源：合作企业 / 企业名称"}],
                    [{"tag":"text","text":"下一步：","style":["bold"]},{"tag":"text","text":"清理同名重复项"}]
                ]
            }})
        );
    }

    #[test]
    fn post_content_puts_the_click_time_under_the_record() {
        let text = "【本轮未创建审批，需要修正数据】\n记录：第 5 行\n点击时间：2026-10-26 10:32:55\n说明：本轮未处理：缺少必填值";
        assert_eq!(
            post_content(text, &[]),
            json!({"zh_cn":{
                "title":"本轮未创建审批，需要修正数据",
                "content":[
                    [{"tag":"text","text":"记录：","style":["bold"]},{"tag":"text","text":"第 5 行"}],
                    [{"tag":"text","text":"点击时间：","style":["bold"]},{"tag":"text","text":"2026-10-26 10:32:55"}],
                    [{"tag":"text","text":"原因：","style":["bold"]},{"tag":"text","text":"缺少必填值"}]
                ]
            }})
        );
    }

    #[test]
    fn post_content_keeps_plain_headings_and_dynamic_markup_literal() {
        for text in [
            "正文",
            "首行\n第二行",
            "【缺少右括号\n正文",
            "缺少左括号】\n正文",
        ] {
            assert_eq!(
                post_content(text, &[]),
                json!({"zh_cn":{
                    "title":"审批派发结果","content":[[
                        {"tag":"text","text":"原因：","style":["bold"]},
                        {"tag":"text","text":text.replace('\n', "；")}
                    ]]
                }})
            );
        }
        let value = "**企业** [地址](https://example.com) <at user_id=\"all\"></at> &lt; 😀";
        let text = format!("【审批实例已确认】\n记录：{value}\n备注：第一行\n第二行");
        assert_eq!(
            post_content(&text, &[]),
            json!({"zh_cn":{
                "title":"审批实例已确认",
                "content":[
                    [{"tag":"text","text":"记录：","style":["bold"]},{"tag":"text","text":value}],
                    [{"tag":"text","text":"原因：","style":["bold"]},{"tag":"text","text":"备注：第一行；第二行"}]
                ]
            }})
        );
    }

    #[test]
    fn post_content_keeps_distinct_values_and_general_next_step() {
        let text = "【本轮未完成，审批是否创建尚未确认】\n数据表：付款表\n记录：付款申请 001\n说明：暂时失败，可重试：审批控件：付款公司\n来源字段：实际付款公司\n选项来源：名称暂不可用\n当前值：公司 B\n原因：外部选项读取失败\n下一步：先检查审批记录，再重试";
        assert_eq!(
            post_content(text, &[]),
            json!({"zh_cn":{
                "title":"本轮未完成，审批是否创建尚未确认",
                "content":[
                    [{"tag":"text","text":"记录：","style":["bold"]},{"tag":"text","text":"付款申请 001（付款表）"}],
                    [{"tag":"text","text":"原因：","style":["bold"]},{"tag":"text","text":"外部选项读取失败"}],
                    [{"tag":"text","text":"涉及：","style":["bold"]},{"tag":"text","text":"审批控件：付款公司；来源字段：实际付款公司；选项来源：名称暂不可用"}],
                    [{"tag":"text","text":"当前值：","style":["bold"]},{"tag":"text","text":"公司 B"}],
                    [{"tag":"text","text":"下一步：","style":["bold"]},{"tag":"text","text":"先检查审批记录，再重试"}]
                ]
            }})
        );
    }

    #[test]
    fn post_content_uses_each_business_conclusion_as_title() {
        use super::super::approval_feedback::{render, FeedbackStatus};
        for status in [
            FeedbackStatus::Succeeded,
            FeedbackStatus::Waiting,
            FeedbackStatus::CreatedPendingBackfill,
            FeedbackStatus::Retryable,
            FeedbackStatus::Failed,
            FeedbackStatus::Accepted,
        ] {
            let text = render(
                status,
                "安全说明",
                "配置：付款申请\n数据表：付款表",
                None,
                None,
                None,
                "base",
                "table",
            );
            let post = post_content(&text, &[]);
            assert_eq!(post["zh_cn"]["title"], status.heading());
            let rows = &post["zh_cn"]["content"];
            assert_eq!(rows.as_array().map(Vec::len), Some(3));
            assert_eq!(rows[0][0]["text"], "数据表：");
            assert_eq!(rows[0][1]["text"], "付款表");
            let reason_label =
                if matches!(status, FeedbackStatus::Succeeded | FeedbackStatus::Accepted) {
                    "结果："
                } else {
                    "原因："
                };
            assert_eq!(rows[1][0]["text"], reason_label);
            assert_eq!(rows[1][1]["text"], "安全说明");
            assert_eq!(rows[2][0]["text"], "下一步：");
        }
    }

    #[test]
    fn recipient_requires_explicit_type_and_valid_identifier() {
        for (kind, id, valid) in [
            ("open_id", "ou_test", true),
            ("open_id", "employee_1", false),
            ("open_id", "ou_中文", false),
            ("open_id", "张三", false),
            ("email", "a@b.cn", true),
            ("email", "a@b@c.cn", false),
            ("email", "@b.cn", false),
            ("email", "a@bcn", false),
            ("email", "a b@c.cn", false),
            ("user_id", "", false),
            ("user_id", "张三", false),
            ("user_id", "employee_1", true),
            ("open_id", "ou_", false),
            ("open_id", "ou_\u{0000}bad", false),
            ("union_id", "on_test", true),
            ("union_id", "ou_test", false),
            ("union_id", "on_", false),
            ("union_id", "on_中文", false),
        ] {
            let recipient: NotificationRecipient =
                serde_json::from_value(serde_json::json!({"receive_id_type":kind,"receive_id":id}))
                    .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(recipient.validate().is_ok(), valid, "{kind} {id}");
        }
        let oversized = NotificationRecipient {
            receive_id_type: ReceiveIdType::UserId,
            receive_id: "x".repeat(321),
        };
        assert!(oversized.validate().is_err());
        assert!(serde_json::from_value::<NotificationRecipient>(
            json!({"receive_id_type":"name","receive_id":"张三"})
        )
        .is_err());
        assert!(serde_json::from_value::<NotificationRecipient>(
            json!({"receive_id_type":"email","receive_id":"a@b.cn","name":"张三"})
        )
        .is_err());
    }

    struct Replay {
        responses: Mutex<VecDeque<OutboundResponse>>,
        seen: Mutex<Vec<OutboundRequest>>,
    }

    impl Replay {
        fn new(responses: Vec<(u16, Value)>) -> Self {
            Self {
                responses: Mutex::new(
                    responses
                        .into_iter()
                        .map(|(status, body)| OutboundResponse {
                            status,
                            body: body.to_string(),
                            bytes: None,
                            headers: BTreeMap::new(),
                        })
                        .collect(),
                ),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<OutboundRequest> {
            self.seen.lock().unwrap_or_else(|e| panic!("{e}")).clone()
        }
    }

    #[async_trait::async_trait]
    impl OutboundTransport for Replay {
        async fn send(&self, request: OutboundRequest) -> Result<OutboundResponse, BaseError> {
            assert!(
                request.url.ends_with("/im/v1/messages")
                    || request
                        .url
                        .ends_with("/auth/v3/tenant_access_token/internal"),
                "通知不得调用审批或搜索接口"
            );
            self.seen
                .lock()
                .unwrap_or_else(|e| panic!("{e}"))
                .push(request);
            Ok(self
                .responses
                .lock()
                .unwrap_or_else(|e| panic!("{e}"))
                .pop_front()
                .unwrap_or_else(|| panic!("脚本耗尽")))
        }
    }

    struct NoSleep;
    #[async_trait::async_trait]
    impl Sleeper for NoSleep {
        async fn sleep(&self, _: Duration) {}
    }

    // 缓存故意一直返回旧值：刷新结果必须直接用于下一次发送。
    struct StaleCache;
    #[async_trait::async_trait]
    impl TenantTokenCache for StaleCache {
        async fn get(&self) -> anyhow::Result<Option<String>> {
            Ok(Some("old-token".into()))
        }
        async fn put(&self, _: &str, _: i64) -> anyhow::Result<()> {
            Ok(())
        }
        async fn invalidate(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn acquire_lock(&self, _: &str, _: i64) -> anyhow::Result<bool> {
            Ok(true)
        }
        async fn release_lock(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn provider(transport: Arc<Replay>) -> TenantTokenProvider {
        TenantTokenProvider::new(
            Arc::new(StaleCache),
            transport,
            Arc::new(NoSleep),
            FeishuCredentials {
                app_id: "cli_test".into(),
                app_secret: "test-secret".into(),
            },
            "feedback-test",
        )
        .unwrap_or_else(|e| panic!("{e}"))
    }

    fn recipient() -> NotificationRecipient {
        NotificationRecipient {
            receive_id_type: ReceiveIdType::OpenId,
            receive_id: " ou_receiver ".into(),
        }
    }

    #[tokio::test]
    async fn message_rate_limit_retries_same_uuid_and_sends_serialized_post() {
        let replay = Arc::new(Replay::new(vec![
            (400, json!({"code":230020,"msg":"rate limited"})),
            (200, json!({"code":0,"data":{"message_id":"om_feedback"}})),
        ]));
        let text = "【本轮未创建审批，需要补齐数据】\n当前值：成都市易威行科技有限公司";
        let references = [FeedbackReference {
            label: "多维表",
            text: "财务协作 / 付款申请表".into(),
            href: Some("https://www.feishu.cn/base/app?table=tbl".into()),
        }];
        let result = send_feedback(
            replay.as_ref(),
            &NoSleep,
            &provider(replay.clone()),
            &recipient(),
            text,
            &references,
        )
        .await;
        assert!(matches!(result.status, NotificationStatus::Sent));
        assert_eq!(result.message_id.as_deref(), Some("om_feedback"));
        let requests = replay.requests();
        assert_eq!(requests.len(), 2);
        for request in &requests {
            assert_eq!(
                request.query,
                vec![("receive_id_type".into(), "open_id".into())]
            );
            assert_eq!(request.bearer_token.as_deref(), Some("old-token"));
            assert_eq!(request.timeout_secs, Some(2));
            assert!(request.idempotent);
            let body = request
                .json_body
                .as_ref()
                .unwrap_or_else(|| panic!("缺少正文"));
            assert_eq!(body["receive_id"], "ou_receiver");
            assert_eq!(body["msg_type"], "post");
            assert_eq!(body["uuid"].as_str(), result.uuid.as_deref());
            assert!(result.uuid.as_ref().is_some_and(|id| id.len() <= 50));
            let content: Value = serde_json::from_str(
                body["content"]
                    .as_str()
                    .unwrap_or_else(|| panic!("content 必须是 JSON 字符串")),
            )
            .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(content, post_content(text, &references));
            assert_eq!(content["zh_cn"]["content"][1][1]["tag"], "a");
            assert_eq!(content["zh_cn"]["title"], "本轮未创建审批，需要补齐数据");
            assert_eq!(content["zh_cn"]["content"][0][0]["style"], json!(["bold"]));
            assert_eq!(
                content["zh_cn"]["content"][0][1]["text"],
                "成都市易威行科技有限公司"
            );
        }
    }

    #[tokio::test]
    async fn expired_token_refreshes_once_and_preserves_message_uuid() {
        let replay = Arc::new(Replay::new(vec![
            (200, json!({"code":99991663})),
            (
                200,
                json!({"code":0,"tenant_access_token":"fresh-token","expire":7200}),
            ),
            (200, json!({"code":0,"data":{"message_id":"om_fresh"}})),
        ]));
        let result = send_feedback(
            replay.as_ref(),
            &NoSleep,
            &provider(replay.clone()),
            &recipient(),
            "正文",
            &[],
        )
        .await;
        assert!(matches!(result.status, NotificationStatus::Sent));
        let requests = replay.requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].bearer_token.as_deref(), Some("old-token"));
        assert_eq!(requests[2].bearer_token.as_deref(), Some("fresh-token"));
        assert_eq!(requests[0].json_body, requests[2].json_body);

        let replay = Arc::new(Replay::new(vec![
            (200, json!({"code":99991663})),
            (
                200,
                json!({"code":0,"tenant_access_token":"fresh-token","expire":7200}),
            ),
            (200, json!({"code":99991663})),
        ]));
        let result = send_feedback(
            replay.as_ref(),
            &NoSleep,
            &provider(replay.clone()),
            &recipient(),
            "正文",
            &[],
        )
        .await;
        assert!(matches!(result.status, NotificationStatus::Failed));
        assert_eq!(replay.requests().len(), 3, "只允许一次刷新");
    }

    #[tokio::test]
    async fn message_requires_confirmation_and_bounds_retries_without_leaking_raw_body() {
        for (status, body, calls) in [
            (200, json!({"code":0,"data":{}}), 1),
            (200, json!({"code":0,"data":{"message_id":" "}}), 1),
            (200, json!({"data":{"message_id":"om_unconfirmed"}}), 1),
            (400, json!({"code":230013,"msg":"private-raw-error"}), 1),
            (503, json!({"code":230020,"msg":"private-raw-error"}), 3),
        ] {
            let replay = Arc::new(Replay::new(vec![(status, body); calls]));
            let result = send_feedback(
                replay.as_ref(),
                &NoSleep,
                &provider(replay.clone()),
                &recipient(),
                "正文",
                &[],
            )
            .await;
            assert!(matches!(result.status, NotificationStatus::Failed));
            assert!(result.message_id.is_none());
            assert_eq!(replay.requests().len(), calls);
            assert!(!result.message.contains("private-raw-error"));
            assert!(!result.message.contains("错误码：0"));
        }
        let replay = Arc::new(Replay::new(vec![]));
        let invalid = NotificationRecipient {
            receive_id_type: ReceiveIdType::OpenId,
            receive_id: "张三".into(),
        };
        let result = send_feedback(
            replay.as_ref(),
            &NoSleep,
            &provider(replay.clone()),
            &invalid,
            "正文",
            &[],
        )
        .await;
        assert!(matches!(result.status, NotificationStatus::Failed));
        assert!(replay.requests().is_empty());
    }

    #[tokio::test]
    async fn all_recipient_types_reach_im_without_search_and_malformed_confirmation_fails() {
        for (kind, id) in [
            ("open_id", "ou_receiver"),
            ("union_id", "on_receiver"),
            ("user_id", "employee_1"),
            ("email", "initiator@example.com"),
        ] {
            let recipient: NotificationRecipient =
                serde_json::from_value(json!({"receive_id_type":kind,"receive_id":id}))
                    .unwrap_or_else(|e| panic!("{e}"));
            let replay = Arc::new(Replay::new(vec![(
                200,
                json!({"code":0,"data":{"message_id":"om_confirmed"}}),
            )]));
            let result = send_feedback(
                replay.as_ref(),
                &NoSleep,
                &provider(replay.clone()),
                &recipient,
                "正文",
                &[],
            )
            .await;
            assert!(matches!(result.status, NotificationStatus::Sent));
            let requests = replay.requests();
            assert_eq!(requests.len(), 1, "通知不需要姓名解析");
            assert_eq!(
                requests[0].query,
                vec![("receive_id_type".into(), kind.into())]
            );
            assert_eq!(
                requests[0].json_body.as_ref().map(|v| &v["receive_id"]),
                Some(&json!(id))
            );
        }
        let replay = Arc::new(Replay::new(vec![(200, json!(null))]));
        replay.responses.lock().unwrap_or_else(|e| panic!("{e}"))[0].body =
            "private-invalid-json".into();
        let result = send_feedback(
            replay.as_ref(),
            &NoSleep,
            &provider(replay.clone()),
            &recipient(),
            "正文",
            &[],
        )
        .await;
        assert!(matches!(result.status, NotificationStatus::Failed));
        assert!(result.message_id.is_none());
        assert!(!result.message.contains("private-invalid-json"));
    }

    #[tokio::test]
    async fn notification_timeout_is_bounded_and_does_not_claim_delivery() {
        struct PendingTransport;
        #[async_trait::async_trait]
        impl OutboundTransport for PendingTransport {
            async fn send(&self, _: OutboundRequest) -> Result<OutboundResponse, BaseError> {
                std::future::pending().await
            }
        }
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(12),
            send_feedback(
                &PendingTransport,
                &NoSleep,
                &provider(Arc::new(Replay::new(vec![]))),
                &recipient(),
                "正文",
                &[],
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("通知总预算必须在十秒内结束等待"));
        assert!(started.elapsed() < Duration::from_secs(12));
        assert!(matches!(result.status, NotificationStatus::Failed));
        assert!(result.message.contains("超时"));
        assert!(result.message_id.is_none());
    }

    #[tokio::test]
    #[ignore = "向真实用户发送消息：需显式提供配置、应用 ID、open_id 和测试 Redis"]
    async fn real_app_sends_rich_feedback_and_confirms_sender_and_content() {
        use super::super::approval::instance_applink;
        use super::super::outbound::{HttpClientTransport, TokioSleeper};
        use super::super::tenant_token::RedisTenantTokenCache;
        use yang_base::http::HttpClient;
        use yang_db::{RedisClient, RedisConfig};

        struct ObservedTransport {
            inner: HttpClientTransport,
            receipts: Mutex<Vec<Value>>,
        }
        #[async_trait::async_trait]
        impl OutboundTransport for ObservedTransport {
            async fn send(&self, request: OutboundRequest) -> Result<OutboundResponse, BaseError> {
                let is_message = request.url.ends_with("/im/v1/messages");
                let response = self.inner.send(request).await?;
                if is_message {
                    let body: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
                    self.receipts
                        .lock()
                        .unwrap_or_else(|_| panic!("回执锁不可用"))
                        .push(json!({
                            "http_status": response.status,
                            "code": body["code"],
                            "message_id": body["data"]["message_id"],
                            "chat_id": body["data"]["chat_id"],
                            "create_time": body["data"]["create_time"],
                            "sender": body["data"]["sender"],
                            "msg_type": body["data"]["msg_type"],
                            "content": body["data"]["body"]["content"]
                        }));
                }
                Ok(response)
            }
        }

        let config_path = std::env::var("FEEDBACK_SMOKE_CONFIG")
            .unwrap_or_else(|_| panic!("必须显式设置 FEEDBACK_SMOKE_CONFIG"));
        let expected_app = std::env::var("FEEDBACK_SMOKE_APP_ID")
            .unwrap_or_else(|_| panic!("必须显式设置 FEEDBACK_SMOKE_APP_ID"));
        let recipient = NotificationRecipient {
            receive_id_type: ReceiveIdType::OpenId,
            receive_id: std::env::var("FEEDBACK_SMOKE_OPEN_ID")
                .unwrap_or_else(|_| panic!("必须显式设置 FEEDBACK_SMOKE_OPEN_ID")),
        };
        assert!(recipient.validate().is_ok());
        let redis_url = std::env::var("YANG_SYSTEM_TEST_REDIS_URL")
            .unwrap_or_else(|_| panic!("必须使用测试 Redis DB 15"));
        assert!(redis_url.ends_with("/15"), "必须使用测试 Redis DB 15");
        let receipt_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/feedback-live-post-links-receipt.json");
        assert!(
            !receipt_path.exists(),
            "已有发送回执，请先检查，避免重复外发"
        );
        let settings = crate::config::Settings::load(std::path::Path::new(&config_path))
            .unwrap_or_else(|_| panic!("真实配置加载失败，未发送消息"));
        let feishu = settings
            .feishu
            .unwrap_or_else(|| panic!("未配置飞书，未发送消息"));
        assert_eq!(feishu.app_id.as_deref(), Some(expected_app.as_str()));
        let credentials = FeishuCredentials {
            app_id: expected_app.clone(),
            app_secret: feishu
                .app_secret
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| panic!("未配置应用密钥，未发送消息")),
        };
        let client = HttpClient::new(30).unwrap_or_else(|_| panic!("HTTP 初始化失败"));
        let transport = Arc::new(ObservedTransport {
            inner: HttpClientTransport::new(client),
            receipts: Mutex::new(Vec::new()),
        });
        let sleeper = Arc::new(TokioSleeper);
        let redis = RedisClient::connect_with_config(&redis_url, RedisConfig::default())
            .await
            .unwrap_or_else(|_| panic!("测试 Redis 连接失败"));
        let deployment = format!("feedback-live-{}", uuid::Uuid::new_v4().simple());
        let cache = Arc::new(RedisTenantTokenCache::new(redis, &deployment));
        let tokens = TenantTokenProvider::new(
            cache.clone(),
            transport.clone(),
            sleeper.clone(),
            credentials,
            &deployment,
        )
        .unwrap_or_else(|_| panic!("应用凭证提供器初始化失败"));
        let text = "【验证消息｜表格与审批跳转】\n原因：核对已有表格与审批的跳转入口。\n下一步：请在电脑和手机分别点击下方两个链接，确认表格和审批编号正确。";
        let approval_url = instance_applink("20971264-278F-4501-8743-5B7E2DA25C46")
            .unwrap_or_else(|_| panic!("审批链接生成失败，未发送消息"));
        assert_eq!(approval_url, "https://applink.feishu.cn/client/mini_program/open?appId=cli_9cb844403dbb9108&mode=appCenter&path_pc=pc%2Fpages%2Fin-process%2Findex%3FinstanceId%3D20971264-278F-4501-8743-5B7E2DA25C46%26source%3Dbitable&path=pages%2Fdetail%2Findex%3FinstanceId%3D20971264-278F-4501-8743-5B7E2DA25C46%26source%3Dbitable");
        let references = [
            FeedbackReference {
                label: "多维表",
                text: "打开本次审批来源表".into(),
                href: Some("https://bvijptdn9rx.feishu.cn/base/Lxf4bG4R3agS3QsMWGycPzXDnCg?table=tblWZfpeWI9rglXs&view=vewmDPvC9M".into()),
            },
            FeedbackReference {
                label: "审批",
                text: "202610130151".into(),
                href: Some(approval_url),
            },
        ];
        let result = send_feedback(
            transport.as_ref(),
            sleeper.as_ref(),
            &tokens,
            &recipient,
            text,
            &references,
        )
        .await;
        let receipts = transport
            .receipts
            .lock()
            .unwrap_or_else(|_| panic!("回执锁不可用"))
            .clone();
        std::fs::write(
            &receipt_path,
            serde_json::to_vec_pretty(&json!({
                "app_id": expected_app,
                "receive_id_type": "open_id",
                "recipient": recipient.receive_id,
                "text": text,
                "post": post_content(text, &references),
                "notification": result,
                "responses": receipts
            }))
            .unwrap_or_else(|_| panic!("回执序列化失败")),
        )
        .unwrap_or_else(|_| panic!("回执保存失败，请先检查消息，勿直接重试"));
        let _ = cache.invalidate().await;
        assert!(
            matches!(result.status, NotificationStatus::Sent),
            "{}",
            result.message
        );
        let receipt = receipts.last().unwrap_or_else(|| panic!("缺少飞书回执"));
        assert_eq!(receipt["http_status"], 200);
        assert_eq!(receipt["code"], 0);
        assert_eq!(receipt["sender"]["id"], expected_app);
        assert_eq!(receipt["sender"]["id_type"], "app_id");
        assert_eq!(receipt["sender"]["sender_type"], "app");
        assert_eq!(receipt["msg_type"], "post");
        assert_eq!(receipt["message_id"].as_str(), result.message_id.as_deref());
        assert_post_receipt(receipt, &post_content(text, &references));
        println!("应用消息已获确认；回执：{}", receipt_path.display());
    }

    fn assert_post_receipt(receipt: &Value, expected_post: &Value) {
        let content: Value = serde_json::from_str(
            receipt["content"]
                .as_str()
                .unwrap_or_else(|| panic!("缺少正文")),
        )
        .unwrap_or_else(|_| panic!("正文不是 JSON 字符串"));
        let expected = &expected_post["zh_cn"];
        assert_eq!(content["title"], expected["title"]);
        let actual_rows = content["content"]
            .as_array()
            .unwrap_or_else(|| panic!("缺少段落"));
        let expected_rows = expected["content"]
            .as_array()
            .unwrap_or_else(|| panic!("缺少预期段落"));
        assert_eq!(actual_rows.len(), expected_rows.len());
        for (actual, expected) in actual_rows.iter().zip(expected_rows) {
            let actual = actual.as_array().unwrap_or_else(|| panic!("段落不是数组"));
            let expected = expected
                .as_array()
                .unwrap_or_else(|| panic!("预期段落不是数组"));
            assert_eq!(actual.len(), expected.len());
            for (actual, expected) in actual.iter().zip(expected) {
                assert_eq!(actual["tag"], expected["tag"]);
                assert_eq!(actual["text"], expected["text"]);
                assert_eq!(actual["href"], expected["href"]);
                assert_eq!(
                    actual["style"].as_array().cloned().unwrap_or_default(),
                    expected["style"].as_array().cloned().unwrap_or_default(),
                );
            }
        }
    }

    #[test]
    #[ignore = "只读核对已保存的真实富文本回执；不外发消息"]
    fn saved_rich_feedback_receipt_preserves_title_content_styles_and_sender() {
        for name in [
            "target/feedback-live-post-receipt.json",
            "target/feedback-live-post-compact-receipt.json",
            "target/feedback-live-post-links-receipt.json",
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
            let saved: Value = serde_json::from_slice(
                &std::fs::read(&path).unwrap_or_else(|_| panic!("缺少已保存回执：{name}")),
            )
            .unwrap_or_else(|_| panic!("回执不是 JSON"));
            let receipt = saved["responses"]
                .as_array()
                .and_then(|rows| rows.last())
                .unwrap_or_else(|| panic!("缺少官方响应"));
            assert_eq!(receipt["http_status"], 200);
            assert_eq!(receipt["code"], 0);
            assert_eq!(saved["notification"]["status"], "sent");
            assert_eq!(receipt["sender"]["id"], saved["app_id"]);
            assert_eq!(receipt["sender"]["id_type"], "app_id");
            assert_eq!(receipt["sender"]["sender_type"], "app");
            assert_eq!(receipt["msg_type"], "post");
            assert_eq!(receipt["message_id"], saved["notification"]["message_id"]);
            // 历史回执与当时请求比较，样式迭代不能改写历史证据。
            assert_post_receipt(receipt, &saved["post"]);
        }
        println!("已保存的应用富文本回执核对通过；未发送新消息");
    }
}

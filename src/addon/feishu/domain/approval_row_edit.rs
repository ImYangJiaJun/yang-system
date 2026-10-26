//! 消息发出后再补行号：扫描在后台做，失败只降级，不影响已送达的消息。
//!
//! 发送路径先按「无行号」立即出站，行号由本模块按 `(base_token, table_id)` 分组
//! 批量扫描后，用飞书编辑消息接口回填到**同一条**消息里。

use std::collections::BTreeMap;
use std::time::Duration;

use super::approval_feedback::{self, FeedbackStatus};
use super::approval_notification::{self, FeedbackReference};
use super::bitable::{self, BitableCoordinates};
use super::outbound::{OutboundTransport, Sleeper};
use super::tenant_token::TenantTokenProvider;

/// 单组行号扫描的总预算。超时按「没有行号」处理，消息保持原样。
pub(crate) const ROW_EDIT_SCAN_BUDGET: Duration = Duration::from_secs(15);

/// 一条待补行号的消息。
#[derive(Debug, Clone)]
pub(crate) struct RowNumberEdit {
    pub(crate) message_id: String,
    pub(crate) status: FeedbackStatus,
    pub(crate) reason: String,
    pub(crate) names: String,
    pub(crate) record_id: String,
    pub(crate) clicked_at: Option<String>,
    pub(crate) base_token: String,
    pub(crate) table_id: String,
    pub(crate) references: Vec<FeedbackReference>,
}

impl RowNumberEdit {
    /// 发送成功后的补行号任务；未送达、没请求/被跳过、或飞书没回 `message_id`
    /// 时都没有可编辑的对象。HTTP 与 worker 两条发送路径共用这一条判据。
    #[allow(clippy::too_many_arguments)] // 9 个参数：与 render 同形，展示要素逐个平铺。
    pub(crate) fn after_send(
        notification: &approval_notification::NotificationResult,
        status: FeedbackStatus,
        reason: &str,
        names: &str,
        record_id: &str,
        clicked_at: Option<&str>,
        base_token: &str,
        table_id: &str,
        references: &[FeedbackReference],
    ) -> Option<Self> {
        if !matches!(
            notification.status,
            approval_notification::NotificationStatus::Sent
        ) {
            return None;
        }
        let message_id = notification
            .message_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())?;
        Some(Self {
            message_id: message_id.to_string(),
            status,
            reason: reason.to_string(),
            names: names.to_string(),
            record_id: record_id.to_string(),
            clicked_at: clicked_at.map(str::to_string),
            base_token: base_token.to_string(),
            table_id: table_id.to_string(),
            references: references.to_vec(),
        })
    }
}

/// 批量补行号。任何失败只记日志与指标，不向上抛。
pub(crate) async fn apply(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    edits: Vec<RowNumberEdit>,
) {
    // 同表的消息共用一次扫描：行号是整表顺序，逐条扫会把同一张表翻 N 遍。
    let mut groups: BTreeMap<(String, String), Vec<RowNumberEdit>> = BTreeMap::new();
    for edit in edits {
        groups
            .entry((edit.base_token.clone(), edit.table_id.clone()))
            .or_default()
            .push(edit);
    }

    for ((base_token, table_id), group) in groups {
        let record_ids: Vec<String> = group.iter().map(|edit| edit.record_id.clone()).collect();
        let coordinates = BitableCoordinates {
            app_token: base_token.clone(),
            table_id: table_id.clone(),
            view_id: None,
        };
        let scan = tokio::time::timeout(
            ROW_EDIT_SCAN_BUDGET,
            bitable::record_row_numbers(transport, sleeper, tokens, &coordinates, &record_ids),
        )
        .await;
        let rows = match scan {
            Ok(Ok(rows)) => rows,
            Ok(Err(failure)) => {
                tracing::warn!(
                    base_token,
                    table_id,
                    error = %failure,
                    "行号扫描失败，消息保持原样"
                );
                for _ in &group {
                    metrics::counter!("feishu_approval_row_edit_total", "result" => "failed")
                        .increment(1);
                }
                continue;
            }
            Err(_) => {
                tracing::warn!(base_token, table_id, "行号扫描超时，消息保持原样");
                for _ in &group {
                    metrics::counter!("feishu_approval_row_edit_total", "result" => "timeout")
                        .increment(1);
                }
                continue;
            }
        };

        for mut edit in group {
            let Some(row) = rows.get(&edit.record_id).copied() else {
                metrics::counter!("feishu_approval_row_edit_total", "result" => "no_row")
                    .increment(1);
                tracing::warn!(
                    message_id = %edit.message_id,
                    record_id = %edit.record_id,
                    "未找到记录行号，消息保持原样"
                );
                continue;
            };
            // 原记录链接的文案从「打开原记录」换成行号；链接本身不动。
            for reference in &mut edit.references {
                if reference.label == "原记录" {
                    reference.text = format!("第 {row} 行");
                }
            }
            let text = approval_feedback::render(
                edit.status,
                &edit.reason,
                &edit.names,
                Some(&edit.record_id),
                Some(row),
                edit.clicked_at.as_deref(),
                &edit.base_token,
                &edit.table_id,
            );
            match approval_notification::edit_feedback(
                transport,
                sleeper,
                tokens,
                &edit.message_id,
                &text,
                &edit.references,
            )
            .await
            {
                Ok(()) => {
                    metrics::counter!("feishu_approval_row_edit_total", "result" => "edited")
                        .increment(1);
                    tracing::info!(message_id = %edit.message_id, row, "已把行号补进消息");
                }
                Err(failure) => {
                    metrics::counter!("feishu_approval_row_edit_total", "result" => "failed")
                        .increment(1);
                    tracing::warn!(
                        message_id = %edit.message_id,
                        error = %failure,
                        "补行号编辑消息失败"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use serde_json::{json, Value};

    use super::super::outbound::{OutboundMethod, OutboundRequest, OutboundResponse};
    use super::super::tenant_token::{FeishuCredentials, TenantTokenCache};
    use yang_base::BaseError;

    struct RowEditTransport {
        items: Vec<String>,
        requests: Mutex<Vec<OutboundRequest>>,
    }

    impl RowEditTransport {
        fn new(items: &[&str]) -> Self {
            Self {
                items: items.iter().map(|id| (*id).to_string()).collect(),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<OutboundRequest> {
            self.requests
                .lock()
                .unwrap_or_else(|e| panic!("{e}"))
                .clone()
        }
    }

    #[async_trait::async_trait]
    impl OutboundTransport for RowEditTransport {
        async fn send(&self, request: OutboundRequest) -> Result<OutboundResponse, BaseError> {
            let body = match request.method {
                OutboundMethod::Get => json!({
                    "code": 0,
                    "data": {
                        "has_more": false,
                        "items": self
                            .items
                            .iter()
                            .map(|id| json!({"record_id": id, "fields": {}}))
                            .collect::<Vec<_>>(),
                    }
                })
                .to_string(),
                OutboundMethod::Put => json!({"code":0,"data":{"message_id":"om_x"}}).to_string(),
                OutboundMethod::Post => {
                    return Err(BaseError::ConfigError("补行号不应 POST".into()))
                }
            };
            self.requests
                .lock()
                .unwrap_or_else(|e| panic!("{e}"))
                .push(request);
            Ok(OutboundResponse {
                status: 200,
                body,
                bytes: None,
                headers: BTreeMap::new(),
            })
        }
    }

    struct RowEditCache;
    #[async_trait::async_trait]
    impl TenantTokenCache for RowEditCache {
        async fn get(&self) -> anyhow::Result<Option<String>> {
            Ok(Some("t-row-edit".into()))
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

    struct NoSleep;
    #[async_trait::async_trait]
    impl Sleeper for NoSleep {
        async fn sleep(&self, _: Duration) {}
    }

    fn tokens(transport: Arc<RowEditTransport>) -> TenantTokenProvider {
        TenantTokenProvider::new(
            Arc::new(RowEditCache),
            transport,
            Arc::new(NoSleep),
            FeishuCredentials {
                app_id: "cli_test".into(),
                app_secret: "secret".into(),
            },
            "row-edit-test",
        )
        .unwrap_or_else(|e| panic!("{e}"))
    }

    fn edit(message_id: &str, record_id: &str) -> RowNumberEdit {
        RowNumberEdit {
            message_id: message_id.into(),
            status: FeedbackStatus::Failed,
            reason: "缺少必填值".into(),
            names: "配置：付款申请\n数据表：付款表".into(),
            record_id: record_id.into(),
            clicked_at: None,
            base_token: "baseA".into(),
            table_id: "tblA".into(),
            references: vec![
                FeedbackReference {
                    label: "多维表",
                    text: "财务 / 付款表".into(),
                    href: None,
                },
                FeedbackReference {
                    label: "原记录",
                    text: "打开原记录".into(),
                    href: Some("https://tenant.feishu.cn/record/x".into()),
                },
                FeedbackReference {
                    label: "审批",
                    text: "付款审批".into(),
                    href: None,
                },
            ],
        }
    }

    /// 只有**真正送达且带 message_id** 的通知才生成补行号任务；发送失败、
    /// 未请求、跳过或飞书没回 message_id 时都没有可编辑的对象。HTTP 与 worker
    /// 两条发送路径共用这一条判据。
    #[test]
    fn after_send_builds_an_edit_only_for_a_delivered_message() {
        use super::super::approval_notification::{NotificationResult, NotificationStatus};

        let references = vec![FeedbackReference {
            label: "原记录",
            text: "打开原记录".into(),
            href: Some("https://tenant.feishu.cn/record/recA".into()),
        }];
        let sent = NotificationResult {
            status: NotificationStatus::Sent,
            message: "飞书已接受消息。".into(),
            message_id: Some("om_x".into()),
            uuid: Some("uuid".into()),
        };
        let edit = RowNumberEdit::after_send(
            &sent,
            FeedbackStatus::Failed,
            "缺少必填值",
            "配置：付款申请",
            "recA",
            Some("2026-10-26 10:00:00"),
            "baseA",
            "tblA",
            &references,
        )
        .unwrap_or_else(|| panic!("已送达且有 message_id 必须补行号"));
        assert_eq!(edit.message_id, "om_x");
        assert_eq!(edit.status, FeedbackStatus::Failed);
        assert_eq!(edit.reason, "缺少必填值");
        assert_eq!(edit.names, "配置：付款申请");
        assert_eq!(edit.record_id, "recA");
        assert_eq!(edit.clicked_at.as_deref(), Some("2026-10-26 10:00:00"));
        assert_eq!(edit.base_token, "baseA");
        assert_eq!(edit.table_id, "tblA");
        assert_eq!(edit.references.len(), 1);
        assert_eq!(edit.references[0].label, "原记录");

        for status in [
            NotificationStatus::Failed,
            NotificationStatus::NotRequested,
            NotificationStatus::Skipped,
        ] {
            let notification = NotificationResult {
                status,
                message: String::new(),
                message_id: Some("om_x".into()),
                uuid: None,
            };
            assert!(
                RowNumberEdit::after_send(
                    &notification,
                    FeedbackStatus::Failed,
                    "原因",
                    "名称",
                    "recA",
                    None,
                    "base",
                    "tbl",
                    &references,
                )
                .is_none(),
                "未送达的通知不得编辑消息"
            );
        }
        // 送达但飞书没回 message_id：没有可定位的消息，不能补。
        let without_id = NotificationResult {
            status: NotificationStatus::Sent,
            message: String::new(),
            message_id: None,
            uuid: None,
        };
        assert!(
            RowNumberEdit::after_send(
                &without_id,
                FeedbackStatus::Failed,
                "原因",
                "名称",
                "recA",
                None,
                "base",
                "tbl",
                &references,
            )
            .is_none(),
            "没有 message_id 时无从编辑"
        );
    }

    #[tokio::test]
    async fn apply_scans_once_per_table_and_edits_each_message() {
        let transport = Arc::new(RowEditTransport::new(&["recA", "recB"]));
        let tokens = tokens(transport.clone());

        apply(
            transport.as_ref(),
            &NoSleep,
            &tokens,
            vec![edit("om_a", "recA"), edit("om_b", "recB")],
        )
        .await;

        let requests = transport.requests();
        let scans = requests
            .iter()
            .filter(|request| request.method == OutboundMethod::Get)
            .count();
        let edits: Vec<_> = requests
            .iter()
            .filter(|request| request.method == OutboundMethod::Put)
            .collect();
        assert_eq!(scans, 1, "同表两条消息只允许扫一次表");
        assert_eq!(edits.len(), 2, "每条消息各编辑一次");

        let second = edits[1]
            .json_body
            .as_ref()
            .unwrap_or_else(|| panic!("缺少正文"));
        assert_eq!(second["msg_type"], "post");
        let content: Value = serde_json::from_str(
            second["content"]
                .as_str()
                .unwrap_or_else(|| panic!("content 必须是 JSON 字符串")),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let rows = content["zh_cn"]["content"]
            .as_array()
            .unwrap_or_else(|| panic!("缺少段落"));
        assert!(
            rows.iter().any(|row| row[0]["text"] == "记录："
                && row[1]["text"]
                    .as_str()
                    .is_some_and(|value| value.contains("第 2 行"))),
            "第二条记录在总第 2 行: {content}"
        );
        let technical = rows
            .iter()
            .find(|row| row[0]["text"] == "技术定位：")
            .unwrap_or_else(|| panic!("缺少技术定位段: {content}"));
        assert_eq!(technical[1]["text"], "Base=baseA；表=tblA；记录=recB");
        let original = rows
            .iter()
            .find(|row| row[0]["text"] == "原记录：")
            .unwrap_or_else(|| panic!("缺少原记录段: {content}"));
        assert_eq!(original[1]["text"], "第 2 行", "原记录链接文案改成行号");
        assert_eq!(
            original[1]["href"], "https://tenant.feishu.cn/record/x",
            "链接本身保留"
        );
        assert!(
            edits[0].url.ends_with("/im/v1/messages/om_a"),
            "{}",
            edits[0].url
        );
        assert!(
            edits[1].url.ends_with("/im/v1/messages/om_b"),
            "{}",
            edits[1].url
        );
    }

    #[tokio::test]
    async fn apply_leaves_the_message_untouched_when_the_row_is_missing() {
        let transport = Arc::new(RowEditTransport::new(&["recOther"]));
        let tokens = tokens(transport.clone());

        apply(
            transport.as_ref(),
            &NoSleep,
            &tokens,
            vec![edit("om_missing", "recMissing")],
        )
        .await;

        let requests = transport.requests();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method == OutboundMethod::Put)
                .count(),
            0,
            "找不到行号不编辑消息"
        );
    }
}

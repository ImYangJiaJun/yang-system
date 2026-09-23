//! 表级拉取失败的告警：阈值判定、消息构造与投递边界。
//!
//! 设计依据：`docs/architecture/feishu-datasource-table-config.md` §9.2（决策 D5）——
//! 一张表的 `consecutive_failures` 达阈值时，把邮件发给 `feishu.alert_recipients`
//! 里配置的收件人。
//!
//! # 判据为什么是「连续」失败
//!
//! `consecutive_failures` 在整表成功一轮后清零（`pull::record_table_success`）。
//! 于是它自带「已经好起来了」的信息：单次失败与持续故障本来无法区分，而**连续**
//! 失败可以。阈值因此是「抖动不告警」这条要求的唯一落点。
//!
//! # 为什么超过阈值后每轮都发，不做冷却
//!
//! 设计文档明确选择「达阈值 → 发，直到恢复」。加冷却会造出一个**错误**的收敛信号：
//! 「没收到邮件」会被读成「已经恢复」。持续故障时每轮一封的嘈杂，正是它换来的
//! 「故障没被忘记」。收口靠阈值，不靠冷却。

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

use crate::addon::account::email_delivery::{EmailDeliveryError, SmtpEmailSender};

/// 校验一列告警收件人。
///
/// **不配 = 不告警**：空列表是合法配置（设计 D5 的默认值）。
/// 而配了空串是**最坏**的一种——运维以为配上了，邮件永远发不出去，且没有任何症状。
/// 所以空白项必须在配置加载期就报错，而不是静默跳过。
///
/// 返回 `Err` 时带**具体是哪一项、为什么**：运维面对的是一行 `alert_recipients`，
/// 只说「无效」等于让他逐个猜。
pub(crate) fn validate_recipients(recipients: &[String]) -> Result<(), String> {
    for (index, address) in recipients.iter().enumerate() {
        if !address_is_usable(address) {
            return Err(format!(
                "第 {} 项 {address:?} 不是可投递的邮箱地址（空白项、缺 @、域名缺 . 都会被拒）",
                index + 1
            ));
        }
    }
    Ok(())
}

/// 一条地址是否是「可投递的邮箱地址」。
///
/// 刻意**不做**完整的 RFC 5322 校验：收件人是我们自己配置的运维邮箱，误拒一个合法
/// 地址（代价是进程起不来）比放过一个拼错的地址（代价是一封信静默消失）更贵。
/// 所以只挡两类东西——**结构上不可能**是地址的，以及**能把 SMTP 头拆开**的。
fn address_is_usable(address: &str) -> bool {
    // 空白与控制字符一律拒，且**不 trim 后再判**：`" a@b.com "` 这种在配置里
    // 看着配上了，投递时会被 lettre 拒掉——正是「以为配了其实没配」的形态。
    if address
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return false;
    }
    let mut parts = address.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    // 64 是 RFC 5321 对 local-part 的长度上限；域名的 253 同理。
    !local.is_empty() && local.len() <= 64 && domain.len() <= 253 && domain_is_usable(domain)
}

/// 域名部分：必须有点，且每一段都非空。
///
/// 有点这条是**抓笔误**而不是 DNS 规则：`ops@example` 多半是漏了 `.com`。
/// 代价是纯内网单段域名（`ops@mail`）配不上——那种部署要用带点的内网域名
/// （`ops@corp.local`），报错文案已点名是哪一项。
fn domain_is_usable(domain: &str) -> bool {
    domain.contains('.') && domain.split('.').all(|label| !label.is_empty())
}

/// 一张表连续失败 `failures` 次之后，本轮要不要发告警。
///
/// 纯函数：这是「抖动不发邮件风暴」这条要求**唯一**可被单测钉住的地方。
///
/// 非正阈值（配置层有下限，正常配不出来）按「不告警」处理——`failures >= 0` 恒真，
/// 照字面比大小会让阈值 0 在**还没失败过**的时候就开始发邮件。
pub(crate) fn should_alert(failures: i64, threshold: i64) -> bool {
    threshold > 0 && failures >= threshold
}

/// 告警邮件主题。
///
/// 表名来自控制台输入，压成一行再拼：主题里的一处换行等于让表名去改写邮件头。
pub(crate) fn pull_failure_subject(datasource_title: &str) -> String {
    format!(
        "YANG System 飞书数据源拉取失败：{}",
        single_line(datasource_title)
    )
}

/// 告警邮件正文。
///
/// 三样东西缺一不可：**哪张表**、**连续几轮**、**最近一次错误原文**。错误原文是运维
/// 唯一的线索（`1254302` 要去改文档权限、`1254024` 要去改列名，修法完全不同）。
pub(crate) fn pull_failure_body(datasource_title: &str, last_error: &str, failures: i64) -> String {
    format!(
        "飞书数据源「{datasource_title}」已连续 {failures} 轮拉取失败：该表上所有字段的\
         外部选项本轮都没有更新。\n\n\
         最近一次错误：\n{last_error}\n\n\
         请到控制台看该表的「体检」：坐标（Base / 数据表 / 视图）、勾选的字段、以及\
         应用在多维表格里的文档权限。\n\n\
         在恢复之前，每一轮拉取都会再发一次本邮件。"
    )
}

/// 压成单行：控制字符一律换成空格，再掐掉首尾。
fn single_line(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// 表级拉取失败告警的投递边界。
///
/// 形状照 `account::domain::email_delivery::PasswordResetEmailSender`：一个方法 +
/// 一个类型擦除句柄，实现由组合根注入、消费者从 `Tools` 扩展槽取。
#[async_trait]
pub(crate) trait FeishuAlertSender: Send + Sync + 'static {
    /// 投递一封表级拉取失败告警。
    async fn send_pull_failure(
        &self,
        recipient: &str,
        datasource_title: &str,
        last_error: &str,
        failures: i64,
    ) -> Result<(), EmailDeliveryError>;
}

/// SMTP 适配器：告警走的是与验证码 / 重置链接**同一条** STARTTLS 传输。
///
/// 为什么实现在这里而不是 `account` 的邮件模块里：依赖方向要保持单向。`feishu`
/// 已经依赖 `account`（`user_from_claims`），反过来再依赖一次就成环了；而 trait
/// 定义在飞书这一侧，消息文案也跟着留在这一侧。
#[async_trait]
impl FeishuAlertSender for SmtpEmailSender {
    async fn send_pull_failure(
        &self,
        recipient: &str,
        datasource_title: &str,
        last_error: &str,
        failures: i64,
    ) -> Result<(), EmailDeliveryError> {
        self.deliver_text(
            recipient,
            &pull_failure_subject(datasource_title),
            pull_failure_body(datasource_title, last_error, failures),
        )
        .await
    }
}

/// 可放入 `Tools` 扩展槽的类型擦除投递句柄。
#[derive(Clone)]
pub(crate) struct FeishuAlertSenderHandle(Arc<dyn FeishuAlertSender>);

impl FeishuAlertSenderHandle {
    /// 从已共享的投递器创建句柄。
    pub(crate) fn from_arc(sender: Arc<dyn FeishuAlertSender>) -> Self {
        Self(sender)
    }

    /// 投递一封表级拉取失败告警。
    pub(crate) async fn send_pull_failure(
        &self,
        recipient: &str,
        datasource_title: &str,
        last_error: &str,
        failures: i64,
    ) -> Result<(), EmailDeliveryError> {
        self.0
            .send_pull_failure(recipient, datasource_title, last_error, failures)
            .await
    }
}

impl fmt::Debug for FeishuAlertSenderHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FeishuAlertSenderHandle")
            .finish_non_exhaustive()
    }
}

/// 一次表级失败之后按阈值决定要不要告警，要就发给**每一个**收件人。
///
/// 返回实际投递出去的封数。**best-effort**：单封投递失败只记日志、不上抛——告警
/// 发不出去不该改变拉取结果，更不该把刚落库的失败状态回滚。
///
/// 未达阈值、未配收件人都直接返回 0：不告警是**正常**路径，不是错误。
pub(crate) async fn alert_pull_failure(
    sender: &FeishuAlertSenderHandle,
    recipients: &[String],
    threshold: i64,
    datasource_title: &str,
    last_error: &str,
    failures: i64,
) -> usize {
    if !should_alert(failures, threshold) {
        return 0;
    }
    if recipients.is_empty() {
        // 达阈值却无人可通知：这是**配置缺口**，不是正常路径。静默会让「配了阈值
        // 却没配收件人」的部署以为告警在跑。
        tracing::warn!(
            datasource_title,
            failures,
            "飞书数据源已连续失败达告警阈值，但 feishu.alert_recipients 未配置收件人"
        );
        return 0;
    }

    let mut delivered = 0;
    for recipient in recipients {
        match sender
            .send_pull_failure(recipient, datasource_title, last_error, failures)
            .await
        {
            Ok(()) => delivered += 1,
            // 一个收件人投递失败不影响其它收件人：告警的全部价值在于「有人看到」。
            Err(error) => {
                tracing::error!(error = %error, recipient = %recipient, "飞书拉取失败告警邮件投递失败");
            }
        }
    }
    delivered
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 只记录、不投递的发送器。
    #[derive(Default)]
    struct RecordingSender {
        sent: Mutex<Vec<(String, String, i64)>>,
    }

    impl RecordingSender {
        fn sent(&self) -> Vec<(String, String, i64)> {
            self.sent
                .lock()
                .map(|sent| sent.clone())
                .unwrap_or_default()
        }
    }

    #[async_trait]
    impl FeishuAlertSender for RecordingSender {
        async fn send_pull_failure(
            &self,
            recipient: &str,
            datasource_title: &str,
            last_error: &str,
            failures: i64,
        ) -> Result<(), EmailDeliveryError> {
            if let Ok(mut sent) = self.sent.lock() {
                sent.push((
                    recipient.to_string(),
                    format!("{datasource_title}|{last_error}"),
                    failures,
                ));
            }
            Ok(())
        }
    }

    fn recipient(address: &str) -> String {
        address.to_string()
    }

    #[test]
    fn blank_recipients_are_rejected_at_config_time() {
        // 配了一个空串 = 以为配了其实没配，邮件永远发不出去
        assert!(validate_recipients(&["a@b.com".into(), "  ".into()]).is_err());
    }

    #[test]
    fn an_invalid_address_is_rejected() {
        assert!(validate_recipients(&["not-an-email".into()]).is_err());
    }

    #[test]
    fn empty_list_means_alerts_disabled() {
        assert!(
            validate_recipients(&[]).is_ok(),
            "不配 = 不告警，是合法配置"
        );
    }

    #[test]
    fn alert_fires_only_at_the_threshold() {
        // 抖动时不发邮件风暴：1 次失败不发，达到阈值才发
        assert!(!should_alert(1, 3));
        assert!(should_alert(3, 3));
        assert!(should_alert(4, 3), "超过阈值后每轮都应继续告警，直到恢复");
    }

    #[test]
    fn a_non_positive_threshold_never_alerts() {
        // 阈值 0 照字面比大小会在**还没失败过**的时候就开始发邮件
        assert!(!should_alert(0, 0));
        assert!(!should_alert(5, 0));
    }

    #[test]
    fn a_realistic_list_passes() {
        assert!(
            validate_recipients(&["ops@example.com".into(), "oncall@example.com.cn".into()])
                .is_ok()
        );
    }

    #[test]
    fn addresses_that_could_split_an_smtp_header_are_rejected() {
        // 换行会被写进 SMTP 头，等于让收件人字段去改写别的头
        for address in [
            "ops@example.com\nBcc: attacker@example.com",
            "ops@example.com\r\nSubject: 伪造",
            "ops @example.com",
            "ops@example",
        ] {
            assert!(
                validate_recipients(&[recipient(address)]).is_err(),
                "{address:?} 必须被拒"
            );
        }
    }

    #[test]
    fn a_title_with_a_newline_cannot_split_the_subject_header() {
        // 表名来自控制台输入，直接拼进主题会让它改写邮件头
        let subject = pull_failure_subject("费用表\nBcc: attacker@example.com");
        assert!(!subject.contains('\n'), "主题里不得残留换行: {subject:?}");
        assert!(!subject.contains('\r'), "主题里不得残留换行: {subject:?}");
    }

    #[test]
    fn the_message_names_the_table_the_count_and_the_last_error() {
        let body = pull_failure_body("费用表", "1254302 Permission denied", 7);
        assert!(body.contains("费用表"), "要点名是哪张表: {body}");
        assert!(body.contains('7'), "要点名连续失败了几轮: {body}");
        assert!(
            body.contains("1254302 Permission denied"),
            "要带上最近一次错误原文（运维唯一的线索）: {body}"
        );
    }

    #[tokio::test]
    async fn alerts_fan_out_to_every_recipient_at_the_threshold() {
        let recorder = Arc::new(RecordingSender::default());
        let sender: Arc<dyn FeishuAlertSender> = recorder.clone();
        let handle = FeishuAlertSenderHandle::from_arc(sender);
        let recipients = [
            "ops@example.com".to_string(),
            "oncall@example.com".to_string(),
        ];

        let delivered = alert_pull_failure(&handle, &recipients, 3, "费用表", "上游超时", 3).await;

        assert_eq!(delivered, 2, "阈值到达时**每个**收件人都要收到");
        assert_eq!(recorder.sent().len(), 2, "漏发一个收件人等于有人永远不知情");
    }

    #[tokio::test]
    async fn nothing_is_sent_below_the_threshold_or_without_recipients() {
        let recorder = Arc::new(RecordingSender::default());
        let sender: Arc<dyn FeishuAlertSender> = recorder.clone();
        let handle = FeishuAlertSenderHandle::from_arc(sender);
        let recipients = ["ops@example.com".to_string()];

        assert_eq!(
            alert_pull_failure(&handle, &recipients, 3, "费用表", "上游超时", 2).await,
            0,
            "抖动（未达阈值）不发邮件"
        );
        assert_eq!(
            alert_pull_failure(&handle, &[], 3, "费用表", "上游超时", 9).await,
            0,
            "没配收件人不发邮件（合法配置）"
        );
        assert!(recorder.sent().is_empty(), "上面两次都不该真的投递");
    }
}

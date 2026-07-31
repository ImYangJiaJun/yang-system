//! 注册邮箱验证码的投递边界。
//!
//! 账户领域只依赖 [`RegistrationEmailSender`]；SMTP 是生产适配器，测试可注入内存实现。

use crate::config::SmtpSettings;
use async_trait::async_trait;
use lettre::message::{header::ContentType, Mailbox};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

/// 邮件投递失败的脱敏类别；不携带 SMTP 响应、收件人或凭据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailDeliveryError {
    /// 邮件内容或地址无法构造。
    InvalidMessage,
    /// SMTP 服务未接受邮件或暂不可用。
    Unavailable,
}

impl fmt::Display for EmailDeliveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMessage => formatter.write_str("邮件内容无效"),
            Self::Unavailable => formatter.write_str("邮件服务暂不可用"),
        }
    }
}

impl std::error::Error for EmailDeliveryError {}

/// 注册验证码投递接口。实现方不得记录 `recipient` 或 `code` 原文。
#[async_trait]
pub trait RegistrationEmailSender: Send + Sync + 'static {
    /// 投递一枚短期注册验证码。
    async fn send_registration_code(
        &self,
        recipient: &str,
        code: &str,
        expires_in_seconds: u64,
    ) -> Result<(), EmailDeliveryError>;
}

/// 可放入 `Tools` 的类型擦除投递句柄。
#[derive(Clone)]
pub struct RegistrationEmailSenderHandle(Arc<dyn RegistrationEmailSender>);

impl RegistrationEmailSenderHandle {
    pub fn new<T>(sender: T) -> Self
    where
        T: RegistrationEmailSender,
    {
        Self(Arc::new(sender))
    }

    pub fn from_arc(sender: Arc<dyn RegistrationEmailSender>) -> Self {
        Self(sender)
    }

    pub(crate) async fn send_registration_code(
        &self,
        recipient: &str,
        code: &str,
        expires_in_seconds: u64,
    ) -> Result<(), EmailDeliveryError> {
        self.0
            .send_registration_code(recipient, code, expires_in_seconds)
            .await
    }
}

impl fmt::Debug for RegistrationEmailSenderHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegistrationEmailSenderHandle")
            .finish_non_exhaustive()
    }
}

/// 强制 STARTTLS 的生产 SMTP 适配器。
#[derive(Clone)]
pub(crate) struct SmtpRegistrationEmailSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl SmtpRegistrationEmailSender {
    pub(crate) fn new(settings: &SmtpSettings) -> anyhow::Result<Self> {
        let from_address = settings
            .from_address
            .parse()
            .map_err(|_| anyhow::anyhow!("email.smtp.from_address 不是合法邮箱地址"))?;
        let from = Mailbox::new(Some(settings.from_name.trim().to_string()), from_address);
        let mut builder =
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(settings.relay.trim())
                .map_err(|_| anyhow::anyhow!("构建 SMTP STARTTLS 参数失败"))?
                .port(settings.port)
                .timeout(Some(Duration::from_secs(settings.timeout_seconds)));
        if !settings.username.trim().is_empty() {
            builder = builder.credentials(Credentials::new(
                settings.username.clone(),
                settings.password.clone(),
            ));
        }
        Ok(Self {
            transport: builder.build(),
            from,
        })
    }
}

#[async_trait]
impl RegistrationEmailSender for SmtpRegistrationEmailSender {
    async fn send_registration_code(
        &self,
        recipient: &str,
        code: &str,
        expires_in_seconds: u64,
    ) -> Result<(), EmailDeliveryError> {
        let recipient = recipient
            .parse::<Mailbox>()
            .map_err(|_| EmailDeliveryError::InvalidMessage)?;
        let minutes = expires_in_seconds.div_ceil(60);
        let message = Message::builder()
            .from(self.from.clone())
            .to(recipient)
            .subject("YANG System 注册邮箱验证码")
            .header(ContentType::TEXT_PLAIN)
            .body(format!(
                "你的注册验证码是：{code}\n\n验证码将在 {minutes} 分钟后失效，且只能使用一次。若非本人操作，请忽略本邮件。"
            ))
            .map_err(|_| EmailDeliveryError::InvalidMessage)?;
        self.transport
            .send(message)
            .await
            .map_err(|_| EmailDeliveryError::Unavailable)?;
        Ok(())
    }
}

//! 注册邮箱验证码的投递边界。
//!
//! 投递契约（[`RegistrationEmailSender`] / [`RegistrationEmailSenderHandle`] /
//! [`EmailDeliveryError`]）由 `yang_base::action::auth` 提供并在此再导出；
//! 本模块只保留强制 STARTTLS 的生产 SMTP 适配器，测试可注入内存实现。

use crate::config::SmtpSettings;
use async_trait::async_trait;
use lettre::message::{header::ContentType, Mailbox};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use std::time::Duration;

pub use yang_base::action::auth::{
    EmailDeliveryError, RegistrationEmailSender, RegistrationEmailSenderHandle,
};

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

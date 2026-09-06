//! 事务性邮件的投递边界：注册验证码与密码重置链接。
//!
//! 注册验证码的投递契约（[`RegistrationEmailSender`] / [`RegistrationEmailSenderHandle`] /
//! [`EmailDeliveryError`]）由 `yang_base::action::auth` 提供并在此再导出；
//! 密码重置链接的投递契约（[`PasswordResetEmailSender`] /
//! [`PasswordResetEmailSenderHandle`]）由本模块定义，链接地址经
//! [`PasswordResetLinkConfig`] 从 `Tools` config 槽注入。
//! 本模块只保留强制 STARTTLS 的生产 SMTP 适配器，测试可注入内存实现。

use crate::config::SmtpSettings;
use async_trait::async_trait;
use lettre::message::{header::ContentType, Mailbox};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

pub use yang_base::action::auth::{
    EmailDeliveryError, RegistrationEmailSender, RegistrationEmailSenderHandle,
};

/// 密码重置邮件投递接口，由业务实现并注入。
///
/// 实现方不得记录 `recipient` 或 `reset_url` 原文（链接内含明文凭证）。
#[async_trait]
pub trait PasswordResetEmailSender: Send + Sync + 'static {
    /// 投递一封含一次性重置链接的邮件。
    async fn send_password_reset_link(
        &self,
        recipient: &str,
        reset_url: &str,
        expires_in_seconds: u64,
    ) -> Result<(), EmailDeliveryError>;
}

/// 可放入 `Tools` 扩展槽的类型擦除投递句柄。
#[derive(Clone)]
pub struct PasswordResetEmailSenderHandle(Arc<dyn PasswordResetEmailSender>);

impl PasswordResetEmailSenderHandle {
    /// 用业务投递器创建句柄。
    pub fn new<T>(sender: T) -> Self
    where
        T: PasswordResetEmailSender,
    {
        Self(Arc::new(sender))
    }

    /// 从已共享的投递器创建句柄。
    pub fn from_arc(sender: Arc<dyn PasswordResetEmailSender>) -> Self {
        Self(sender)
    }

    /// 投递一封含一次性重置链接的邮件。
    pub async fn send_password_reset_link(
        &self,
        recipient: &str,
        reset_url: &str,
        expires_in_seconds: u64,
    ) -> Result<(), EmailDeliveryError> {
        self.0
            .send_password_reset_link(recipient, reset_url, expires_in_seconds)
            .await
    }
}

impl fmt::Debug for PasswordResetEmailSenderHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PasswordResetEmailSenderHandle")
            .finish_non_exhaustive()
    }
}

/// 新设备登录提醒邮件投递接口（路线图 C-3，best-effort 不阻塞登录路径）。
#[async_trait]
pub trait NewDeviceEmailSender: Send + Sync + 'static {
    /// 通知用户一次来自新设备的成功登录。
    async fn send_new_device_login(
        &self,
        recipient: &str,
        ip: &str,
        user_agent: &str,
        _occurred_at_unix: i64,
    ) -> Result<(), EmailDeliveryError>;
}

/// 可放入 `Tools` 扩展槽的类型擦除投递句柄。
#[derive(Clone)]
pub struct NewDeviceEmailSenderHandle(Arc<dyn NewDeviceEmailSender>);

impl NewDeviceEmailSenderHandle {
    /// 用业务投递器创建句柄。
    pub fn new<T>(sender: T) -> Self
    where
        T: NewDeviceEmailSender,
    {
        Self(Arc::new(sender))
    }

    /// 从已共享的投递器创建句柄。
    pub fn from_arc(sender: Arc<dyn NewDeviceEmailSender>) -> Self {
        Self(sender)
    }

    /// 投递一封新设备登录提醒邮件。
    pub async fn send_new_device_login(
        &self,
        recipient: &str,
        ip: &str,
        user_agent: &str,
        _occurred_at_unix: i64,
    ) -> Result<(), EmailDeliveryError> {
        self.0
            .send_new_device_login(recipient, ip, user_agent, _occurred_at_unix)
            .await
    }
}

impl fmt::Debug for NewDeviceEmailSenderHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NewDeviceEmailSenderHandle")
            .finish_non_exhaustive()
    }
}

/// 密码重置邮件链接的地址配置，经 `Tools` config 槽注入。
///
/// 地址必须是部署方显式配置的前端控制台入口，不得从请求头推导（Host 头可被攻击者伪造）。
#[derive(Debug, Clone)]
pub struct PasswordResetLinkConfig {
    /// 前端控制台地址（scheme + host[:port]，无路径与查询串）。
    pub base_url: String,
}

impl PasswordResetLinkConfig {
    /// 用明文凭证拼装一次性重置链接（前端 `ResetPasswordPage` 预填 `token` 参数）。
    pub fn reset_url(&self, raw_token: &str) -> String {
        format!("{}/reset-password?token={raw_token}", self.base_url)
    }
}

/// 强制 STARTTLS 的生产 SMTP 适配器，承载注册验证码与密码重置两类事务性邮件。
#[derive(Clone)]
pub(crate) struct SmtpEmailSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl SmtpEmailSender {
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

    async fn deliver(
        &self,
        recipient: &str,
        subject: &str,
        body: String,
    ) -> Result<(), EmailDeliveryError> {
        let recipient = recipient
            .parse::<Mailbox>()
            .map_err(|_| EmailDeliveryError::InvalidMessage)?;
        let message = Message::builder()
            .from(self.from.clone())
            .to(recipient)
            .subject(subject)
            .header(ContentType::TEXT_PLAIN)
            .body(body)
            .map_err(|_| EmailDeliveryError::InvalidMessage)?;
        self.transport
            .send(message)
            .await
            .map_err(|_| EmailDeliveryError::Unavailable)?;
        Ok(())
    }
}

#[async_trait]
impl RegistrationEmailSender for SmtpEmailSender {
    async fn send_registration_code(
        &self,
        recipient: &str,
        code: &str,
        expires_in_seconds: u64,
    ) -> Result<(), EmailDeliveryError> {
        let minutes = expires_in_seconds.div_ceil(60);
        self.deliver(
            recipient,
            "YANG System 注册邮箱验证码",
            format!(
                "你的注册验证码是：{code}\n\n验证码将在 {minutes} 分钟后失效，且只能使用一次。若非本人操作，请忽略本邮件。"
            ),
        )
        .await
    }
}

#[async_trait]
impl PasswordResetEmailSender for SmtpEmailSender {
    async fn send_password_reset_link(
        &self,
        recipient: &str,
        reset_url: &str,
        expires_in_seconds: u64,
    ) -> Result<(), EmailDeliveryError> {
        let minutes = expires_in_seconds.div_ceil(60);
        self.deliver(
            recipient,
            "YANG System 密码重置",
            format!(
                "你收到本邮件是因为有人请求重置该账号的密码。\n\n请在 {minutes} 分钟内打开以下链接设置新密码（链接只能使用一次）：\n{reset_url}\n\n若非本人操作，请忽略本邮件，你的密码不会改变。"
            ),
        )
        .await
    }
}

#[async_trait]
impl NewDeviceEmailSender for SmtpEmailSender {
    async fn send_new_device_login(
        &self,
        recipient: &str,
        ip: &str,
        user_agent: &str,
        _occurred_at_unix: i64,
    ) -> Result<(), EmailDeliveryError> {
        let agent = if user_agent.trim().is_empty() {
            "未知设备".to_string()
        } else {
            user_agent.chars().take(120).collect()
        };
        self.deliver(
            recipient,
            "YANG System 新设备登录提醒",
            format!(
                "你的账号刚刚从一台新设备成功登录。

IP：{ip}
设备：{agent}

如果这是你本人的操作，可以忽略本邮件；如果不是，请立即修改密码并检查会话列表。"
            ),
        )
        .await
    }
}


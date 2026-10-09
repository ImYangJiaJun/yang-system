//! 事务性邮件的投递边界：注册验证码、密码重置链接、新设备提醒、MFA 登录验证码与免密登录验证码。
//!
//! 注册验证码的投递契约（[`RegistrationEmailSender`] / [`RegistrationEmailSenderHandle`] /
//! [`EmailDeliveryError`]）与通用验证码投递契约（[`VerificationCodeSender`] /
//! [`VerificationCodeSenderHandle`]，供登录 MFA 备用邮箱验证码使用）由
//! `yang_base::action::auth` 提供并在此再导出；
//! 密码重置链接的投递契约（[`PasswordResetEmailSender`] /
//! [`PasswordResetEmailSenderHandle`]）由本模块定义，链接地址经
//! [`PasswordResetLinkConfig`] 从 `Tools` config 槽注入。
//! 本模块只保留强制 STARTTLS 的生产 SMTP 适配器，测试可注入内存实现。

use super::email_templates::EmailTemplates;
use crate::config::EmailSettings;
use async_trait::async_trait;
use lettre::message::{Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

pub use yang_base::action::auth::{
    EmailDeliveryError, RegistrationEmailSender, RegistrationEmailSenderHandle,
    VerificationCodeSender, VerificationCodeSenderHandle,
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

/// 强制 STARTTLS 的生产 SMTP 适配器，统一发送可替换的 HTML 与纯文本模板。
#[derive(Clone)]
pub(crate) struct SmtpEmailSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    system_name: String,
    templates: Arc<EmailTemplates>,
}

impl SmtpEmailSender {
    pub(crate) fn new(email: &EmailSettings, system_name: &str) -> anyhow::Result<Self> {
        let templates =
            EmailTemplates::load(email.template_dir.as_deref().map(std::path::Path::new))?;
        let settings = &email.smtp;
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
            system_name: system_name.trim().to_owned(),
            templates: Arc::new(templates),
        })
    }

    pub(crate) async fn deliver_template(
        &self,
        recipient: &str,
        kind: &str,
        values: &[(&str, &str)],
    ) -> Result<(), EmailDeliveryError> {
        let (subject, plain, html) = self
            .templates
            .render(kind, &self.system_name, values)
            .map_err(|_| EmailDeliveryError::InvalidMessage)?;
        let recipient = recipient
            .parse::<Mailbox>()
            .map_err(|_| EmailDeliveryError::InvalidMessage)?;
        let message = Message::builder()
            .from(self.from.clone())
            .to(recipient)
            .subject(subject)
            .multipart(
                MultiPart::alternative()
                    .singlepart(SinglePart::plain(plain))
                    .singlepart(SinglePart::html(html)),
            )
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
        let minutes = expires_in_seconds.div_ceil(60).to_string();
        self.deliver_template(
            recipient,
            "registration",
            &[("code", code), ("minutes", &minutes)],
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
        let minutes = expires_in_seconds.div_ceil(60).to_string();
        self.deliver_template(
            recipient,
            "password_reset",
            &[("reset_url", reset_url), ("minutes", &minutes)],
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
        occurred_at_unix: i64,
    ) -> Result<(), EmailDeliveryError> {
        let agent = if user_agent.trim().is_empty() {
            "未知设备".to_string()
        } else {
            user_agent.chars().take(120).collect()
        };
        self.deliver_template(
            recipient,
            "new_device",
            &[
                ("ip", ip),
                ("device", &agent),
                ("occurred_at", &occurred_at_unix.to_string()),
            ],
        )
        .await
    }
}

#[async_trait]
impl VerificationCodeSender for SmtpEmailSender {
    async fn send_verification_code(
        &self,
        recipient: &str,
        code: &str,
        expires_in_seconds: u64,
    ) -> Result<(), EmailDeliveryError> {
        let minutes = expires_in_seconds.div_ceil(60).to_string();
        self.deliver_template(recipient, "mfa", &[("code", code), ("minutes", &minutes)])
            .await
    }
}

/// 免密登录邮箱验证码投递句柄的独立 extension 槽类型。
///
/// `Tools` 的 extension 按具体 Rust 类型索引，MFA 备用验证码已占用
/// [`VerificationCodeSenderHandle`] 槽；免密登录验证码文案不同，必须用
/// distinct newtype 区分两个槽，避免相互覆盖。
#[derive(Clone)]
pub struct LoginEmailCodeSenderHandle(VerificationCodeSenderHandle);

impl LoginEmailCodeSenderHandle {
    /// 用业务投递器创建句柄。
    pub fn new<T>(sender: T) -> Self
    where
        T: VerificationCodeSender,
    {
        Self(VerificationCodeSenderHandle::new(sender))
    }

    /// 取内部通用验证码投递句柄（供框架验证码引擎 `request_via` 使用）。
    pub fn engine(&self) -> &VerificationCodeSenderHandle {
        &self.0
    }
}

impl fmt::Debug for LoginEmailCodeSenderHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoginEmailCodeSenderHandle")
            .finish_non_exhaustive()
    }
}

/// 免密登录验证码的生产 SMTP 适配器：复用同一 SMTP 传输，文案与 MFA 验证码独立。
#[derive(Clone)]
pub(crate) struct SmtpLoginEmailCodeSender {
    inner: SmtpEmailSender,
}

impl SmtpLoginEmailCodeSender {
    pub(crate) fn new(inner: SmtpEmailSender) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl VerificationCodeSender for SmtpLoginEmailCodeSender {
    async fn send_verification_code(
        &self,
        recipient: &str,
        code: &str,
        expires_in_seconds: u64,
    ) -> Result<(), EmailDeliveryError> {
        let minutes = expires_in_seconds.div_ceil(60).to_string();
        self.inner
            .deliver_template(recipient, "login", &[("code", code), ("minutes", &minutes)])
            .await
    }
}

/// 换绑验证码占用独立投递槽，保持文案与注册、登录验证码分离。
#[derive(Clone, Debug)]
pub struct ChangeEmailCodeSenderHandle(pub VerificationCodeSenderHandle);

#[derive(Clone)]
pub(crate) struct SmtpChangeEmailCodeSender(pub SmtpEmailSender);

#[async_trait]
impl VerificationCodeSender for SmtpChangeEmailCodeSender {
    async fn send_verification_code(
        &self,
        recipient: &str,
        code: &str,
        expires_in_seconds: u64,
    ) -> Result<(), EmailDeliveryError> {
        self.0
            .deliver_template(
                recipient,
                "change_email",
                &[
                    ("code", code),
                    ("minutes", &expires_in_seconds.div_ceil(60).to_string()),
                ],
            )
            .await
    }
}

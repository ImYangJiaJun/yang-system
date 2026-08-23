use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use yang_base::tools::ToolsBuilder;
use yang_system::addon::account::email_delivery::{
    EmailDeliveryError, RegistrationEmailSender, RegistrationEmailSenderHandle,
};
use yang_system::config::EmailVerificationSettings;

#[derive(Clone, Default)]
struct CapturingRegistrationEmailSender {
    codes: Arc<Mutex<BTreeMap<String, String>>>,
}

#[async_trait]
impl RegistrationEmailSender for CapturingRegistrationEmailSender {
    async fn send_registration_code(
        &self,
        recipient: &str,
        code: &str,
        _expires_in_seconds: u64,
    ) -> Result<(), EmailDeliveryError> {
        self.codes
            .lock()
            .map_err(|_| EmailDeliveryError::Unavailable)?
            .insert(recipient.to_owned(), code.to_owned());
        Ok(())
    }
}

fn sender() -> CapturingRegistrationEmailSender {
    static SENDER: OnceLock<CapturingRegistrationEmailSender> = OnceLock::new();
    SENDER.get_or_init(Default::default).clone()
}

pub trait RegistrationEmailToolsExt {
    fn with_registration_email(self, namespace: impl Into<String>) -> Self;
}

impl RegistrationEmailToolsExt for ToolsBuilder {
    fn with_registration_email(self, namespace: impl Into<String>) -> Self {
        self.extension(RegistrationEmailSenderHandle::new(sender()))
            .config(
                EmailVerificationSettings {
                    namespace: namespace.into(),
                    secret: "integration-email-verification-secret-32-bytes-minimum".to_owned(),
                    ttl_seconds: 300,
                    resend_cooldown_seconds: 1,
                    max_attempts: 5,
                    send_window_seconds: 60,
                    send_ip_attempts: 10_000,
                    send_email_attempts: 1_000,
                    send_global_attempts: 100_000,
                }
                .engine_config(),
            )
    }
}

pub fn take_registration_code(email: &str) -> anyhow::Result<String> {
    let normalized = email.trim().to_ascii_lowercase();
    sender()
        .codes
        .lock()
        .map_err(|_| anyhow::anyhow!("集成测试邮件缓冲区锁已损坏"))?
        .remove(&normalized)
        .ok_or_else(|| anyhow::anyhow!("集成测试未收到注册验证码"))
}

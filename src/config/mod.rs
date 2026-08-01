mod source;

use anyhow::{bail, Context};
use serde::Deserialize;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;
use yang_base::action::StepUpManager;
use yang_base::token::TokenManager;
use yang_db::{DatabaseConfig, RedisConfig};
pub use yang_runtime::observability::ObservabilitySettings;

const MAX_ACCESS_TTL_SECONDS: u64 = 24 * 60 * 60;
const MAX_REFRESH_TTL_SECONDS: u64 = 90 * 24 * 60 * 60;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub app: AppSettings,
    pub authorization: AuthorizationSettings,
    pub http: HttpSettings,
    pub mysql: MysqlSettings,
    pub redis: RedisSettings,
    pub token: TokenSettings,
    pub step_up: StepUpSettings,
    pub email: EmailSettings,
    pub security: SecuritySettings,
    #[serde(default)]
    pub shutdown: ShutdownSettings,
    #[serde(default)]
    pub observability: ObservabilitySettings,
    pub logging: LoggingSettings,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSettings {
    pub name: String,
    #[serde(default)]
    pub environment: DeploymentEnvironment,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentEnvironment {
    Development,
    Test,
    #[default]
    Production,
}

impl DeploymentEnvironment {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Test => "test",
            Self::Production => "production",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationSettings {
    pub deployment: String,
    pub outbox_poll_interval_ms: u64,
    pub outbox_batch_size: u32,
    pub outbox_lease_seconds: u64,
    pub outbox_max_retry_seconds: u64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpSettings {
    pub bind: String,
    pub max_body_bytes: usize,
    pub request_timeout_seconds: u64,
    pub max_concurrency: usize,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MysqlSettings {
    pub url: String,
    pub max_connections: u32,
    pub min_connections: u32,
    pub connect_timeout_seconds: u64,
    pub idle_timeout_seconds: u64,
    pub max_lifetime_seconds: Option<u64>,
    pub test_before_acquire: bool,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedisSettings {
    pub url: String,
    pub max_connections: usize,
    pub min_connections: usize,
    pub connect_timeout_seconds: u64,
    pub wait_timeout_seconds: u64,
    pub idle_timeout_seconds: u64,
    pub max_lifetime_seconds: Option<u64>,
    pub test_before_acquire: bool,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenSettings {
    pub active_key_id: String,
    pub active_secret: String,
    #[serde(default)]
    pub retiring_keys: Vec<RetiringTokenKeySettings>,
    pub issuer: String,
    pub audience: String,
    pub access_ttl_seconds: u64,
    pub refresh_ttl_seconds: u64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetiringTokenKeySettings {
    pub key_id: String,
    pub secret: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepUpSettings {
    pub active_key_id: String,
    pub active_secret: String,
    #[serde(default)]
    pub retiring_keys: Vec<RetiringTokenKeySettings>,
    pub issuer: String,
    pub audience: String,
    pub challenge_ttl_seconds: u64,
    pub proof_ttl_seconds: u64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailSettings {
    pub smtp: SmtpSettings,
    pub verification: EmailVerificationSettings,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmtpSettings {
    pub relay: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub from_address: String,
    pub from_name: String,
    pub timeout_seconds: u64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailVerificationSettings {
    /// Redis key namespace，隔离共享 Redis 上的部署环境。
    pub namespace: String,
    /// 验证码摘要的独立服务端密钥，不得与 Token/Step-up 密钥复用。
    pub secret: String,
    pub ttl_seconds: u64,
    pub resend_cooldown_seconds: u64,
    pub max_attempts: u32,
    pub send_window_seconds: u64,
    pub send_ip_attempts: u64,
    pub send_email_attempts: u64,
    pub send_global_attempts: u64,
}

impl std::fmt::Debug for EmailSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmailSettings")
            .field("smtp", &self.smtp)
            .field("verification", &self.verification)
            .finish()
    }
}

impl std::fmt::Debug for SmtpSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SmtpSettings")
            .field("relay", &self.relay)
            .field("port", &self.port)
            .field("username", &"[REDACTED]")
            .field("password", &"[REDACTED]")
            .field("from_address", &self.from_address)
            .field("from_name", &self.from_name)
            .field("timeout_seconds", &self.timeout_seconds)
            .finish()
    }
}

impl std::fmt::Debug for EmailVerificationSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmailVerificationSettings")
            .field("namespace", &self.namespace)
            .field("secret", &"[REDACTED]")
            .field("ttl_seconds", &self.ttl_seconds)
            .field("resend_cooldown_seconds", &self.resend_cooldown_seconds)
            .field("max_attempts", &self.max_attempts)
            .field("send_window_seconds", &self.send_window_seconds)
            .field("send_ip_attempts", &self.send_ip_attempts)
            .field("send_email_attempts", &self.send_email_attempts)
            .field("send_global_attempts", &self.send_global_attempts)
            .finish()
    }
}

impl std::fmt::Debug for TokenSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TokenSettings")
            .field("active_key_id", &self.active_key_id)
            .field("active_secret", &"[REDACTED]")
            .field(
                "retiring_key_ids",
                &self
                    .retiring_keys
                    .iter()
                    .map(|key| key.key_id.as_str())
                    .collect::<Vec<_>>(),
            )
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .field("access_ttl_seconds", &self.access_ttl_seconds)
            .field("refresh_ttl_seconds", &self.refresh_ttl_seconds)
            .finish()
    }
}

impl std::fmt::Debug for RetiringTokenKeySettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RetiringTokenKeySettings")
            .field("key_id", &self.key_id)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

impl std::fmt::Debug for StepUpSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StepUpSettings")
            .field("active_key_id", &self.active_key_id)
            .field("active_secret", &"[REDACTED]")
            .field(
                "retiring_key_ids",
                &self
                    .retiring_keys
                    .iter()
                    .map(|key| key.key_id.as_str())
                    .collect::<Vec<_>>(),
            )
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .field("challenge_ttl_seconds", &self.challenge_ttl_seconds)
            .field("proof_ttl_seconds", &self.proof_ttl_seconds)
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecuritySettings {
    pub argon2_max_concurrency: usize,
    pub auth_rate_limit_window_seconds: u64,
    pub auth_rate_limit_ip_attempts: u64,
    pub auth_rate_limit_username_attempts: u64,
    /// 密码重置凭证的短期有效期；旧配置缺省为 15 分钟。
    #[serde(default = "default_password_reset_ttl_seconds")]
    pub password_reset_ttl_seconds: u64,
    /// 所有实例均已支持凭据版本读取后，才开启新 Refresh 字段签发与凭据写 Action。
    #[serde(default)]
    pub issue_refresh_credential_version: bool,
    /// 允许提供 `Forwarded`/`X-Forwarded-For` 的 TCP 对端网段；空列表表示完全忽略。
    #[serde(default)]
    pub trusted_proxy_cidrs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggingSettings {
    pub filter: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownSettings {
    #[serde(default = "default_shutdown_total_timeout_seconds")]
    pub total_timeout_seconds: u64,
}

impl Default for ShutdownSettings {
    fn default() -> Self {
        Self {
            total_timeout_seconds: default_shutdown_total_timeout_seconds(),
        }
    }
}

const fn default_shutdown_total_timeout_seconds() -> u64 {
    30
}

impl Settings {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let settings: Self = crate::config::source::load(path, "读取配置文件失败")?;
        settings.validate()?;
        Ok(settings)
    }

    #[cfg(test)]
    fn parse(raw: &str) -> anyhow::Result<Self> {
        let settings: Self = crate::config::source::parse_file_only(raw)?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn bind_addr(&self) -> anyhow::Result<SocketAddr> {
        self.http
            .bind
            .parse()
            .with_context(|| format!("HTTP bind 地址无效: {}", self.http.bind))
    }

    pub fn mysql_config(&self) -> DatabaseConfig {
        self.mysql.database_config()
    }

    pub fn redis_config(&self) -> RedisConfig {
        RedisConfig::default()
            .with_max_connections(self.redis.max_connections)
            .with_min_connections(self.redis.min_connections)
            .with_connect_timeout(self.redis.connect_timeout_seconds)
            .with_wait_timeout(self.redis.wait_timeout_seconds)
            .with_idle_timeout(self.redis.idle_timeout_seconds)
            .with_max_lifetime(self.redis.max_lifetime_seconds)
            .with_test_before_acquire(self.redis.test_before_acquire)
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.app.name.trim().is_empty() {
            bail!("app.name 不能为空");
        }
        self.authorization.validate()?;
        self.bind_addr()?;
        if self.http.max_body_bytes == 0 || self.http.max_body_bytes > 16 * 1024 * 1024 {
            bail!("http.max_body_bytes 必须在 1..=16777216 范围内");
        }
        if self.http.request_timeout_seconds == 0 || self.http.request_timeout_seconds > 300 {
            bail!("http.request_timeout_seconds 必须在 1..=300 范围内");
        }
        if self.http.max_concurrency == 0 || self.http.max_concurrency > 100_000 {
            bail!("http.max_concurrency 必须在 1..=100000 范围内");
        }
        self.mysql_config().validate().context("mysql 配置无效")?;
        self.redis_config().validate().context("redis 配置无效")?;
        if self.token.issuer.trim().is_empty() || self.token.audience.trim().is_empty() {
            bail!("token.issuer 与 token.audience 不能为空");
        }
        if self.token.access_ttl_seconds == 0 || self.token.refresh_ttl_seconds == 0 {
            bail!("Token 有效期必须大于 0 秒");
        }
        if self.token.access_ttl_seconds > MAX_ACCESS_TTL_SECONDS {
            bail!("access token 有效期不能超过 {MAX_ACCESS_TTL_SECONDS} 秒");
        }
        if self.token.refresh_ttl_seconds > MAX_REFRESH_TTL_SECONDS {
            bail!("refresh token 有效期不能超过 {MAX_REFRESH_TTL_SECONDS} 秒");
        }
        if self.token.refresh_ttl_seconds <= self.token.access_ttl_seconds {
            bail!("refresh token 有效期必须长于 access token");
        }
        self.token.validate()?;
        self.step_up.validate(&self.token)?;
        self.email.validate(&self.token, &self.step_up)?;
        self.security.validate()?;
        if !(1..=300).contains(&self.shutdown.total_timeout_seconds) {
            bail!("shutdown.total_timeout_seconds 必须在 1..=300 范围内");
        }
        self.observability.validate(self.bind_addr()?)?;
        if self.app.environment == DeploymentEnvironment::Production
            && !self.observability.metrics_enabled
        {
            bail!("production 环境必须启用 observability.metrics_enabled 管理面与预算化 readiness");
        }
        Ok(())
    }
}

impl TokenSettings {
    pub fn build_manager(&self) -> anyhow::Result<TokenManager> {
        TokenManager::new_symmetric_keyring(
            self.active_key_id.clone(),
            &self.active_secret,
            self.retiring_keys
                .iter()
                .map(|key| (key.key_id.clone(), key.secret.clone()))
                .collect(),
            jsonwebtoken::Algorithm::HS256,
            self.issuer.clone(),
            self.audience.clone(),
            self.access_ttl_seconds,
            self.refresh_ttl_seconds,
        )
        .context("构建 Token keyring 失败")
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.retiring_keys.len() + 1 > 8 {
            bail!("token keyring 最多允许 8 把密钥");
        }
        let mut key_ids = HashSet::with_capacity(self.retiring_keys.len() + 1);
        validate_token_key_id(&self.active_key_id)?;
        validate_token_secret(&self.active_secret)?;
        key_ids.insert(self.active_key_id.as_str());
        for key in &self.retiring_keys {
            validate_token_key_id(&key.key_id)?;
            validate_token_secret(&key.secret)?;
            if !key_ids.insert(key.key_id.as_str()) {
                bail!("token keyring 的 key_id 必须唯一");
            }
        }
        Ok(())
    }
}

impl StepUpSettings {
    pub fn build_manager(&self) -> anyhow::Result<StepUpManager> {
        StepUpManager::new_with_keyring(
            self.active_key_id.clone(),
            &self.active_secret,
            self.retiring_keys
                .iter()
                .map(|key| (key.key_id.clone(), key.secret.clone())),
            self.issuer.clone(),
            self.audience.clone(),
        )
        .and_then(|manager| {
            manager.with_ttls(
                Duration::from_secs(self.challenge_ttl_seconds),
                Duration::from_secs(self.proof_ttl_seconds),
            )
        })
        .map_err(anyhow::Error::from)
        .context("构建 Step-up keyring 失败")
    }

    fn validate(&self, token: &TokenSettings) -> anyhow::Result<()> {
        if self.retiring_keys.len() > 8 {
            bail!("step_up retiring_keys 最多允许 8 把密钥");
        }
        if self.issuer.trim().is_empty() || self.audience.trim().is_empty() {
            bail!("step_up.issuer 与 step_up.audience 不能为空");
        }
        if !(1..=300).contains(&self.challenge_ttl_seconds) {
            bail!("step_up.challenge_ttl_seconds 必须在 1..=300 范围内");
        }
        if !(1..=600).contains(&self.proof_ttl_seconds) {
            bail!("step_up.proof_ttl_seconds 必须在 1..=600 范围内");
        }

        let mut key_ids = HashSet::with_capacity(self.retiring_keys.len() + 1);
        let mut secrets = HashSet::with_capacity(self.retiring_keys.len() + 1);
        validate_step_up_key_id(&self.active_key_id)?;
        validate_step_up_secret(&self.active_secret)?;
        key_ids.insert(self.active_key_id.as_str());
        secrets.insert(self.active_secret.as_str());
        for key in &self.retiring_keys {
            validate_step_up_key_id(&key.key_id)?;
            validate_step_up_secret(&key.secret)?;
            if !key_ids.insert(key.key_id.as_str()) {
                bail!("step_up keyring 的 key_id 必须唯一");
            }
            if !secrets.insert(key.secret.as_str()) {
                bail!("step_up active/retiring keys 不得复用同一密钥");
            }
        }

        let token_secrets = std::iter::once(token.active_secret.as_str())
            .chain(token.retiring_keys.iter().map(|key| key.secret.as_str()));
        if token_secrets
            .into_iter()
            .any(|secret| secrets.contains(secret))
        {
            bail!("step_up 与 Access/Refresh Token 必须使用不同密钥");
        }
        self.build_manager()?;
        Ok(())
    }
}

impl EmailSettings {
    fn validate(&self, token: &TokenSettings, step_up: &StepUpSettings) -> anyhow::Result<()> {
        self.smtp.validate()?;
        self.verification.validate()?;
        let secret = self.verification.secret.as_str();
        let collides_with_token = std::iter::once(token.active_secret.as_str())
            .chain(token.retiring_keys.iter().map(|key| key.secret.as_str()))
            .any(|candidate| candidate == secret);
        let collides_with_step_up = std::iter::once(step_up.active_secret.as_str())
            .chain(step_up.retiring_keys.iter().map(|key| key.secret.as_str()))
            .any(|candidate| candidate == secret);
        if collides_with_token || collides_with_step_up {
            bail!("email.verification.secret 不得复用 Token 或 Step-up 密钥");
        }
        Ok(())
    }
}

impl SmtpSettings {
    fn validate(&self) -> anyhow::Result<()> {
        let relay = self.relay.trim();
        if relay.is_empty()
            || relay.len() > 253
            || relay.bytes().any(|byte| byte.is_ascii_whitespace())
            || relay.contains('/')
            || relay.contains(':')
        {
            bail!("email.smtp.relay 必须是无 scheme、端口或路径的 SMTP 主机名");
        }
        if self.port == 0 {
            bail!("email.smtp.port 必须大于 0");
        }
        let username_empty = self.username.trim().is_empty();
        let password_empty = self.password.is_empty();
        if username_empty != password_empty {
            bail!("email.smtp.username 与 password 必须同时配置或同时留空");
        }
        if matches!(
            self.username.trim().to_ascii_lowercase().as_str(),
            "replace-with-smtp-username" | "changeme"
        ) || matches!(
            self.password.trim().to_ascii_lowercase().as_str(),
            "replace-with-smtp-password" | "changeme"
        ) {
            bail!("email.smtp 凭据不能使用示例占位值");
        }
        if self.from_name.trim().is_empty() || self.from_name.chars().count() > 100 {
            bail!("email.smtp.from_name 必须是 1..=100 个字符");
        }
        self.from_address
            .parse::<lettre::Address>()
            .map_err(|_| anyhow::anyhow!("email.smtp.from_address 不是合法邮箱地址"))?;
        if !(1..=30).contains(&self.timeout_seconds) {
            bail!("email.smtp.timeout_seconds 必须在 1..=30 范围内");
        }
        Ok(())
    }
}

impl EmailVerificationSettings {
    fn validate(&self) -> anyhow::Result<()> {
        crate::authorization::validate_deployment_name(&self.namespace)
            .context("email.verification.namespace 无效")?;
        validate_token_secret(&self.secret).context("email.verification.secret 无效")?;
        if !(60..=1_800).contains(&self.ttl_seconds) {
            bail!("email.verification.ttl_seconds 必须在 60..=1800 范围内");
        }
        if self.resend_cooldown_seconds == 0 || self.resend_cooldown_seconds > self.ttl_seconds {
            bail!("email.verification.resend_cooldown_seconds 必须在 1..=ttl_seconds 范围内");
        }
        if !(1..=10).contains(&self.max_attempts) {
            bail!("email.verification.max_attempts 必须在 1..=10 范围内");
        }
        if !(60..=3_600).contains(&self.send_window_seconds) {
            bail!("email.verification.send_window_seconds 必须在 60..=3600 范围内");
        }
        for (name, value) in [
            ("send_ip_attempts", self.send_ip_attempts),
            ("send_email_attempts", self.send_email_attempts),
            ("send_global_attempts", self.send_global_attempts),
        ] {
            if value == 0 || value > 1_000_000 {
                bail!("email.verification.{name} 必须在 1..=1000000 范围内");
            }
        }
        if self.send_global_attempts < self.send_ip_attempts
            || self.send_global_attempts < self.send_email_attempts
        {
            bail!("email.verification.send_global_attempts 不得小于单 IP 或单邮箱额度");
        }
        Ok(())
    }
}

impl MysqlSettings {
    fn database_config(&self) -> DatabaseConfig {
        DatabaseConfig::default()
            .with_max_connections(self.max_connections)
            .with_min_connections(self.min_connections)
            .with_connect_timeout(self.connect_timeout_seconds)
            .with_idle_timeout(self.idle_timeout_seconds)
            .with_max_lifetime(self.max_lifetime_seconds)
            .with_test_before_acquire(self.test_before_acquire)
    }
}

impl SecuritySettings {
    fn validate(&self) -> anyhow::Result<()> {
        if self.argon2_max_concurrency == 0 {
            bail!("security.argon2_max_concurrency 必须大于 0");
        }
        validate_rate_limit("window_seconds", self.auth_rate_limit_window_seconds)?;
        validate_rate_limit("ip_attempts", self.auth_rate_limit_ip_attempts)?;
        validate_rate_limit("username_attempts", self.auth_rate_limit_username_attempts)?;
        if !(60..=3_600).contains(&self.password_reset_ttl_seconds) {
            bail!("security.password_reset_ttl_seconds 必须在 60..=3600 范围内");
        }
        yang_base::transport::client_ip::validate_trusted_proxy_cidrs(&self.trusted_proxy_cidrs)
            .map_err(|error| anyhow::anyhow!("security.trusted_proxy_cidrs 配置无效: {error}"))?;
        Ok(())
    }
}

const fn default_password_reset_ttl_seconds() -> u64 {
    900
}

impl AuthorizationSettings {
    fn validate(&self) -> anyhow::Result<()> {
        crate::authorization::validate_deployment_name(&self.deployment)?;
        if !(10..=250).contains(&self.outbox_poll_interval_ms) {
            bail!("authorization.outbox_poll_interval_ms 必须在 10..=250 范围内");
        }
        if !(1..=1_000).contains(&self.outbox_batch_size) {
            bail!("authorization.outbox_batch_size 必须在 1..=1000 范围内");
        }
        if !(1..=300).contains(&self.outbox_lease_seconds) {
            bail!("authorization.outbox_lease_seconds 必须在 1..=300 范围内");
        }
        if !(1..=300).contains(&self.outbox_max_retry_seconds) {
            bail!("authorization.outbox_max_retry_seconds 必须在 1..=300 范围内");
        }
        Ok(())
    }
}

fn validate_rate_limit(name: &str, value: u64) -> anyhow::Result<()> {
    if value == 0 || value > 86_400 {
        bail!("security.auth_rate_limit_{name} 必须在 1..=86400 范围内");
    }
    Ok(())
}

fn validate_token_secret(secret: &str) -> anyhow::Result<()> {
    if secret.len() < 32 {
        bail!("token key secret 至少需要 32 字节");
    }
    let normalized = secret.trim().to_ascii_lowercase();
    let known_placeholder = matches!(
        normalized.as_str(),
        "changeme"
            | "replace-me"
            | "replace_with_a_random_secret"
            | "replace-with-at-least-32-random-bytes"
            | "replace-with-independent-email-verification-secret"
            | "example-secret"
    );
    let repeated_byte = secret
        .as_bytes()
        .first()
        .is_some_and(|first| secret.as_bytes().iter().all(|byte| byte == first));
    if known_placeholder || repeated_byte {
        bail!("token key secret 不能使用示例值、占位值或重复字符");
    }
    Ok(())
}

fn validate_token_key_id(key_id: &str) -> anyhow::Result<()> {
    if key_id.is_empty()
        || key_id.len() > 64
        || !key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!("token key_id 必须是 1..=64 字节的 ASCII 字母、数字、点、下划线或连字符");
    }
    Ok(())
}

fn validate_step_up_secret(secret: &str) -> anyhow::Result<()> {
    validate_token_secret(secret).context("step_up key secret 无效")
}

fn validate_step_up_key_id(key_id: &str) -> anyhow::Result<()> {
    if key_id.is_empty()
        || key_id.len() > 64
        || !key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        bail!("step_up key_id 必须是 1..=64 字节的 ASCII 字母、数字、下划线或连字符");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::source::{SecretKey, SecretProvider};
    use std::collections::BTreeMap;

    fn valid_config() -> &'static str {
        r#"
[app]
name = "test"
environment = "development"
[authorization]
deployment = "test-local"
outbox_poll_interval_ms = 250
outbox_batch_size = 100
outbox_lease_seconds = 10
outbox_max_retry_seconds = 60
[http]
bind = "127.0.0.1:8080"
max_body_bytes = 1024
request_timeout_seconds = 30
max_concurrency = 256
[mysql]
url = "mysql://config-user:config-password@config-mysql/config-database"
max_connections = 2
min_connections = 0
connect_timeout_seconds = 2
idle_timeout_seconds = 30
max_lifetime_seconds = 60
test_before_acquire = false
[redis]
url = "redis://config-redis/3"
max_connections = 2
min_connections = 0
connect_timeout_seconds = 2
wait_timeout_seconds = 3
idle_timeout_seconds = 30
max_lifetime_seconds = 60
test_before_acquire = false
[token]
active_key_id = "test-2026-07"
active_secret = "0123456789abcdef0123456789abcdef"
retiring_keys = []
issuer = "test"
audience = "test-api"
access_ttl_seconds = 60
refresh_ttl_seconds = 120
[step_up]
active_key_id = "step-up-test-2026-07"
active_secret = "step-up-0123456789abcdef0123456789abcdef"
retiring_keys = []
issuer = "test-step-up"
audience = "test-sensitive-actions"
challenge_ttl_seconds = 120
proof_ttl_seconds = 300
[email.smtp]
relay = "smtp.example.test"
port = 587
username = "test-smtp-user"
password = "test-smtp-password"
from_address = "no-reply@example.test"
from_name = "YANG Test"
timeout_seconds = 5
[email.verification]
namespace = "test-local"
secret = "email-verification-0123456789abcdef0123456789abcdef"
ttl_seconds = 600
resend_cooldown_seconds = 60
max_attempts = 5
send_window_seconds = 3600
send_ip_attempts = 20
send_email_attempts = 5
send_global_attempts = 1000
[security]
argon2_max_concurrency = 4
auth_rate_limit_window_seconds = 60
auth_rate_limit_ip_attempts = 30
auth_rate_limit_username_attempts = 10
issue_refresh_credential_version = false
[shutdown]
total_timeout_seconds = 30
[observability]
metrics_enabled = false
metrics_bind = "127.0.0.1:9090"
traces_enabled = false
traces_otlp_endpoint = "http://127.0.0.1:4317"
traces_sample_ratio = 0.1
traces_export_timeout_seconds = 5
readiness_budget_ms = 2000
[logging]
filter = "info"
"#
    }

    #[test]
    fn parses_values_from_config_file_and_redacts_token_debug() {
        let settings = Settings::parse(valid_config())
            .unwrap_or_else(|error| panic!("有效配置应解析成功: {error}"));

        assert_eq!(
            settings.mysql.url,
            "mysql://config-user:config-password@config-mysql/config-database"
        );
        assert_eq!(settings.redis.url, "redis://config-redis/3");
        assert_eq!(settings.app.environment, DeploymentEnvironment::Development);
        assert_eq!(settings.authorization.deployment, "test-local");
        assert_eq!(settings.authorization.outbox_poll_interval_ms, 250);
        assert_eq!(settings.authorization.outbox_batch_size, 100);
        assert!(settings.security.trusted_proxy_cidrs.is_empty());
        assert!(!settings.security.issue_refresh_credential_version);
        assert_eq!(settings.security.password_reset_ttl_seconds, 900);
        assert_eq!(settings.shutdown.total_timeout_seconds, 30);
        assert!(!settings.observability.metrics_enabled);
        assert!(!settings.observability.traces_enabled);
        assert_eq!(settings.observability.traces_sample_ratio, 0.1);
        assert!(
            !format!("{:?}", settings.token).contains(&settings.token.active_secret),
            "active secret 不得进入 Debug"
        );
        assert!(
            !format!("{:?}", settings.step_up).contains(&settings.step_up.active_secret),
            "step-up active secret 不得进入 Debug"
        );
        assert!(
            !format!("{:?}", settings.email).contains(&settings.email.verification.secret),
            "邮箱验证 secret 不得进入 Debug"
        );
        assert!(
            !format!("{:?}", settings.email).contains(&settings.email.smtp.password),
            "SMTP password 不得进入 Debug"
        );
    }

    #[test]
    fn rejects_password_reset_ttl_outside_the_short_lived_window() {
        let mut too_short = Settings::parse(valid_config())
            .unwrap_or_else(|error| panic!("测试配置应可解析: {error}"));
        too_short.security.password_reset_ttl_seconds = 59;
        let error = match too_short.validate() {
            Ok(()) => panic!("少于 60 秒的重置凭证必须拒绝"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("security.password_reset_ttl_seconds"));

        let mut too_long = Settings::parse(valid_config())
            .unwrap_or_else(|error| panic!("测试配置应可解析: {error}"));
        too_long.security.password_reset_ttl_seconds = 3_601;
        let error = match too_long.validate() {
            Ok(()) => panic!("超过一小时的重置凭证必须拒绝"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("security.password_reset_ttl_seconds"));
    }

    struct TestSecretProvider(BTreeMap<SecretKey, String>);

    impl SecretProvider for TestSecretProvider {
        fn read(&self, key: SecretKey) -> anyhow::Result<Option<String>> {
            Ok(self.0.get(&key).cloned())
        }
    }

    #[test]
    fn resolves_actual_settings_with_explicit_source_precedence() {
        let environment = BTreeMap::from([
            (
                "YANG_SYSTEM_MYSQL_URL".to_owned(),
                "mysql://environment".to_owned(),
            ),
            (
                "YANG_SYSTEM_TOKEN_ACTIVE_SECRET".to_owned(),
                "environment-secret-0123456789abcdef".to_owned(),
            ),
            (
                "YANG_SYSTEM_HTTP_MAX_CONCURRENCY".to_owned(),
                "128".to_owned(),
            ),
            (
                "YANG_SYSTEM_SECURITY_TRUSTED_PROXY_CIDRS".to_owned(),
                "127.0.0.1/32, 10.42.0.0/24".to_owned(),
            ),
            (
                "YANG_SYSTEM_SHUTDOWN_TOTAL_TIMEOUT_SECONDS".to_owned(),
                "45".to_owned(),
            ),
            (
                "YANG_SYSTEM_OBSERVABILITY_TRACES_SAMPLE_RATIO".to_owned(),
                "0.25".to_owned(),
            ),
        ]);
        let provider = TestSecretProvider(BTreeMap::from([
            (
                SecretKey::MysqlUrl,
                "mysql://provider-user:provider-password@provider/database".to_owned(),
            ),
            (
                SecretKey::TokenActiveSecret,
                "provider-secret-0123456789abcdef0123456789abcdef".to_owned(),
            ),
            (
                SecretKey::TokenRetiringKeys,
                r#"[{"key_id":"provider-retiring","secret":"provider-retiring-secret-0123456789abcdef"}]"#
                    .to_owned(),
            ),
        ]));

        let settings: Settings = crate::config::source::parse_with_sources(
            valid_config(),
            &environment,
            Some(&provider),
        )
        .and_then(|settings: Settings| {
            settings.validate()?;
            Ok(settings)
        })
        .unwrap_or_else(|error| panic!("真实 Settings 应按优先级合成并通过校验: {error:#}"));

        assert_eq!(
            settings.mysql.url,
            "mysql://provider-user:provider-password@provider/database"
        );
        assert_eq!(
            settings.token.active_secret,
            "provider-secret-0123456789abcdef0123456789abcdef"
        );
        assert_eq!(settings.token.retiring_keys.len(), 1);
        assert_eq!(settings.token.retiring_keys[0].key_id, "provider-retiring");
        assert_eq!(settings.http.max_concurrency, 128);
        assert_eq!(settings.shutdown.total_timeout_seconds, 45);
        assert_eq!(settings.observability.traces_sample_ratio, 0.25);
        assert_eq!(
            settings.security.trusted_proxy_cidrs,
            ["127.0.0.1/32", "10.42.0.0/24"]
        );
    }

    #[test]
    fn token_keyring_signs_with_active_and_verifies_retiring_key_without_debug_leaks() {
        let retiring_secret = "retiring-secret-0123456789abcdef0123456789abcdef";
        let raw = valid_config().replacen(
            "retiring_keys = []",
            &format!(
                "retiring_keys = [{{ key_id = \"test-2026-06\", secret = \"{retiring_secret}\" }}]"
            ),
            1,
        );
        let settings = Settings::parse(&raw)
            .unwrap_or_else(|error| panic!("合法 Token keyring 应解析成功: {error:#}"));
        let manager = settings
            .token
            .build_manager()
            .unwrap_or_else(|error| panic!("合法 Token keyring 应构建成功: {error:#}"));
        let active_token = manager
            .generate_access_token("7", serde_json::json!({}))
            .unwrap_or_else(|error| panic!("active key 应签发成功: {error}"));
        assert_eq!(
            jsonwebtoken::decode_header(&active_token)
                .unwrap_or_else(|error| panic!("签发 Token Header 应合法: {error}"))
                .kid
                .as_deref(),
            Some("test-2026-07")
        );

        let previous = TokenManager::new_symmetric_keyring(
            "test-2026-06".to_owned(),
            retiring_secret,
            Vec::new(),
            jsonwebtoken::Algorithm::HS256,
            "test".to_owned(),
            "test-api".to_owned(),
            60,
            120,
        )
        .unwrap_or_else(|error| panic!("旧 keyring 应构建成功: {error}"));
        let retiring_token = previous
            .generate_refresh_token("7")
            .unwrap_or_else(|error| panic!("旧 key 应签发测试 Token: {error}"));
        assert!(
            manager.verify_token(&retiring_token).is_ok(),
            "retiring key 应继续验证存量 Token"
        );

        let debug = format!("{:?}", settings.token);
        assert!(!debug.contains(&settings.token.active_secret));
        assert!(!debug.contains(retiring_secret));
    }

    #[test]
    fn step_up_keyring_is_independent_redacted_and_uses_configured_ttls() {
        let settings = Settings::parse(valid_config())
            .unwrap_or_else(|error| panic!("合法 Step-up 配置应解析成功: {error:#}"));
        let manager = settings
            .step_up
            .build_manager()
            .unwrap_or_else(|error| panic!("合法 Step-up keyring 应构建成功: {error:#}"));
        let challenge = manager
            .issue_challenge(
                "7",
                &yang_base::action!("admin.user.set_admin"),
                "admin_user:42:admin=true",
            )
            .unwrap_or_else(|error| panic!("Step-up challenge 应签发成功: {error}"));

        assert_eq!(challenge.expires_in, 120);
        assert_eq!(
            jsonwebtoken::decode_header(&challenge.challenge)
                .unwrap_or_else(|error| panic!("challenge header 应可解码: {error}"))
                .kid
                .as_deref(),
            Some("step-up-test-2026-07")
        );
        let debug = format!("{:?}", settings.step_up);
        assert!(!debug.contains(&settings.step_up.active_secret));
    }

    #[test]
    fn rejects_step_up_key_reuse_duplicates_and_ttl_overflow() {
        for (raw, expected) in [
            (
                valid_config().replace(
                    "step-up-0123456789abcdef0123456789abcdef",
                    "0123456789abcdef0123456789abcdef",
                ),
                "必须使用不同密钥",
            ),
            (
                valid_config().replace(
                    "retiring_keys = []\nissuer = \"test-step-up\"",
                    "retiring_keys = [{ key_id = \"step-up-test-2026-07\", secret = \"another-step-up-secret-0123456789abcdef\" }]\nissuer = \"test-step-up\"",
                ),
                "key_id 必须唯一",
            ),
            (
                valid_config().replace("challenge_ttl_seconds = 120", "challenge_ttl_seconds = 301"),
                "challenge_ttl_seconds",
            ),
            (
                valid_config().replace("proof_ttl_seconds = 300", "proof_ttl_seconds = 601"),
                "proof_ttl_seconds",
            ),
        ] {
            let error = Settings::parse(&raw)
                .err()
                .unwrap_or_else(|| panic!("非法 Step-up 配置必须被拒绝: {expected}"));
            assert!(
                format!("{error:#}").contains(expected),
                "错误应指出 {expected}: {error:#}"
            );
        }
    }

    #[test]
    fn rejects_invalid_or_duplicate_token_key_ids() {
        for raw in [
            valid_config().replace("test-2026-07", "invalid key id"),
            valid_config().replace(
                "retiring_keys = []",
                "retiring_keys = [{ key_id = \"test-2026-07\", secret = \"retiring-secret-0123456789abcdef0123456789abcdef\" }]",
            ),
        ] {
            let error = Settings::parse(&raw)
                .err()
                .unwrap_or_else(|| panic!("非法或重复 key_id 必须被拒绝"));
            assert!(
                format!("{error:#}").contains("key_id"),
                "错误必须定位 key_id: {error:#}"
            );
        }
    }

    #[test]
    fn deployment_environment_defaults_to_production() {
        let raw = valid_config()
            .replace("environment = \"development\"\n", "")
            .replace("metrics_enabled = false", "metrics_enabled = true");
        let settings = Settings::parse(&raw)
            .unwrap_or_else(|error| panic!("缺省部署环境应采用安全默认值: {error}"));
        assert_eq!(settings.app.environment, DeploymentEnvironment::Production);
    }

    #[test]
    fn shutdown_budget_defaults_safely_and_rejects_out_of_range_values() {
        let without_section =
            valid_config().replace("[shutdown]\ntotal_timeout_seconds = 30\n", "");
        let settings = Settings::parse(&without_section)
            .unwrap_or_else(|error| panic!("缺省关闭预算应使用安全默认值: {error}"));
        assert_eq!(settings.shutdown.total_timeout_seconds, 30);

        for invalid in [0, 301] {
            let raw = valid_config().replace(
                "total_timeout_seconds = 30",
                &format!("total_timeout_seconds = {invalid}"),
            );
            let error = Settings::parse(&raw)
                .err()
                .unwrap_or_else(|| panic!("越界关闭预算 {invalid} 必须被拒绝"));
            assert!(
                error.to_string().contains("shutdown.total_timeout_seconds"),
                "错误必须定位关闭预算字段: {error:#}"
            );
        }
    }

    #[test]
    fn observability_defaults_are_disabled_and_validation_is_fail_fast() {
        let without_section = valid_config().replace(
            "[observability]\nmetrics_enabled = false\nmetrics_bind = \"127.0.0.1:9090\"\ntraces_enabled = false\ntraces_otlp_endpoint = \"http://127.0.0.1:4317\"\ntraces_sample_ratio = 0.1\ntraces_export_timeout_seconds = 5\nreadiness_budget_ms = 2000\n",
            "",
        );
        let settings = Settings::parse(&without_section)
            .unwrap_or_else(|error| panic!("缺省可观测性配置应安全关闭: {error:#}"));
        assert!(!settings.observability.metrics_enabled);
        assert!(!settings.observability.traces_enabled);

        for raw in [
            valid_config()
                .replace("metrics_enabled = false", "metrics_enabled = true")
                .replace(
                    "metrics_bind = \"127.0.0.1:9090\"",
                    "metrics_bind = \"127.0.0.1:8080\"",
                ),
            valid_config().replace("traces_sample_ratio = 0.1", "traces_sample_ratio = 1.1"),
            valid_config().replace(
                "traces_export_timeout_seconds = 5",
                "traces_export_timeout_seconds = 0",
            ),
            valid_config().replace("readiness_budget_ms = 2000", "readiness_budget_ms = 49"),
            valid_config()
                .replace("traces_enabled = false", "traces_enabled = true")
                .replace(
                    "traces_otlp_endpoint = \"http://127.0.0.1:4317\"",
                    "traces_otlp_endpoint = \"collector:4317\"",
                ),
        ] {
            assert!(
                Settings::parse(&raw).is_err(),
                "非法可观测性配置必须在启动前失败"
            );
        }
    }

    #[test]
    fn production_requires_the_budgeted_management_probe() {
        let disabled = valid_config().replace(
            "environment = \"development\"",
            "environment = \"production\"",
        );
        let error = Settings::parse(&disabled)
            .err()
            .unwrap_or_else(|| panic!("production 不得在无管理面 readiness 时启动"));
        assert!(error.to_string().contains("metrics_enabled"));

        let enabled = disabled.replace("metrics_enabled = false", "metrics_enabled = true");
        Settings::parse(&enabled)
            .unwrap_or_else(|error| panic!("启用管理面后 production 配置应通过: {error:#}"));
    }

    #[test]
    fn rejects_unknown_deployment_environment() {
        let raw =
            valid_config().replace("environment = \"development\"", "environment = \"staging\"");
        let error = match Settings::parse(&raw) {
            Ok(_) => panic!("未知部署环境必须被拒绝"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("解析配置文件失败"));
    }

    #[test]
    fn example_config_has_no_schema_mode() {
        let value: toml::Value = toml::from_str(include_str!("../../config.example.toml"))
            .unwrap_or_else(|error| panic!("示例配置必须是合法 TOML: {error}"));
        assert!(value.get("schema").is_none());
        assert_eq!(
            value
                .get("app")
                .and_then(|app| app.get("environment"))
                .and_then(toml::Value::as_str),
            Some("development")
        );
        assert_eq!(
            value
                .get("security")
                .and_then(|security| security.get("trusted_proxy_cidrs"))
                .and_then(toml::Value::as_array)
                .map(Vec::len),
            Some(0)
        );
        assert_eq!(
            value
                .get("shutdown")
                .and_then(|shutdown| shutdown.get("total_timeout_seconds"))
                .and_then(toml::Value::as_integer),
            Some(30)
        );
        assert_eq!(
            value
                .get("observability")
                .and_then(|observability| observability.get("metrics_enabled"))
                .and_then(toml::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            value
                .get("observability")
                .and_then(|observability| observability.get("traces_enabled"))
                .and_then(toml::Value::as_bool),
            Some(false)
        );
    }

    #[test]
    fn rejects_removed_schema_mode_from_config_file() {
        let raw = valid_config().replace("[http]", "[schema]\nmode = \"off\"\n[http]");
        let error = match Settings::parse(&raw) {
            Ok(_) => panic!("已删除的 schema mode 必须被拒绝，不能绕过启动同步"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("解析配置文件失败"));
    }

    #[test]
    fn rejects_placeholder_or_repeated_token_secret() {
        let repeated = valid_config().replace("0123456789abcdef0123456789abcdef", &"1".repeat(32));
        let error = match Settings::parse(&repeated) {
            Ok(_) => panic!("重复字符密钥必须被拒绝"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("重复字符"));
    }

    #[test]
    fn rejects_placeholder_token_secret_from_config_file() {
        let raw = valid_config().replace(
            "0123456789abcdef0123456789abcdef",
            "replace-with-at-least-32-random-bytes",
        );
        let error = match Settings::parse(&raw) {
            Ok(_) => panic!("配置文件中的占位密钥必须被拒绝"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("占位值"));
    }

    #[test]
    fn rejects_unbounded_http_resource_settings() {
        let raw = valid_config().replace("max_concurrency = 256", "max_concurrency = 0");
        let error = match Settings::parse(&raw) {
            Ok(_) => panic!("HTTP 并发上限为 0 时必须被拒绝"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("max_concurrency"));
    }

    #[test]
    fn validates_trusted_proxy_cidrs_and_keeps_empty_as_safe_default() {
        let valid = valid_config().replace(
            "auth_rate_limit_username_attempts = 10",
            "auth_rate_limit_username_attempts = 10\ntrusted_proxy_cidrs = [\"127.0.0.1/32\", \"10.42.0.0/24\"]",
        );
        let settings = Settings::parse(&valid)
            .unwrap_or_else(|error| panic!("合法代理 CIDR 应通过配置校验: {error:#}"));
        assert_eq!(
            settings.security.trusted_proxy_cidrs,
            ["127.0.0.1/32", "10.42.0.0/24"]
        );

        for cidr in ["0.0.0.0/0", "::/0", "10.0.0.1", "10.0.0.0/33"] {
            let raw = valid_config().replace(
                "auth_rate_limit_username_attempts = 10",
                &format!(
                    "auth_rate_limit_username_attempts = 10\ntrusted_proxy_cidrs = [\"{cidr}\"]"
                ),
            );
            let error = match Settings::parse(&raw) {
                Ok(_) => panic!("不安全或非法代理 CIDR 必须在启动前被拒绝: {cidr}"),
                Err(error) => error,
            };
            assert!(
                format!("{error:#}").contains("trusted_proxy_cidrs"),
                "错误必须定位代理 CIDR 配置: {error:#}"
            );
        }
    }

    #[test]
    fn rejects_unsafe_authorization_worker_settings() {
        for raw in [
            valid_config().replace("deployment = \"test-local\"", "deployment = \"INVALID\""),
            valid_config().replace(
                "outbox_poll_interval_ms = 250",
                "outbox_poll_interval_ms = 251",
            ),
            valid_config().replace("outbox_batch_size = 100", "outbox_batch_size = 0"),
            valid_config().replace("outbox_lease_seconds = 10", "outbox_lease_seconds = 0"),
            valid_config().replace(
                "outbox_max_retry_seconds = 60",
                "outbox_max_retry_seconds = 301",
            ),
        ] {
            assert!(
                Settings::parse(&raw).is_err(),
                "不安全的授权传播配置必须在启动前被拒绝"
            );
        }
    }
}

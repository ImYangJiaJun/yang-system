use crate::bootstrap_secret::BootstrapSecretDigest;
use anyhow::{bail, Context};
use serde::Deserialize;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::Path;
use yang_base::token::TokenManager;
use yang_db::{DatabaseConfig, RedisConfig};

const MAX_ACCESS_TTL_SECONDS: u64 = 24 * 60 * 60;
const MAX_REFRESH_TTL_SECONDS: u64 = 90 * 24 * 60 * 60;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub app: AppSettings,
    pub authorization: AuthorizationSettings,
    #[serde(default)]
    pub schema: SchemaSettings,
    pub http: HttpSettings,
    pub mysql: MysqlSettings,
    pub redis: RedisSettings,
    pub token: TokenSettings,
    pub bootstrap: BootstrapSettings,
    pub security: SecuritySettings,
    #[serde(default)]
    pub shutdown: ShutdownSettings,
    pub logging: LoggingSettings,
}

/// 独立迁移作业所需的最小配置投影。
///
/// 迁移不依赖 HTTP、Redis 或 Token；只读取构建应用 Schema 所需的 MySQL 与安全参数。
#[derive(Clone, Deserialize)]
pub struct MigrationSettings {
    pub mysql: MysqlSettings,
    pub security: SecuritySettings,
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

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaSettings {
    #[serde(default)]
    pub mode: SchemaMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaMode {
    Apply,
    #[default]
    Validate,
    Off,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapSettings {
    pub secret_digest: BootstrapSecretDigest,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecuritySettings {
    pub argon2_max_concurrency: usize,
    pub auth_rate_limit_window_seconds: u64,
    pub auth_rate_limit_ip_attempts: u64,
    pub auth_rate_limit_username_attempts: u64,
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
        let settings: Self = crate::config_source::load(path, "读取配置文件失败")?;
        settings.validate()?;
        Ok(settings)
    }

    #[cfg(test)]
    fn parse(raw: &str) -> anyhow::Result<Self> {
        let settings: Self = crate::config_source::parse_file_only(raw)?;
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
        if self.schema.mode == SchemaMode::Apply
            && !matches!(
                self.app.environment,
                DeploymentEnvironment::Development | DeploymentEnvironment::Test
            )
        {
            bail!("production 环境禁止 schema.mode=apply；请使用独立迁移作业并保持 validate");
        }
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
        self.security.validate()?;
        if !(1..=300).contains(&self.shutdown.total_timeout_seconds) {
            bail!("shutdown.total_timeout_seconds 必须在 1..=300 范围内");
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

impl MigrationSettings {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let settings: Self = crate::config_source::load(path, "读取迁移配置文件失败")?;
        settings
            .mysql_config()
            .validate()
            .context("mysql 配置无效")?;
        settings.security.validate()?;
        Ok(settings)
    }

    pub fn mysql_config(&self) -> DatabaseConfig {
        self.mysql.database_config()
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
        crate::security::validate_trusted_proxy_cidrs(&self.trusted_proxy_cidrs)?;
        Ok(())
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_source::{SecretKey, SecretProvider};
    use std::collections::BTreeMap;

    const VALID_BOOTSTRAP_DIGEST: &str = concat!(
        "$argon2id$v=19$m=19456,t=2,p=1$",
        "MDEyMzQ1Njc4OWFiY2RlZg$",
        "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY"
    );

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
[schema]
mode = "validate"
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
[bootstrap]
secret_digest = "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY"
[security]
argon2_max_concurrency = 4
auth_rate_limit_window_seconds = 60
auth_rate_limit_ip_attempts = 30
auth_rate_limit_username_attempts = 10
[shutdown]
total_timeout_seconds = 30
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
        assert_eq!(settings.schema.mode, SchemaMode::Validate);
        assert_eq!(settings.app.environment, DeploymentEnvironment::Development);
        assert_eq!(settings.authorization.deployment, "test-local");
        assert_eq!(settings.authorization.outbox_poll_interval_ms, 250);
        assert_eq!(settings.authorization.outbox_batch_size, 100);
        assert!(settings.security.trusted_proxy_cidrs.is_empty());
        assert_eq!(settings.shutdown.total_timeout_seconds, 30);
        assert!(
            !format!("{:?}", settings.token).contains(&settings.token.active_secret),
            "active secret 不得进入 Debug"
        );
        assert_eq!(
            settings.bootstrap.secret_digest.as_str(),
            VALID_BOOTSTRAP_DIGEST
        );
        assert!(
            !format!("{:?}", settings.bootstrap.secret_digest).contains(VALID_BOOTSTRAP_DIGEST),
            "bootstrap 摘要不得进入 Debug"
        );
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
                "YANG_SYSTEM_BOOTSTRAP_SECRET_DIGEST".to_owned(),
                "invalid-environment-digest".to_owned(),
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
            (
                SecretKey::BootstrapSecretDigest,
                VALID_BOOTSTRAP_DIGEST.to_owned(),
            ),
        ]));

        let settings: Settings =
            crate::config_source::parse_with_sources(valid_config(), &environment, Some(&provider))
                .and_then(|settings: Settings| {
                    settings.validate()?;
                    Ok(settings)
                })
                .unwrap_or_else(|error| {
                    panic!("真实 Settings 应按优先级合成并通过校验: {error:#}")
                });

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
        assert_eq!(
            settings.bootstrap.secret_digest.as_str(),
            VALID_BOOTSTRAP_DIGEST
        );
        assert_eq!(settings.http.max_concurrency, 128);
        assert_eq!(settings.shutdown.total_timeout_seconds, 45);
        assert_eq!(
            settings.security.trusted_proxy_cidrs,
            ["127.0.0.1/32", "10.42.0.0/24"]
        );
    }

    #[test]
    fn token_keyring_signs_with_active_and_verifies_retiring_key_without_debug_leaks() {
        let retiring_secret = "retiring-secret-0123456789abcdef0123456789abcdef";
        let raw = valid_config().replace(
            "retiring_keys = []",
            &format!(
                "retiring_keys = [{{ key_id = \"test-2026-06\", secret = \"{retiring_secret}\" }}]"
            ),
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
    fn defaults_schema_mode_to_validate_when_section_or_mode_is_omitted() {
        let without_section = valid_config().replace("[schema]\nmode = \"validate\"\n", "");
        let without_mode = valid_config().replace("mode = \"validate\"\n", "");

        for raw in [without_section, without_mode] {
            let settings = Settings::parse(&raw)
                .unwrap_or_else(|error| panic!("缺省 schema 配置应使用安全默认值: {error}"));
            assert_eq!(settings.schema.mode, SchemaMode::Validate);
        }
    }

    #[test]
    fn accepts_apply_and_off_only_when_explicitly_selected() {
        for (raw_mode, expected) in [("apply", SchemaMode::Apply), ("off", SchemaMode::Off)] {
            let raw =
                valid_config().replace("mode = \"validate\"", &format!("mode = \"{raw_mode}\""));
            let settings = Settings::parse(&raw)
                .unwrap_or_else(|error| panic!("显式 schema mode 应解析成功: {error}"));
            assert_eq!(settings.schema.mode, expected);
        }
    }

    #[test]
    fn allows_schema_apply_in_test_environment() {
        let raw = valid_config()
            .replace("environment = \"development\"", "environment = \"test\"")
            .replace("mode = \"validate\"", "mode = \"apply\"");
        let settings = Settings::parse(&raw)
            .unwrap_or_else(|error| panic!("测试环境应允许显式 schema apply: {error}"));
        assert_eq!(settings.app.environment, DeploymentEnvironment::Test);
        assert_eq!(settings.schema.mode, SchemaMode::Apply);
    }

    #[test]
    fn deployment_environment_defaults_to_production() {
        let raw = valid_config().replace("environment = \"development\"\n", "");
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
    fn rejects_schema_apply_in_production_or_without_environment_marker() {
        let explicit_production = valid_config()
            .replace(
                "environment = \"development\"",
                "environment = \"production\"",
            )
            .replace("mode = \"validate\"", "mode = \"apply\"");
        let implicit_production = valid_config()
            .replace("environment = \"development\"\n", "")
            .replace("mode = \"validate\"", "mode = \"apply\"");

        for raw in [explicit_production, implicit_production] {
            let error = match Settings::parse(&raw) {
                Ok(_) => panic!("生产环境必须拒绝 schema apply"),
                Err(error) => error,
            };
            assert!(
                error
                    .to_string()
                    .contains("production 环境禁止 schema.mode=apply"),
                "应返回明确的生产 DDL 保护错误: {error}"
            );
        }
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
    fn example_config_uses_validate_schema_mode() {
        let value: toml::Value = toml::from_str(include_str!("../config.example.toml"))
            .unwrap_or_else(|error| panic!("示例配置必须是合法 TOML: {error}"));
        assert_eq!(
            value
                .get("schema")
                .and_then(|schema| schema.get("mode"))
                .and_then(toml::Value::as_str),
            Some("validate")
        );
        assert_eq!(
            value
                .get("app")
                .and_then(|app| app.get("environment"))
                .and_then(toml::Value::as_str),
            Some("development")
        );
        assert_eq!(
            value
                .get("bootstrap")
                .and_then(|bootstrap| bootstrap.get("secret_digest"))
                .and_then(toml::Value::as_str),
            Some("replace-with-yang-bootstrap-secret-digest")
        );
        assert!(
            value
                .get("bootstrap")
                .and_then(|bootstrap| bootstrap.get("secret"))
                .is_none(),
            "示例配置不得保存原始 bootstrap secret"
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
    }

    #[test]
    fn rejects_unknown_schema_mode_from_config_file() {
        let raw = valid_config().replace("mode = \"validate\"", "mode = \"unsafe\"");
        let error = match Settings::parse(&raw) {
            Ok(_) => panic!("未知 schema mode 必须被拒绝"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("解析配置文件失败"));
    }

    #[test]
    fn rejects_unknown_schema_field_when_safe_defaults_are_enabled() {
        let raw = valid_config().replace(
            "mode = \"validate\"",
            "mode = \"validate\"\nunexpected = true",
        );
        let error = match Settings::parse(&raw) {
            Ok(_) => panic!("未知 schema 字段必须被拒绝"),
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
    fn rejects_missing_weak_or_invalid_bootstrap_digest() {
        let section = format!("[bootstrap]\nsecret_digest = \"{VALID_BOOTSTRAP_DIGEST}\"\n");
        let missing = valid_config().replace(&section, "");
        let invalid = [
            "operator-raw-secret-must-not-be-stored",
            "$argon2i$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY",
            "$argon2id$v=19$m=8192,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY",
            "$argon2id$v=19$m=19456,t=1,p=1$MDEyMzQ1Njc4OWFiY2RlZg$MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY",
            "$argon2id$v=19$m=19456,t=2,p=1$short$short",
        ]
        .map(|digest| valid_config().replace(VALID_BOOTSTRAP_DIGEST, digest));

        for raw in std::iter::once(missing).chain(invalid) {
            let error = match Settings::parse(&raw) {
                Ok(_) => panic!("缺失、弱或非法 bootstrap 摘要必须在启动前被拒绝"),
                Err(error) => error,
            };
            assert!(
                format!("{error:#}").contains("bootstrap"),
                "错误必须定位 bootstrap 配置: {error:#}"
            );
        }
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

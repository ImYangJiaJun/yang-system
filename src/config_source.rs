//! 启动期配置源合成：配置文件 < 环境变量 < 目录型 secret provider。
//!
//! 本模块只在进程启动时工作。合成完成后，业务代码仍只消费不可变的强类型
//! `Settings`，不会形成第二套动态配置运行时。

use serde::de::DeserializeOwned;
#[cfg(test)]
use std::collections::BTreeMap;
#[cfg(test)]
use std::ffi::OsString;
#[cfg(test)]
use std::fs::File;
#[cfg(test)]
use std::io::Read;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use yang_runtime::config::ConfigSourceError;
use yang_runtime::config::{
    ConfigSources, EnvironmentBinding, EnvironmentValueKind, SecretBinding,
};

const SECRET_DIRECTORY_ENV: &str = "YANG_SYSTEM_SECRET_DIR";
#[cfg(test)]
const MAX_SECRET_BYTES: u64 = 64 * 1024;

macro_rules! environment_binding {
    ($variable:literal, $section:literal, $field:literal, $kind:ident) => {
        EnvironmentBinding::new($variable, $section, $field, EnvironmentValueKind::$kind)
    };
}

const ENVIRONMENT_BINDINGS: &[EnvironmentBinding] = &[
    environment_binding!("YANG_SYSTEM_APP_NAME", "app", "name", Text),
    environment_binding!("YANG_SYSTEM_APP_ENVIRONMENT", "app", "environment", Text),
    environment_binding!(
        "YANG_SYSTEM_AUTHORIZATION_DEPLOYMENT",
        "authorization",
        "deployment",
        Text
    ),
    environment_binding!(
        "YANG_SYSTEM_AUTHORIZATION_OUTBOX_POLL_INTERVAL_MS",
        "authorization",
        "outbox_poll_interval_ms",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_AUTHORIZATION_OUTBOX_BATCH_SIZE",
        "authorization",
        "outbox_batch_size",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_AUTHORIZATION_OUTBOX_LEASE_SECONDS",
        "authorization",
        "outbox_lease_seconds",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_AUTHORIZATION_OUTBOX_MAX_RETRY_SECONDS",
        "authorization",
        "outbox_max_retry_seconds",
        Integer
    ),
    environment_binding!("YANG_SYSTEM_HTTP_BIND", "http", "bind", Text),
    environment_binding!(
        "YANG_SYSTEM_HTTP_MAX_BODY_BYTES",
        "http",
        "max_body_bytes",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_HTTP_REQUEST_TIMEOUT_SECONDS",
        "http",
        "request_timeout_seconds",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_HTTP_MAX_CONCURRENCY",
        "http",
        "max_concurrency",
        Integer
    ),
    environment_binding!("YANG_SYSTEM_MYSQL_URL", "mysql", "url", Text),
    environment_binding!(
        "YANG_SYSTEM_MYSQL_MAX_CONNECTIONS",
        "mysql",
        "max_connections",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_MYSQL_MIN_CONNECTIONS",
        "mysql",
        "min_connections",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_MYSQL_CONNECT_TIMEOUT_SECONDS",
        "mysql",
        "connect_timeout_seconds",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_MYSQL_IDLE_TIMEOUT_SECONDS",
        "mysql",
        "idle_timeout_seconds",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_MYSQL_MAX_LIFETIME_SECONDS",
        "mysql",
        "max_lifetime_seconds",
        OptionalInteger
    ),
    environment_binding!(
        "YANG_SYSTEM_MYSQL_TEST_BEFORE_ACQUIRE",
        "mysql",
        "test_before_acquire",
        Boolean
    ),
    environment_binding!("YANG_SYSTEM_REDIS_URL", "redis", "url", Text),
    environment_binding!(
        "YANG_SYSTEM_REDIS_MAX_CONNECTIONS",
        "redis",
        "max_connections",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_REDIS_MIN_CONNECTIONS",
        "redis",
        "min_connections",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_REDIS_CONNECT_TIMEOUT_SECONDS",
        "redis",
        "connect_timeout_seconds",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_REDIS_WAIT_TIMEOUT_SECONDS",
        "redis",
        "wait_timeout_seconds",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_REDIS_IDLE_TIMEOUT_SECONDS",
        "redis",
        "idle_timeout_seconds",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_REDIS_MAX_LIFETIME_SECONDS",
        "redis",
        "max_lifetime_seconds",
        OptionalInteger
    ),
    environment_binding!(
        "YANG_SYSTEM_REDIS_TEST_BEFORE_ACQUIRE",
        "redis",
        "test_before_acquire",
        Boolean
    ),
    environment_binding!(
        "YANG_SYSTEM_TOKEN_ACTIVE_KEY_ID",
        "token",
        "active_key_id",
        Text
    ),
    environment_binding!(
        "YANG_SYSTEM_TOKEN_ACTIVE_SECRET",
        "token",
        "active_secret",
        Text
    ),
    environment_binding!(
        "YANG_SYSTEM_TOKEN_RETIRING_KEYS_JSON",
        "token",
        "retiring_keys",
        Json
    ),
    environment_binding!("YANG_SYSTEM_TOKEN_ISSUER", "token", "issuer", Text),
    environment_binding!("YANG_SYSTEM_TOKEN_AUDIENCE", "token", "audience", Text),
    environment_binding!(
        "YANG_SYSTEM_TOKEN_ACCESS_TTL_SECONDS",
        "token",
        "access_ttl_seconds",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_TOKEN_REFRESH_TTL_SECONDS",
        "token",
        "refresh_ttl_seconds",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_STEP_UP_ACTIVE_KEY_ID",
        "step_up",
        "active_key_id",
        Text
    ),
    environment_binding!(
        "YANG_SYSTEM_STEP_UP_ACTIVE_SECRET",
        "step_up",
        "active_secret",
        Text
    ),
    environment_binding!(
        "YANG_SYSTEM_STEP_UP_RETIRING_KEYS_JSON",
        "step_up",
        "retiring_keys",
        Json
    ),
    environment_binding!("YANG_SYSTEM_STEP_UP_ISSUER", "step_up", "issuer", Text),
    environment_binding!("YANG_SYSTEM_STEP_UP_AUDIENCE", "step_up", "audience", Text),
    environment_binding!(
        "YANG_SYSTEM_STEP_UP_CHALLENGE_TTL_SECONDS",
        "step_up",
        "challenge_ttl_seconds",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_STEP_UP_PROOF_TTL_SECONDS",
        "step_up",
        "proof_ttl_seconds",
        Integer
    ),
    environment_binding!("YANG_SYSTEM_EMAIL_SMTP_RELAY", "email.smtp", "relay", Text),
    environment_binding!("YANG_SYSTEM_EMAIL_SMTP_PORT", "email.smtp", "port", Integer),
    environment_binding!(
        "YANG_SYSTEM_EMAIL_SMTP_USERNAME",
        "email.smtp",
        "username",
        Text
    ),
    environment_binding!(
        "YANG_SYSTEM_EMAIL_SMTP_PASSWORD",
        "email.smtp",
        "password",
        Text
    ),
    environment_binding!(
        "YANG_SYSTEM_EMAIL_SMTP_FROM_ADDRESS",
        "email.smtp",
        "from_address",
        Text
    ),
    environment_binding!(
        "YANG_SYSTEM_EMAIL_SMTP_FROM_NAME",
        "email.smtp",
        "from_name",
        Text
    ),
    environment_binding!(
        "YANG_SYSTEM_EMAIL_SMTP_TIMEOUT_SECONDS",
        "email.smtp",
        "timeout_seconds",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_EMAIL_VERIFICATION_NAMESPACE",
        "email.verification",
        "namespace",
        Text
    ),
    environment_binding!(
        "YANG_SYSTEM_EMAIL_VERIFICATION_SECRET",
        "email.verification",
        "secret",
        Text
    ),
    environment_binding!(
        "YANG_SYSTEM_EMAIL_VERIFICATION_TTL_SECONDS",
        "email.verification",
        "ttl_seconds",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_EMAIL_VERIFICATION_RESEND_COOLDOWN_SECONDS",
        "email.verification",
        "resend_cooldown_seconds",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_EMAIL_VERIFICATION_MAX_ATTEMPTS",
        "email.verification",
        "max_attempts",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_EMAIL_VERIFICATION_SEND_WINDOW_SECONDS",
        "email.verification",
        "send_window_seconds",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_EMAIL_VERIFICATION_SEND_IP_ATTEMPTS",
        "email.verification",
        "send_ip_attempts",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_EMAIL_VERIFICATION_SEND_EMAIL_ATTEMPTS",
        "email.verification",
        "send_email_attempts",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_EMAIL_VERIFICATION_SEND_GLOBAL_ATTEMPTS",
        "email.verification",
        "send_global_attempts",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_SECURITY_ARGON2_MAX_CONCURRENCY",
        "security",
        "argon2_max_concurrency",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_SECURITY_AUTH_RATE_LIMIT_WINDOW_SECONDS",
        "security",
        "auth_rate_limit_window_seconds",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_SECURITY_AUTH_RATE_LIMIT_IP_ATTEMPTS",
        "security",
        "auth_rate_limit_ip_attempts",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_SECURITY_AUTH_RATE_LIMIT_USERNAME_ATTEMPTS",
        "security",
        "auth_rate_limit_username_attempts",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_SECURITY_TRUSTED_PROXY_CIDRS",
        "security",
        "trusted_proxy_cidrs",
        StringList
    ),
    environment_binding!(
        "YANG_SYSTEM_SHUTDOWN_TOTAL_TIMEOUT_SECONDS",
        "shutdown",
        "total_timeout_seconds",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_OBSERVABILITY_METRICS_ENABLED",
        "observability",
        "metrics_enabled",
        Boolean
    ),
    environment_binding!(
        "YANG_SYSTEM_OBSERVABILITY_METRICS_BIND",
        "observability",
        "metrics_bind",
        Text
    ),
    environment_binding!(
        "YANG_SYSTEM_OBSERVABILITY_TRACES_ENABLED",
        "observability",
        "traces_enabled",
        Boolean
    ),
    environment_binding!(
        "YANG_SYSTEM_OBSERVABILITY_TRACES_OTLP_ENDPOINT",
        "observability",
        "traces_otlp_endpoint",
        Text
    ),
    environment_binding!(
        "YANG_SYSTEM_OBSERVABILITY_TRACES_SAMPLE_RATIO",
        "observability",
        "traces_sample_ratio",
        Float
    ),
    environment_binding!(
        "YANG_SYSTEM_OBSERVABILITY_TRACES_EXPORT_TIMEOUT_SECONDS",
        "observability",
        "traces_export_timeout_seconds",
        Integer
    ),
    environment_binding!(
        "YANG_SYSTEM_OBSERVABILITY_READINESS_BUDGET_MS",
        "observability",
        "readiness_budget_ms",
        Integer
    ),
    environment_binding!("YANG_SYSTEM_LOGGING_FILTER", "logging", "filter", Text),
];

/// secret provider 只允许覆盖明确标记为敏感的字段。
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SecretKey {
    MysqlUrl,
    RedisUrl,
    TokenActiveSecret,
    TokenRetiringKeys,
    StepUpActiveSecret,
    StepUpRetiringKeys,
    EmailSmtpPassword,
    EmailVerificationSecret,
}

#[cfg(test)]
impl SecretKey {
    fn file_name(self) -> &'static str {
        match self {
            Self::MysqlUrl => "mysql_url",
            Self::RedisUrl => "redis_url",
            Self::TokenActiveSecret => "token_active_secret",
            Self::TokenRetiringKeys => "token_retiring_keys_json",
            Self::StepUpActiveSecret => "step_up_active_secret",
            Self::StepUpRetiringKeys => "step_up_retiring_keys_json",
            Self::EmailSmtpPassword => "email_smtp_password",
            Self::EmailVerificationSecret => "email_verification_secret",
        }
    }
}

#[cfg(test)]
const SECRET_KEYS: &[SecretKey] = &[
    SecretKey::MysqlUrl,
    SecretKey::RedisUrl,
    SecretKey::TokenActiveSecret,
    SecretKey::TokenRetiringKeys,
    SecretKey::StepUpActiveSecret,
    SecretKey::StepUpRetiringKeys,
    SecretKey::EmailSmtpPassword,
    SecretKey::EmailVerificationSecret,
];

const SECRET_BINDINGS: &[SecretBinding] = &[
    SecretBinding::text("mysql_url", "mysql", "url"),
    SecretBinding::text("redis_url", "redis", "url"),
    SecretBinding::text("token_active_secret", "token", "active_secret"),
    SecretBinding::json("token_retiring_keys_json", "token", "retiring_keys"),
    SecretBinding::text("step_up_active_secret", "step_up", "active_secret"),
    SecretBinding::json("step_up_retiring_keys_json", "step_up", "retiring_keys"),
    SecretBinding::text("email_smtp_password", "email.smtp", "password"),
    SecretBinding::text("email_verification_secret", "email.verification", "secret"),
];

const CONFIG_SOURCES: ConfigSources = ConfigSources::new(
    "YANG_SYSTEM_",
    SECRET_DIRECTORY_ENV,
    ENVIRONMENT_BINDINGS,
    SECRET_BINDINGS,
)
.with_ignored_environment_prefixes(&["YANG_SYSTEM_TEST_"]);

#[cfg(test)]
pub(crate) trait SecretProvider {
    fn read(&self, key: SecretKey) -> anyhow::Result<Option<String>>;
}

pub(crate) fn load<T>(path: &Path, read_context: &str) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    CONFIG_SOURCES.load(path, read_context).map_err(Into::into)
}

#[cfg(test)]
pub(crate) fn parse_file_only<T>(raw: &str) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    CONFIG_SOURCES.parse_file_only(raw).map_err(Into::into)
}

#[cfg(test)]
pub(crate) fn parse_with_sources<T>(
    raw: &str,
    environment: &BTreeMap<String, String>,
    provider: Option<&dyn SecretProvider>,
) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    let adapter = provider.map(SecretProviderAdapter);
    CONFIG_SOURCES
        .parse_with_sources(
            raw,
            environment,
            adapter
                .as_ref()
                .map(|value| value as &dyn yang_runtime::config::SecretProvider),
        )
        .map_err(Into::into)
}

#[cfg(test)]
fn collect_environment<I>(variables: I) -> anyhow::Result<BTreeMap<String, String>>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    CONFIG_SOURCES
        .collect_environment_variables(variables)
        .map_err(Into::into)
}

#[cfg(test)]
struct SecretProviderAdapter<'a>(&'a dyn SecretProvider);

#[cfg(test)]
impl yang_runtime::config::SecretProvider for SecretProviderAdapter<'_> {
    fn read(&self, file_name: &str) -> Result<Option<String>, ConfigSourceError> {
        let Some(key) = SECRET_KEYS
            .iter()
            .copied()
            .find(|key| key.file_name() == file_name)
        else {
            return Err(ConfigSourceError::Invalid(format!(
                "未声明的 secret 文件: {file_name}"
            )));
        };
        self.0
            .read(key)
            .map_err(|error| ConfigSourceError::Invalid(error.to_string()))
    }
}

#[cfg(test)]
struct DirectorySecretProvider {
    directory: PathBuf,
}

#[cfg(test)]
impl DirectorySecretProvider {
    fn new(directory: PathBuf) -> anyhow::Result<Self> {
        anyhow::ensure!(directory.is_dir(), "secret provider 必须指向目录");
        Ok(Self { directory })
    }
}

#[cfg(test)]
impl SecretProvider for DirectorySecretProvider {
    fn read(&self, key: SecretKey) -> anyhow::Result<Option<String>> {
        let path = self.directory.join(key.file_name());
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_SECRET_BYTES + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() as u64 <= MAX_SECRET_BYTES, "secret 文件过大");
        let value = String::from_utf8(bytes)?;
        let value = value.trim_end_matches(['\r', '\n']);
        anyhow::ensure!(!value.is_empty(), "secret 文件不能为空");
        anyhow::ensure!(
            !value
                .chars()
                .any(|value| matches!(value, '\0' | '\r' | '\n')),
            "secret 文件只能包含单行文本"
        );
        Ok(Some(value.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct TestConfig {
        mysql: TestMysql,
        token: TestToken,
        step_up: TestToken,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct TestMysql {
        url: String,
        max_lifetime_seconds: Option<u64>,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct TestToken {
        active_secret: String,
        retiring_keys: Vec<TestRetiringKey>,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct TestRetiringKey {
        key_id: String,
        secret: String,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct TestEmailConfig {
        email: TestEmail,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct TestEmail {
        smtp: TestSmtp,
        verification: TestEmailVerification,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct TestSmtp {
        relay: String,
        password: String,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct TestEmailVerification {
        secret: String,
        ttl_seconds: u64,
    }

    struct StaticSecretProvider(BTreeMap<SecretKey, String>);

    impl SecretProvider for StaticSecretProvider {
        fn read(&self, key: SecretKey) -> anyhow::Result<Option<String>> {
            Ok(self.0.get(&key).cloned())
        }
    }

    #[test]
    fn nested_email_environment_and_secret_provider_preserve_precedence() {
        let raw = r#"
[email.smtp]
relay = "smtp.file.example"
password = "file-password"
[email.verification]
secret = "file-verification-secret"
ttl_seconds = 600
"#;
        let environment = BTreeMap::from([
            (
                "YANG_SYSTEM_EMAIL_SMTP_RELAY".to_owned(),
                "smtp.environment.example".to_owned(),
            ),
            (
                "YANG_SYSTEM_EMAIL_SMTP_PASSWORD".to_owned(),
                "environment-password".to_owned(),
            ),
            (
                "YANG_SYSTEM_EMAIL_VERIFICATION_TTL_SECONDS".to_owned(),
                "900".to_owned(),
            ),
        ]);
        let provider = StaticSecretProvider(BTreeMap::from([
            (SecretKey::EmailSmtpPassword, "provider-password".to_owned()),
            (
                SecretKey::EmailVerificationSecret,
                "provider-verification-secret".to_owned(),
            ),
        ]));

        let config: TestEmailConfig = parse_with_sources(raw, &environment, Some(&provider))
            .unwrap_or_else(|error| panic!("嵌套邮箱配置应成功合成: {error:#}"));

        assert_eq!(config.email.smtp.relay, "smtp.environment.example");
        assert_eq!(config.email.smtp.password, "provider-password");
        assert_eq!(
            config.email.verification.secret,
            "provider-verification-secret"
        );
        assert_eq!(config.email.verification.ttl_seconds, 900);
    }

    #[test]
    fn applies_file_then_environment_then_secret_provider() {
        let raw = r#"
[mysql]
url = "mysql://file"
max_lifetime_seconds = 60
[token]
active_secret = "file-secret"
retiring_keys = []
[step_up]
active_secret = "file-step-up-secret"
retiring_keys = []
"#;
        let environment = BTreeMap::from([
            (
                "YANG_SYSTEM_MYSQL_URL".to_owned(),
                "mysql://environment".to_owned(),
            ),
            (
                "YANG_SYSTEM_MYSQL_MAX_LIFETIME_SECONDS".to_owned(),
                "none".to_owned(),
            ),
            (
                "YANG_SYSTEM_TOKEN_ACTIVE_SECRET".to_owned(),
                "environment-secret".to_owned(),
            ),
            (
                "YANG_SYSTEM_TOKEN_RETIRING_KEYS_JSON".to_owned(),
                r#"[{"key_id":"environment","secret":"environment-retiring"}]"#.to_owned(),
            ),
            (
                "YANG_SYSTEM_STEP_UP_ACTIVE_SECRET".to_owned(),
                "environment-step-up-secret".to_owned(),
            ),
            (
                "YANG_SYSTEM_STEP_UP_RETIRING_KEYS_JSON".to_owned(),
                r#"[{"key_id":"environment-step-up","secret":"environment-step-up-retiring"}]"#
                    .to_owned(),
            ),
        ]);
        let provider = StaticSecretProvider(BTreeMap::from([
            (SecretKey::MysqlUrl, "mysql://provider".to_owned()),
            (SecretKey::TokenActiveSecret, "provider-secret".to_owned()),
            (
                SecretKey::TokenRetiringKeys,
                r#"[{"key_id":"provider","secret":"provider-retiring"}]"#.to_owned(),
            ),
            (
                SecretKey::StepUpActiveSecret,
                "provider-step-up-secret".to_owned(),
            ),
            (
                SecretKey::StepUpRetiringKeys,
                r#"[{"key_id":"provider-step-up","secret":"provider-step-up-retiring"}]"#
                    .to_owned(),
            ),
        ]));

        let config: TestConfig = parse_with_sources(raw, &environment, Some(&provider))
            .unwrap_or_else(|error| panic!("三层配置应成功合成: {error:#}"));

        assert_eq!(config.mysql.url, "mysql://provider");
        assert_eq!(config.mysql.max_lifetime_seconds, None);
        assert_eq!(config.token.active_secret, "provider-secret");
        assert_eq!(
            config.token.retiring_keys,
            [TestRetiringKey {
                key_id: "provider".to_owned(),
                secret: "provider-retiring".to_owned(),
            }]
        );
        assert_eq!(config.step_up.active_secret, "provider-step-up-secret");
        assert_eq!(
            config.step_up.retiring_keys,
            [TestRetiringKey {
                key_id: "provider-step-up".to_owned(),
                secret: "provider-step-up-retiring".to_owned(),
            }]
        );
    }

    #[test]
    fn environment_wins_when_provider_has_no_matching_secret() {
        let raw = r#"
[mysql]
url = "mysql://file"
[token]
active_secret = "file-secret"
retiring_keys = []
[step_up]
active_secret = "file-step-up-secret"
retiring_keys = []
"#;
        let environment = BTreeMap::from([(
            "YANG_SYSTEM_MYSQL_URL".to_owned(),
            "mysql://environment".to_owned(),
        )]);
        let provider = StaticSecretProvider(BTreeMap::new());

        let config: TestConfig = parse_with_sources(raw, &environment, Some(&provider))
            .unwrap_or_else(|error| panic!("环境覆盖应成功: {error:#}"));

        assert_eq!(config.mysql.url, "mysql://environment");
        assert_eq!(config.token.active_secret, "file-secret");
        assert!(config.token.retiring_keys.is_empty());
        assert_eq!(config.step_up.active_secret, "file-step-up-secret");
        assert!(config.step_up.retiring_keys.is_empty());
    }

    #[test]
    fn malformed_environment_value_names_source_without_echoing_value() {
        let sensitive_value = "not-a-number-sensitive-fragment";
        let environment = BTreeMap::from([(
            "YANG_SYSTEM_MYSQL_MAX_LIFETIME_SECONDS".to_owned(),
            sensitive_value.to_owned(),
        )]);
        let error = parse_with_sources::<TestConfig>(
            "[mysql]\nurl='mysql://file'\n[token]\nactive_secret='file'\nretiring_keys=[]\n[step_up]\nactive_secret='step-up-file'\nretiring_keys=[]\n",
            &environment,
            None,
        )
        .err()
        .unwrap_or_else(|| panic!("非法环境覆盖必须失败"));
        let message = format!("{error:#}");

        assert!(message.contains("YANG_SYSTEM_MYSQL_MAX_LIFETIME_SECONDS"));
        assert!(!message.contains(sensitive_value));
    }

    #[test]
    fn rejects_unknown_runtime_environment_but_reserves_test_namespace() {
        let error = collect_environment([(
            OsString::from("YANG_SYSTEM_MYSQL_ULR"),
            OsString::from("sensitive-value"),
        )])
        .err()
        .unwrap_or_else(|| panic!("拼写错误必须失败"));
        let message = format!("{error:#}");
        assert!(message.contains("YANG_SYSTEM_MYSQL_ULR"));
        assert!(!message.contains("sensitive-value"));

        let environment = collect_environment([
            (
                OsString::from("YANG_SYSTEM_TEST_DATABASE_URL"),
                OsString::from("mysql://test"),
            ),
            (
                OsString::from(SECRET_DIRECTORY_ENV),
                OsString::from("C:\\run\\secrets"),
            ),
        ])
        .unwrap_or_else(|error| panic!("控制变量与测试命名空间应由各自入口消费: {error:#}"));
        assert!(environment.is_empty());
    }

    #[test]
    fn directory_provider_reads_fixed_single_line_secret_files() {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "yang-system-config-source-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&directory)
            .unwrap_or_else(|error| panic!("应创建测试 secret 目录: {error}"));
        let secret_path = directory.join(SecretKey::TokenActiveSecret.file_name());
        std::fs::write(&secret_path, b"provider-secret\r\n")
            .unwrap_or_else(|error| panic!("应写入测试 secret: {error}"));
        let provider = DirectorySecretProvider::new(directory.clone())
            .unwrap_or_else(|error| panic!("合法 secret 目录应被接受: {error:#}"));

        let value = provider
            .read(SecretKey::TokenActiveSecret)
            .unwrap_or_else(|error| panic!("应读取单行 secret: {error:#}"));
        assert_eq!(value.as_deref(), Some("provider-secret"));
        assert!(provider
            .read(SecretKey::MysqlUrl)
            .unwrap_or_else(|error| panic!("缺失文件应安全回退: {error:#}"))
            .is_none());

        std::fs::remove_dir_all(&directory)
            .unwrap_or_else(|error| panic!("应清理测试 secret 目录: {error}"));
    }
}

use anyhow::{bail, Context};
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::Path;
use yang_db::{DatabaseConfig, RedisConfig};

const MAX_ACCESS_TTL_SECONDS: u64 = 24 * 60 * 60;
const MAX_REFRESH_TTL_SECONDS: u64 = 90 * 24 * 60 * 60;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub app: AppSettings,
    pub schema: SchemaSettings,
    pub http: HttpSettings,
    pub mysql: MysqlSettings,
    pub redis: RedisSettings,
    pub token: TokenSettings,
    pub security: SecuritySettings,
    pub logging: LoggingSettings,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSettings {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaSettings {
    pub mode: SchemaMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaMode {
    Apply,
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
    pub secret: String,
    pub issuer: String,
    pub audience: String,
    pub access_ttl_seconds: u64,
    pub refresh_ttl_seconds: u64,
}

impl std::fmt::Debug for TokenSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TokenSettings")
            .field("secret", &"[REDACTED]")
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .field("access_ttl_seconds", &self.access_ttl_seconds)
            .field("refresh_ttl_seconds", &self.refresh_ttl_seconds)
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
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggingSettings {
    pub filter: String,
}

impl Settings {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("读取配置文件失败: {}", path.display()))?;
        Self::parse(&raw)
    }

    fn parse(raw: &str) -> anyhow::Result<Self> {
        let settings: Self = toml::from_str(raw).context("解析配置文件失败")?;
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
        DatabaseConfig::default()
            .with_max_connections(self.mysql.max_connections)
            .with_min_connections(self.mysql.min_connections)
            .with_connect_timeout(self.mysql.connect_timeout_seconds)
            .with_idle_timeout(self.mysql.idle_timeout_seconds)
            .with_max_lifetime(self.mysql.max_lifetime_seconds)
            .with_test_before_acquire(self.mysql.test_before_acquire)
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
        validate_token_secret(&self.token.secret)?;
        if self.security.argon2_max_concurrency == 0 {
            bail!("security.argon2_max_concurrency 必须大于 0");
        }
        validate_rate_limit(
            "window_seconds",
            self.security.auth_rate_limit_window_seconds,
        )?;
        validate_rate_limit("ip_attempts", self.security.auth_rate_limit_ip_attempts)?;
        validate_rate_limit(
            "username_attempts",
            self.security.auth_rate_limit_username_attempts,
        )?;
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
        bail!("token.secret 至少需要 32 字节");
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
        bail!("token.secret 不能使用示例值、占位值或重复字符");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config() -> &'static str {
        r#"
[app]
name = "test"
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
secret = "0123456789abcdef0123456789abcdef"
issuer = "test"
audience = "test-api"
access_ttl_seconds = 60
refresh_ttl_seconds = 120
[security]
argon2_max_concurrency = 4
auth_rate_limit_window_seconds = 60
auth_rate_limit_ip_attempts = 30
auth_rate_limit_username_attempts = 10
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
        assert!(!format!("{:?}", settings.token).contains(&settings.token.secret));
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
}

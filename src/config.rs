use anyhow::{bail, Context};
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::Path;
use yang_db::{DatabaseConfig, RedisConfig};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub app: AppSettings,
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

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpSettings {
    pub bind: String,
    pub max_body_bytes: usize,
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
    pub username_min_length: usize,
    pub username_max_length: usize,
    pub password_min_length: usize,
    pub password_max_length: usize,
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
        Self::parse_with(&raw, |name| std::env::var(name).ok())
    }

    fn parse_with(raw: &str, lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let expanded = interpolate_environment(raw, lookup)?;
        let settings: Self = toml::from_str(&expanded).context("解析 config.toml 失败")?;
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
        self.mysql_config().validate().context("mysql 配置无效")?;
        self.redis_config().validate().context("redis 配置无效")?;
        if self.token.secret.len() < 32 {
            bail!("token.secret 至少需要 32 字节");
        }
        if self.token.issuer.trim().is_empty() || self.token.audience.trim().is_empty() {
            bail!("token.issuer 与 token.audience 不能为空");
        }
        if self.token.access_ttl_seconds == 0 || self.token.refresh_ttl_seconds == 0 {
            bail!("Token 有效期必须大于 0 秒");
        }
        if self.token.refresh_ttl_seconds <= self.token.access_ttl_seconds {
            bail!("refresh token 有效期必须长于 access token");
        }
        validate_range(
            "username",
            self.security.username_min_length,
            self.security.username_max_length,
        )?;
        validate_range(
            "password",
            self.security.password_min_length,
            self.security.password_max_length,
        )?;
        Ok(())
    }
}

fn validate_range(name: &str, minimum: usize, maximum: usize) -> anyhow::Result<()> {
    if minimum == 0 || maximum < minimum {
        bail!("security.{name}_min_length/max_length 范围无效");
    }
    Ok(())
}

fn interpolate_environment(
    input: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<String> {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let tail = &rest[start + 2..];
        let end = tail
            .find('}')
            .context("配置中的环境变量占位符缺少右花括号")?;
        let name = &tail[..end];
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            bail!("环境变量占位符名称无效: {name}");
        }
        let value = lookup(name).with_context(|| format!("缺少环境变量: {name}"))?;
        output.push_str(&value);
        rest = &tail[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn valid_config() -> &'static str {
        r#"
[app]
name = "test"
[http]
bind = "127.0.0.1:8080"
max_body_bytes = 1024
[mysql]
url = "${MYSQL_URL}"
max_connections = 2
min_connections = 0
connect_timeout_seconds = 2
idle_timeout_seconds = 30
max_lifetime_seconds = 60
test_before_acquire = false
[redis]
url = "${REDIS_URL}"
max_connections = 2
min_connections = 0
connect_timeout_seconds = 2
wait_timeout_seconds = 3
idle_timeout_seconds = 30
max_lifetime_seconds = 60
test_before_acquire = false
[token]
secret = "${TOKEN_SECRET}"
issuer = "test"
audience = "test-api"
access_ttl_seconds = 60
refresh_ttl_seconds = 120
[security]
username_min_length = 3
username_max_length = 64
password_min_length = 10
password_max_length = 128
[logging]
filter = "info"
"#
    }

    #[test]
    fn parses_environment_placeholders_and_redacts_token_debug() {
        let values = HashMap::from([
            ("MYSQL_URL", "mysql://example/test"),
            ("REDIS_URL", "redis://example"),
            ("TOKEN_SECRET", "01234567890123456789012345678901"),
        ]);
        let settings = Settings::parse_with(valid_config(), |name| {
            values.get(name).map(|value| (*value).to_string())
        })
        .unwrap_or_else(|error| panic!("有效配置应解析成功: {error}"));

        assert_eq!(settings.mysql.url, "mysql://example/test");
        assert!(!format!("{:?}", settings.token).contains(&settings.token.secret));
    }

    #[test]
    fn rejects_missing_environment_variable() {
        let error = match Settings::parse_with(valid_config(), |_| None) {
            Ok(_) => panic!("缺少环境变量时应失败"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("MYSQL_URL"));
    }
}

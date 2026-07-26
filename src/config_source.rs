//! 启动期配置源合成：配置文件 < 环境变量 < 目录型 secret provider。
//!
//! 本模块只在进程启动时工作。合成完成后，业务代码仍只消费不可变的强类型
//! `Settings`，不会形成第二套动态配置运行时。

use anyhow::{bail, Context};
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use toml::map::Map;
use toml::Value;

const SECRET_DIRECTORY_ENV: &str = "YANG_SYSTEM_SECRET_DIR";
const MAX_SECRET_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy)]
enum EnvironmentValueKind {
    Text,
    Integer,
    OptionalInteger,
    Boolean,
    StringList,
    Json,
}

#[derive(Debug, Clone, Copy)]
struct EnvironmentBinding {
    variable: &'static str,
    section: &'static str,
    field: &'static str,
    kind: EnvironmentValueKind,
}

macro_rules! environment_binding {
    ($variable:literal, $section:literal, $field:literal, $kind:ident) => {
        EnvironmentBinding {
            variable: $variable,
            section: $section,
            field: $field,
            kind: EnvironmentValueKind::$kind,
        }
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
    environment_binding!("YANG_SYSTEM_SCHEMA_MODE", "schema", "mode", Text),
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
        "YANG_SYSTEM_BOOTSTRAP_SECRET_DIGEST",
        "bootstrap",
        "secret_digest",
        Text
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
    environment_binding!("YANG_SYSTEM_LOGGING_FILTER", "logging", "filter", Text),
];

/// secret provider 只允许覆盖明确标记为敏感的字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SecretKey {
    MysqlUrl,
    RedisUrl,
    TokenActiveSecret,
    TokenRetiringKeys,
    BootstrapSecretDigest,
}

impl SecretKey {
    fn file_name(self) -> &'static str {
        match self {
            Self::MysqlUrl => "mysql_url",
            Self::RedisUrl => "redis_url",
            Self::TokenActiveSecret => "token_active_secret",
            Self::TokenRetiringKeys => "token_retiring_keys_json",
            Self::BootstrapSecretDigest => "bootstrap_secret_digest",
        }
    }

    fn destination(self) -> (&'static str, &'static str) {
        match self {
            Self::MysqlUrl => ("mysql", "url"),
            Self::RedisUrl => ("redis", "url"),
            Self::TokenActiveSecret => ("token", "active_secret"),
            Self::TokenRetiringKeys => ("token", "retiring_keys"),
            Self::BootstrapSecretDigest => ("bootstrap", "secret_digest"),
        }
    }

    fn is_json(self) -> bool {
        matches!(self, Self::TokenRetiringKeys)
    }
}

const SECRET_KEYS: &[SecretKey] = &[
    SecretKey::MysqlUrl,
    SecretKey::RedisUrl,
    SecretKey::TokenActiveSecret,
    SecretKey::TokenRetiringKeys,
    SecretKey::BootstrapSecretDigest,
];

pub(crate) trait SecretProvider {
    fn read(&self, key: SecretKey) -> anyhow::Result<Option<String>>;
}

struct DirectorySecretProvider {
    directory: PathBuf,
}

impl DirectorySecretProvider {
    fn new(directory: PathBuf) -> anyhow::Result<Self> {
        let metadata = std::fs::metadata(&directory).with_context(|| {
            format!(
                "{SECRET_DIRECTORY_ENV} 指定的 secret 目录不可访问: {}",
                directory.display()
            )
        })?;
        if !metadata.is_dir() {
            bail!(
                "{SECRET_DIRECTORY_ENV} 必须指向目录: {}",
                directory.display()
            );
        }
        Ok(Self { directory })
    }
}

impl SecretProvider for DirectorySecretProvider {
    fn read(&self, key: SecretKey) -> anyhow::Result<Option<String>> {
        let path = self.directory.join(key.file_name());
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("读取 secret 文件失败: {}", path.display()));
            }
        };
        let mut bytes = Vec::new();
        file.take(MAX_SECRET_BYTES + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("读取 secret 文件失败: {}", path.display()))?;
        if bytes.len() as u64 > MAX_SECRET_BYTES {
            bail!(
                "secret 文件超过 {} 字节上限: {}",
                MAX_SECRET_BYTES,
                path.display()
            );
        }
        let value = String::from_utf8(bytes)
            .map_err(|_| anyhow::anyhow!("secret 文件必须是 UTF-8 文本: {}", path.display()))?;
        let value = value
            .strip_suffix("\r\n")
            .or_else(|| value.strip_suffix('\n'))
            .or_else(|| value.strip_suffix('\r'))
            .unwrap_or(&value);
        if value.is_empty() {
            bail!("secret 文件不能为空: {}", path.display());
        }
        if value
            .chars()
            .any(|character| matches!(character, '\0' | '\r' | '\n'))
        {
            bail!("secret 文件只能包含单行文本: {}", path.display());
        }
        Ok(Some(value.to_owned()))
    }
}

enum ResolvedOverride {
    Set(Value),
    Remove,
}

pub(crate) fn load<T>(path: &Path, read_context: &str) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("{read_context}: {}", path.display()))?;
    let environment = process_environment()?;
    let provider = process_secret_provider()?;
    parse_with_sources(
        &raw,
        &environment,
        provider.as_ref().map(|value| value as &dyn SecretProvider),
    )
}

#[cfg(test)]
pub(crate) fn parse_file_only<T>(raw: &str) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    parse_with_sources(raw, &BTreeMap::new(), None)
}

pub(crate) fn parse_with_sources<T>(
    raw: &str,
    environment: &BTreeMap<String, String>,
    provider: Option<&dyn SecretProvider>,
) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    let mut document: Value = toml::from_str(raw).context("解析配置文件失败")?;
    ensure_document_table(&document)?;
    apply_environment(&mut document, environment)?;
    if let Some(provider) = provider {
        apply_secrets(&mut document, provider)?;
    }
    document.try_into().context("解析配置文件失败")
}

fn process_environment() -> anyhow::Result<BTreeMap<String, String>> {
    collect_environment(std::env::vars_os())
}

fn collect_environment<I>(variables: I) -> anyhow::Result<BTreeMap<String, String>>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let mut environment = BTreeMap::new();
    for (name, value) in variables {
        let Ok(name) = name.into_string() else {
            continue;
        };
        if !name.starts_with("YANG_SYSTEM_")
            || name == SECRET_DIRECTORY_ENV
            || name.starts_with("YANG_SYSTEM_TEST_")
        {
            continue;
        }
        let Some(binding) = ENVIRONMENT_BINDINGS
            .iter()
            .find(|binding| binding.variable == name)
        else {
            bail!("不支持的 YANG System 环境变量: {name}");
        };
        environment.insert(
            binding.variable.to_owned(),
            unicode_environment(binding.variable, value)?,
        );
    }
    Ok(environment)
}

fn process_secret_provider() -> anyhow::Result<Option<DirectorySecretProvider>> {
    let Some(value) = std::env::var_os(SECRET_DIRECTORY_ENV) else {
        return Ok(None);
    };
    let directory = unicode_environment(SECRET_DIRECTORY_ENV, value)?;
    if directory.trim().is_empty() {
        bail!("{SECRET_DIRECTORY_ENV} 不能为空");
    }
    DirectorySecretProvider::new(PathBuf::from(directory)).map(Some)
}

fn unicode_environment(variable: &str, value: OsString) -> anyhow::Result<String> {
    value
        .into_string()
        .map_err(|_| anyhow::anyhow!("环境变量 {variable} 必须是 Unicode 文本"))
}

fn ensure_document_table(document: &Value) -> anyhow::Result<()> {
    if !document.is_table() {
        bail!("配置文件顶层必须是 TOML table");
    }
    Ok(())
}

fn apply_environment(
    document: &mut Value,
    environment: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    for binding in ENVIRONMENT_BINDINGS {
        let Some(raw) = environment.get(binding.variable) else {
            continue;
        };
        match parse_environment_value(binding, raw)? {
            ResolvedOverride::Set(value) => {
                set_value(document, binding.section, binding.field, value)?;
            }
            ResolvedOverride::Remove => {
                remove_value(document, binding.section, binding.field)?;
            }
        }
    }
    Ok(())
}

fn parse_environment_value(
    binding: &EnvironmentBinding,
    raw: &str,
) -> anyhow::Result<ResolvedOverride> {
    let value = match binding.kind {
        EnvironmentValueKind::Text => Value::String(raw.to_owned()),
        EnvironmentValueKind::Integer => {
            Value::Integer(parse_non_negative_integer(binding.variable, raw)?)
        }
        EnvironmentValueKind::OptionalInteger
            if raw.trim().is_empty() || raw.trim().eq_ignore_ascii_case("none") =>
        {
            return Ok(ResolvedOverride::Remove);
        }
        EnvironmentValueKind::OptionalInteger => {
            Value::Integer(parse_non_negative_integer(binding.variable, raw)?)
        }
        EnvironmentValueKind::Boolean => {
            let parsed = match raw.trim() {
                "true" => true,
                "false" => false,
                _ => bail!("环境变量 {} 必须是小写 true 或 false", binding.variable),
            };
            Value::Boolean(parsed)
        }
        EnvironmentValueKind::StringList => {
            let values = if raw.trim().is_empty() {
                Vec::new()
            } else {
                raw.split(',')
                    .map(str::trim)
                    .map(|item| {
                        if item.is_empty() {
                            bail!("环境变量 {} 包含空列表项", binding.variable);
                        }
                        Ok(Value::String(item.to_owned()))
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?
            };
            Value::Array(values)
        }
        EnvironmentValueKind::Json => parse_json_override(binding.variable, raw)?,
    };
    Ok(ResolvedOverride::Set(value))
}

fn parse_non_negative_integer(variable: &str, raw: &str) -> anyhow::Result<i64> {
    raw.trim()
        .parse::<i64>()
        .ok()
        .filter(|value| *value >= 0)
        .with_context(|| format!("环境变量 {variable} 必须是非负十进制整数"))
}

fn parse_json_override(source: &str, raw: &str) -> anyhow::Result<Value> {
    let json: serde_json::Value =
        serde_json::from_str(raw).with_context(|| format!("{source} 必须是合法 JSON"))?;
    Value::try_from(json).with_context(|| format!("{source} JSON 不能转换为 TOML 配置值"))
}

fn apply_secrets(document: &mut Value, provider: &dyn SecretProvider) -> anyhow::Result<()> {
    for key in SECRET_KEYS {
        if let Some(value) = provider
            .read(*key)
            .with_context(|| format!("加载 secret {} 失败", key.file_name()))?
        {
            let (section, field) = key.destination();
            let value = if key.is_json() {
                parse_json_override(key.file_name(), &value)?
            } else {
                Value::String(value)
            };
            set_value(document, section, field, value)?;
        }
    }
    Ok(())
}

fn set_value(document: &mut Value, section: &str, field: &str, value: Value) -> anyhow::Result<()> {
    let root = document
        .as_table_mut()
        .context("配置文件顶层必须是 TOML table")?;
    let section_value = root
        .entry(section.to_owned())
        .or_insert_with(|| Value::Table(Map::new()));
    let table = section_value
        .as_table_mut()
        .with_context(|| format!("配置项 {section} 必须是 TOML table"))?;
    table.insert(field.to_owned(), value);
    Ok(())
}

fn remove_value(document: &mut Value, section: &str, field: &str) -> anyhow::Result<()> {
    let root = document
        .as_table_mut()
        .context("配置文件顶层必须是 TOML table")?;
    let Some(section_value) = root.get_mut(section) else {
        return Ok(());
    };
    let table = section_value
        .as_table_mut()
        .with_context(|| format!("配置项 {section} 必须是 TOML table"))?;
    table.remove(field);
    Ok(())
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

    struct StaticSecretProvider(BTreeMap<SecretKey, String>);

    impl SecretProvider for StaticSecretProvider {
        fn read(&self, key: SecretKey) -> anyhow::Result<Option<String>> {
            Ok(self.0.get(&key).cloned())
        }
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
        ]);
        let provider = StaticSecretProvider(BTreeMap::from([
            (SecretKey::MysqlUrl, "mysql://provider".to_owned()),
            (SecretKey::TokenActiveSecret, "provider-secret".to_owned()),
            (
                SecretKey::TokenRetiringKeys,
                r#"[{"key_id":"provider","secret":"provider-retiring"}]"#.to_owned(),
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
    }

    #[test]
    fn environment_wins_when_provider_has_no_matching_secret() {
        let raw = r#"
[mysql]
url = "mysql://file"
[token]
active_secret = "file-secret"
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
    }

    #[test]
    fn malformed_environment_value_names_source_without_echoing_value() {
        let sensitive_value = "not-a-number-sensitive-fragment";
        let environment = BTreeMap::from([(
            "YANG_SYSTEM_MYSQL_MAX_LIFETIME_SECONDS".to_owned(),
            sensitive_value.to_owned(),
        )]);
        let error = parse_with_sources::<TestConfig>(
            "[mysql]\nurl='mysql://file'\n[token]\nactive_secret='file'\nretiring_keys=[]\n",
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

mod source;

use anyhow::{bail, Context};
use serde::Deserialize;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;
use yang_base::action::auth::{AuthRateLimitConfig, EmailVerificationConfig};
use yang_base::action::StepUpManager;
use yang_base::token::TokenManager;
use yang_db::{DatabaseConfig, RedisConfig};
pub use yang_runtime::observability::ObservabilitySettings;

const MAX_ACCESS_TTL_SECONDS: u64 = 24 * 60 * 60;
const MAX_REFRESH_TTL_SECONDS: u64 = 90 * 24 * 60 * 60;

/// 「踢出单台设备」时 jti 黑名单必须保留的最长时间（秒）。
///
/// 取 refresh token 有效期的**校验上限**而非当前配置值：任何合法签发的 refresh token
/// 都不会比它更长寿，因此黑名单条目不可能先于它要吊销的令牌过期。若改用当前配置值，
/// 一旦两者不同步（历史缺陷：黑名单硬编码 7 天，而默认 refresh TTL 是 30 天、上限 90 天），
/// 被踢设备会在黑名单到期后凭原 refresh cookie 重新轮换，逐台撤销静默失效。
pub(crate) const REVOCATION_BLACKLIST_TTL_SECONDS: u64 = MAX_REFRESH_TTL_SECONDS;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub app: AppSettings,
    pub authorization: AuthorizationSettings,
    #[serde(default)]
    pub http: HttpSettings,
    pub mysql: MysqlSettings,
    pub redis: RedisSettings,
    pub token: TokenSettings,
    pub step_up: StepUpSettings,
    pub email: EmailSettings,
    #[serde(default)]
    pub security: SecuritySettings,
    #[serde(default)]
    pub shutdown: ShutdownSettings,
    #[serde(default)]
    pub observability: ObservabilitySettings,
    #[serde(default)]
    pub logging: LoggingSettings,
    /// 飞书外部数据源集成；`None` 表示未启用。
    ///
    /// 整段可选（照 `email.change` / `security.totp` 的既有形态）：省略时行为与未集成
    /// 完全一致，因此 `config.example.toml` 无需新增键，不会影响
    /// `example_config_has_no_schema_mode` 的精确长度断言。
    #[serde(default)]
    pub feishu: Option<FeishuSettings>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSettings {
    #[serde(default = "default_app_name")]
    pub name: String,
    #[serde(default)]
    pub environment: DeploymentEnvironment,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            name: default_app_name(),
            environment: DeploymentEnvironment::default(),
        }
    }
}

fn default_app_name() -> String {
    "yang-system".to_owned()
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationSettings {
    pub deployment: String,
    #[serde(default = "default_outbox_poll_interval_ms")]
    pub outbox_poll_interval_ms: u64,
    #[serde(default = "default_outbox_batch_size")]
    pub outbox_batch_size: u32,
    #[serde(default = "default_outbox_lease_seconds")]
    pub outbox_lease_seconds: u64,
    #[serde(default = "default_outbox_max_retry_seconds")]
    pub outbox_max_retry_seconds: u64,
}

const fn default_outbox_poll_interval_ms() -> u64 {
    250
}

const fn default_outbox_batch_size() -> u32 {
    100
}

const fn default_outbox_lease_seconds() -> u64 {
    10
}

const fn default_outbox_max_retry_seconds() -> u64 {
    60
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpSettings {
    #[serde(default = "default_http_bind")]
    pub bind: String,
    #[serde(default = "default_http_max_body_bytes")]
    pub max_body_bytes: usize,
    #[serde(default = "default_http_request_timeout_seconds")]
    pub request_timeout_seconds: u64,
    #[serde(default = "default_http_max_concurrency")]
    pub max_concurrency: usize,
}

impl Default for HttpSettings {
    fn default() -> Self {
        Self {
            bind: default_http_bind(),
            max_body_bytes: default_http_max_body_bytes(),
            request_timeout_seconds: default_http_request_timeout_seconds(),
            max_concurrency: default_http_max_concurrency(),
        }
    }
}

fn default_http_bind() -> String {
    "127.0.0.1:8080".to_owned()
}

const fn default_http_max_body_bytes() -> usize {
    1_048_576
}

const fn default_http_request_timeout_seconds() -> u64 {
    30
}

const fn default_http_max_concurrency() -> usize {
    256
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MysqlSettings {
    pub url: String,
    #[serde(default = "default_mysql_max_connections")]
    pub max_connections: u32,
    #[serde(default = "default_mysql_min_connections")]
    pub min_connections: u32,
    #[serde(default = "default_mysql_connect_timeout_seconds")]
    pub connect_timeout_seconds: u64,
    #[serde(default = "default_mysql_idle_timeout_seconds")]
    pub idle_timeout_seconds: u64,
    #[serde(default = "default_pool_max_lifetime_seconds")]
    pub max_lifetime_seconds: Option<u64>,
    #[serde(default = "default_test_before_acquire")]
    pub test_before_acquire: bool,
}

const fn default_mysql_max_connections() -> u32 {
    20
}

const fn default_mysql_min_connections() -> u32 {
    2
}

const fn default_mysql_connect_timeout_seconds() -> u64 {
    10
}

const fn default_mysql_idle_timeout_seconds() -> u64 {
    600
}

const fn default_pool_max_lifetime_seconds() -> Option<u64> {
    Some(1800)
}

const fn default_test_before_acquire() -> bool {
    true
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedisSettings {
    pub url: String,
    #[serde(default = "default_redis_max_connections")]
    pub max_connections: usize,
    #[serde(default = "default_redis_min_connections")]
    pub min_connections: usize,
    #[serde(default = "default_redis_connect_timeout_seconds")]
    pub connect_timeout_seconds: u64,
    #[serde(default = "default_redis_wait_timeout_seconds")]
    pub wait_timeout_seconds: u64,
    #[serde(default = "default_redis_idle_timeout_seconds")]
    pub idle_timeout_seconds: u64,
    #[serde(default = "default_pool_max_lifetime_seconds")]
    pub max_lifetime_seconds: Option<u64>,
    #[serde(default = "default_test_before_acquire")]
    pub test_before_acquire: bool,
}

const fn default_redis_max_connections() -> usize {
    20
}

const fn default_redis_min_connections() -> usize {
    2
}

const fn default_redis_connect_timeout_seconds() -> u64 {
    5
}

const fn default_redis_wait_timeout_seconds() -> u64 {
    10
}

const fn default_redis_idle_timeout_seconds() -> u64 {
    300
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenSettings {
    pub active_key_id: String,
    pub active_secret: String,
    #[serde(default)]
    pub retiring_keys: Vec<RetiringTokenKeySettings>,
    #[serde(default = "default_token_issuer")]
    pub issuer: String,
    #[serde(default = "default_token_audience")]
    pub audience: String,
    #[serde(default = "default_access_ttl_seconds")]
    pub access_ttl_seconds: u64,
    #[serde(default = "default_refresh_ttl_seconds")]
    pub refresh_ttl_seconds: u64,
}

fn default_token_issuer() -> String {
    "yang-system".to_owned()
}

fn default_token_audience() -> String {
    "yang-system-api".to_owned()
}

const fn default_access_ttl_seconds() -> u64 {
    3600
}

const fn default_refresh_ttl_seconds() -> u64 {
    2_592_000
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
    #[serde(default = "default_step_up_issuer")]
    pub issuer: String,
    #[serde(default = "default_step_up_audience")]
    pub audience: String,
    #[serde(default = "default_challenge_ttl_seconds")]
    pub challenge_ttl_seconds: u64,
    #[serde(default = "default_proof_ttl_seconds")]
    pub proof_ttl_seconds: u64,
}

fn default_step_up_issuer() -> String {
    "yang-system-step-up".to_owned()
}

fn default_step_up_audience() -> String {
    "yang-system-sensitive-actions".to_owned()
}

const fn default_challenge_ttl_seconds() -> u64 {
    120
}

const fn default_proof_ttl_seconds() -> u64 {
    300
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailSettings {
    pub smtp: SmtpSettings,
    pub verification: EmailVerificationSettings,
    /// 邮箱换绑验证码的独立配置段：命名空间与密钥必须与注册验证码隔离。
    #[serde(default)]
    pub change: Option<EmailVerificationSettings>,
    /// 登录 MFA 备用邮箱验证码的独立配置段：命名空间与密钥必须与注册/换绑
    /// 验证码隔离；未配置时邮箱验证码登录不可用（Action 返回未启用错误）。
    #[serde(default)]
    pub mfa: Option<EmailVerificationSettings>,
    /// 邮箱验证码免密登录的独立配置段：命名空间与密钥必须与注册/换绑/MFA
    /// 验证码隔离；未配置时免密登录不可用（Action 返回未启用错误）。
    #[serde(default)]
    pub login: Option<EmailVerificationSettings>,
    pub password_reset: PasswordResetEmailSettings,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmtpSettings {
    pub relay: String,
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    pub username: String,
    pub password: String,
    pub from_address: String,
    pub from_name: String,
    #[serde(default = "default_smtp_timeout_seconds")]
    pub timeout_seconds: u64,
}

const fn default_smtp_port() -> u16 {
    587
}

const fn default_smtp_timeout_seconds() -> u64 {
    10
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailVerificationSettings {
    /// Redis key namespace，隔离共享 Redis 上的部署环境；缺省继承 `authorization.deployment`。
    #[serde(default)]
    pub namespace: String,
    /// 验证码摘要的独立服务端密钥，不得与 Token/Step-up 密钥复用。
    pub secret: String,
    #[serde(default = "default_email_code_ttl_seconds")]
    pub ttl_seconds: u64,
    #[serde(default = "default_email_code_resend_cooldown_seconds")]
    pub resend_cooldown_seconds: u64,
    #[serde(default = "default_email_code_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_email_code_send_window_seconds")]
    pub send_window_seconds: u64,
    #[serde(default = "default_email_code_send_ip_attempts")]
    pub send_ip_attempts: u64,
    #[serde(default = "default_email_code_send_email_attempts")]
    pub send_email_attempts: u64,
    #[serde(default = "default_email_code_send_global_attempts")]
    pub send_global_attempts: u64,
}

const fn default_email_code_ttl_seconds() -> u64 {
    600
}

const fn default_email_code_resend_cooldown_seconds() -> u64 {
    60
}

const fn default_email_code_max_attempts() -> u32 {
    5
}

const fn default_email_code_send_window_seconds() -> u64 {
    3600
}

const fn default_email_code_send_ip_attempts() -> u64 {
    20
}

const fn default_email_code_send_email_attempts() -> u64 {
    5
}

const fn default_email_code_send_global_attempts() -> u64 {
    1000
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasswordResetEmailSettings {
    /// 密码重置邮件中链接指向的前端控制台入口（scheme + host[:port]，无路径与查询串）。
    /// 必须从配置注入，不得从请求 Host 头推导（Host 头可被攻击者伪造）。
    pub link_base_url: String,
}

impl std::fmt::Debug for EmailSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmailSettings")
            .field("smtp", &self.smtp)
            .field("verification", &self.verification)
            .field("change", &self.change)
            .field("mfa", &self.mfa)
            .field("login", &self.login)
            .field("password_reset", &self.password_reset)
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
    #[serde(default = "default_argon2_max_concurrency")]
    pub argon2_max_concurrency: usize,
    #[serde(default = "default_auth_rate_limit_window_seconds")]
    pub auth_rate_limit_window_seconds: u64,
    #[serde(default = "default_auth_rate_limit_ip_attempts")]
    pub auth_rate_limit_ip_attempts: u64,
    #[serde(default = "default_auth_rate_limit_username_attempts")]
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
    /// TOTP 第二因子配置域（E-1）；`None` 时 MFA Action 不注册。
    #[serde(default)]
    pub totp: Option<TotpSettings>,
}

impl Default for SecuritySettings {
    fn default() -> Self {
        Self {
            argon2_max_concurrency: default_argon2_max_concurrency(),
            auth_rate_limit_window_seconds: default_auth_rate_limit_window_seconds(),
            auth_rate_limit_ip_attempts: default_auth_rate_limit_ip_attempts(),
            auth_rate_limit_username_attempts: default_auth_rate_limit_username_attempts(),
            password_reset_ttl_seconds: default_password_reset_ttl_seconds(),
            issue_refresh_credential_version: false,
            trusted_proxy_cidrs: Vec::new(),
            totp: None,
        }
    }
}

/// 飞书外部数据源集成配置。
///
/// 整段可选：省略时相关路由不注册，服务行为与未集成飞书时完全一致。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeishuSettings {
    /// 是否启用飞书集成；关闭时不注册任何飞书路由。
    #[serde(default)]
    pub enabled: bool,
    /// 多维表格工作流调用写入 API 时使用的静态 Token。
    ///
    /// 与 [外部选项接口的数据源 Token] 是两条独立的凭证：这条保护**入站写入**，
    /// 那条保护**入站取数**（飞书审批来我们这里取选项，按数据源分别存储其
    /// SHA-256 摘要，见 `feishu_datasource` 表）。本段的 `app_id`/`app_secret`
    /// 是第三条、方向相反的凭证：它让**我们主动出站**调飞书开放平台。
    ///
    /// `#[serde(default)]` 是必需的：secret 目录型 provider 是「先建表后插入」
    /// （`yang-runtime` 的 `apply_secrets`），只要目录里存在任一 feishu secret 文件，
    /// 它就会在文档里造出半张 `[feishu]` 表；此时若本字段无默认值，配置解析会在
    /// **反序列化阶段**以 `missing field` 失败，且与 `enabled` 无关。段真正是否
    /// 生效仍由 [`FeishuSettings::is_usable`] 判定。
    #[serde(default)]
    pub management_api_token: String,
    /// 外部选项接口的 AES 密钥原文；配置后按 `sha256(原文)` 派生 256 位密钥。
    ///
    /// 省略表示明文返回（对应飞书侧「不填写 Key」）。
    #[serde(default)]
    pub encryption_key: Option<String>,
    /// 自建应用的 App ID；出站调用飞书开放平台换取 tenant_access_token 时使用。
    ///
    /// 在开发者后台的 **基础信息 > 凭证与基础信息** 页面获取。省略表示不出站。
    #[serde(default)]
    pub app_id: Option<String>,
    /// 自建应用的 App Secret；与 [`FeishuSettings::app_id`] 成对使用。
    ///
    /// **建议走 secret 目录**（`feishu_app_secret`）而不是配置文件或环境变量——
    /// 它与 `management_api_token` 是两条无关的凭证，泄露面也完全不同：这条是
    /// **租户级**凭证，拿到它能读该应用可见的全部协作多维表格。
    #[serde(default)]
    pub app_secret: Option<String>,
    /// 出站拉取轮询间隔（秒）。
    ///
    /// 契约是「最大可见延迟 = 一个轮询间隔」，因此本值就是新鲜度承诺。默认 900 秒。
    #[serde(default = "default_feishu_pull_interval_seconds")]
    pub pull_interval_seconds: u64,
    /// 出站拉取连续失败达阈值时的告警收件人。
    ///
    /// **默认空 = 不告警**（设计 D5）：告警往哪儿发是部署方的决定，不是我们的。
    /// 每一项都必须在启动期校验（见 `feishu::domain::alert::validate_recipients`）
    /// ——配了一个空白项等于「以为配上了其实没配」，而那种错**没有任何症状**。
    #[serde(default)]
    pub alert_recipients: Vec<String>,
    /// 连续失败多少轮之后开始告警。
    ///
    /// 每轮都会发，直到有一轮成功把计数清零（设计 §9.2 的收口条件是「恢复」，
    /// 不是冷却）。下限见 [`FEISHU_MIN_ALERT_THRESHOLD`]。默认 3。
    #[serde(default = "default_feishu_alert_threshold")]
    pub alert_failure_threshold: i64,
    /// 是否把三个机器入口的**完整请求参数**写进日志（含 `token` 与认证头明文）。
    ///
    /// **默认关闭，且不应用于生产。** `docs/contracts/OBSERVABILITY.md` 的
    /// 「结构化日志」章节禁止日志记录请求体与 Token，本开关是那条禁令的**唯一例外**：
    /// 联调期必须抓到飞书回传的真实报文，而报文里有几处字段形状项目从未观测过
    /// （见 `docs/architecture/feishu-option-ingest.md` 的 V4）。
    ///
    /// 开启期间 stdout 会含**数据源 Token 与多维表格管理 Token 的明文**，日志采集与
    /// 保留策略须按 `docs/operations/LOG_SHIPPING.md` 另行评估。关闭时不注册中间件，
    /// 因此没有运行期开销。
    #[serde(default)]
    pub log_inbound_requests: bool,
}

/// 出站拉取间隔下限：低于 1 分钟会让飞书侧频控（bitable 列出记录 20 次/秒，
/// 但单表并发受限）变成常态。
const FEISHU_MIN_PULL_INTERVAL_SECONDS: u64 = 10;
/// 上限取 24 小时：再长就等于关掉了同步，应当显式停用数据源而不是把间隔调到天上。
const FEISHU_MAX_PULL_INTERVAL_SECONDS: u64 = 86_400;

pub(crate) const fn default_feishu_pull_interval_seconds() -> u64 {
    900
}

/// 告警阈值下限：**2 轮**。
///
/// 阈值 1 会把单次失败也发出去，而单次失败与飞书侧抖动无法区分——这正是阈值要挡的
/// 邮件风暴。允许 1 等于把「阈值」这个机制关掉，那种部署应该走「不配收件人」。
const FEISHU_MIN_ALERT_THRESHOLD: i64 = 2;
/// 上限 1000：再大就等于永久静音，那也该走「不配收件人」，而不是把阈值调到天上。
const FEISHU_MAX_ALERT_THRESHOLD: i64 = 1_000;

pub(crate) const fn default_feishu_alert_threshold() -> i64 {
    3
}

impl FeishuSettings {
    /// 是否具备注册对外路由的最小条件。
    ///
    /// **只表示「入站可用」**（飞书来调我们）。出站可用性见 [`Self::can_pull`]——
    /// 两者刻意分开：把出站凭证并进这里会改变入站路由的注册条件，让一次
    /// 「还没配 app_id」的滚动发布把已经在跑的入站端点一起摘掉。
    pub fn is_usable(&self) -> bool {
        self.enabled && !self.management_api_token.trim().is_empty()
    }

    /// 是否具备出站拉取飞书开放平台的条件。
    ///
    /// 与 [`Self::is_usable`] 相互独立：出站只需要 `app_id`/`app_secret`，
    /// 不需要 `management_api_token`（那是入站写入的凭证）。
    ///
    /// 占位值一律视为未配置。这条判据是**启动期**的闸门：`deploy/config.cloud.toml`
    /// 里预置的就是 `CHANGE_ME_FEISHU_APP_ID` / `CHANGE_ME_FEISHU_APP_SECRET`，
    /// 它们既不是空串、长度也够，只有显式识别才能挡住「worker 拿占位凭证按间隔
    /// 反复出网、每轮都失败并告警」这种启动期就该拦下的状态。
    pub fn can_pull(&self) -> bool {
        self.enabled
            && (FEISHU_MIN_PULL_INTERVAL_SECONDS..=FEISHU_MAX_PULL_INTERVAL_SECONDS)
                .contains(&self.pull_interval_seconds)
            && self.app_id.as_deref().is_some_and(credential_is_configured)
            && self
                .app_secret
                .as_deref()
                .is_some_and(credential_is_configured)
    }
}

/// 判断一条**第三方**凭证是否真的配置了（非空、非占位）。
///
/// 刻意**不复用** [`validate_token_secret`]：那条规则是给我们自己签发的密钥用的
/// （≥32 字节、非重复字符），套到飞书 app_secret 上会把合法的第三方凭证判非法，
/// 在 `[feishu]` 段上重新制造一次启动失败。
fn credential_is_configured(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return false;
    }
    let normalized = trimmed.to_ascii_lowercase();
    // `CHANGE_ME_*` 必须**单独**判：`is_placeholder_secret` 只认 changeme / replace-with*
    // / placeholder 三类，而部署模板用的正是 `CHANGE_ME_FEISHU_APP_ID` 这种带下划线的
    // 拼法（`deploy/config.cloud.example.toml` 明说它「不会被识别为占位」，
    // 部署脚本因此只能用 grep 在部署期兜）。这里补上这一族，让启动期就拦住。
    if normalized.starts_with("change_me") || normalized.starts_with("change-me") {
        return false;
    }
    !is_placeholder_secret(&normalized)
}

const fn default_argon2_max_concurrency() -> usize {
    4
}

const fn default_auth_rate_limit_window_seconds() -> u64 {
    60
}

const fn default_auth_rate_limit_ip_attempts() -> u64 {
    30
}

const fn default_auth_rate_limit_username_attempts() -> u64 {
    10
}

/// TOTP 第二因子配置（路线图 E-1b）：AEAD 密钥域与码位。
///
/// `aead_key` 是加密 `users.totp_secret` 的独立密钥域（32 字节），
/// **禁止**与 token/step-up/邮箱验证码密钥复用——启动校验做交叉隔离检查。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TotpSettings {
    /// AEAD 加密密钥（32 字节；hex 或 base64 或原文均可，取原始字节前 32 位）。
    pub aead_key: String,
    /// TOTP 一次性码位数（默认 6，允许 6..=8）。
    #[serde(default = "default_totp_digits")]
    pub digits: u32,
}

const fn default_totp_digits() -> u32 {
    6
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggingSettings {
    #[serde(default = "default_logging_filter")]
    pub filter: String,
}

impl Default for LoggingSettings {
    fn default() -> Self {
        Self {
            filter: default_logging_filter(),
        }
    }
}

fn default_logging_filter() -> String {
    "yang_system=info,tower_http=info".to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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
        let mut settings: Self = crate::config::source::load(path, "读取配置文件失败")?;
        settings.normalize();
        settings.validate()?;
        Ok(settings)
    }

    #[cfg(test)]
    fn parse(raw: &str) -> anyhow::Result<Self> {
        let mut settings: Self = crate::config::source::parse_file_only(raw)?;
        settings.normalize();
        settings.validate()?;
        Ok(settings)
    }

    /// 派生默认值：邮箱验证码 Redis 命名空间缺省继承 `authorization.deployment`。
    fn normalize(&mut self) {
        if self.email.verification.namespace.is_empty() {
            self.email
                .verification
                .namespace
                .clone_from(&self.authorization.deployment);
        }
        if let Some(change) = &mut self.email.change {
            if change.namespace.is_empty() {
                change.namespace = self.authorization.deployment.clone();
            }
        }
        if let Some(mfa) = &mut self.email.mfa {
            if mfa.namespace.is_empty() {
                mfa.namespace = self.authorization.deployment.clone();
            }
        }
        if let Some(login) = &mut self.email.login {
            if login.namespace.is_empty() {
                login.namespace = self.authorization.deployment.clone();
            }
        }
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
        self.email
            .validate(self.app.environment, &self.token, &self.step_up)?;
        self.security.validate()?;
        // 跨段密钥域隔离：MFA/免密登录邮箱验证码密钥不得与 TOTP AEAD 密钥复用。
        if let Some(totp) = &self.security.totp {
            for (name, section) in [
                ("email.mfa.secret", &self.email.mfa),
                ("email.login.secret", &self.email.login),
            ] {
                if let Some(section) = section {
                    if section.secret == totp.aead_key {
                        bail!("{name} 不得复用 security.totp.aead_key");
                    }
                }
            }
        }
        // 飞书集成：**只校验真正会生效的段**（enabled 且 Token 非空）。
        // 段存在但惰性时（enabled=false，或 Token 留待运维后填）不应让进程起不来——
        // 它不注册任何路由，没有可被误用的行为面。
        if let Some(feishu) = self.feishu.as_ref().filter(|value| value.is_usable()) {
            validate_token_secret(&feishu.management_api_token)
                .context("feishu.management_api_token 无效")?;
            validate_verification_secret(
                "feishu.management_api_token",
                &feishu.management_api_token,
                &self.token,
                &self.step_up,
            )?;
            if let Some(encryption_key) = feishu.encryption_key.as_deref() {
                validate_token_secret(encryption_key).context("feishu.encryption_key 无效")?;
                validate_verification_secret(
                    "feishu.encryption_key",
                    encryption_key,
                    &self.token,
                    &self.step_up,
                )?;
                if let Some(totp) = &self.security.totp {
                    if encryption_key == totp.aead_key {
                        bail!("feishu.encryption_key 不得复用 security.totp.aead_key");
                    }
                }
            }
        }
        // 飞书**出站**拉取：判据是 can_pull() 而不是 is_usable()——出站只需要
        // app_id/app_secret，与入站写入凭证无关。段启用但出站凭证还是占位值时
        // 不做校验：那正是「还没配就部署」的常态，worker 不会起来（can_pull 为假），
        // 不该让进程起不来。
        if let Some(feishu) = self.feishu.as_ref().filter(|value| value.enabled) {
            if !(FEISHU_MIN_PULL_INTERVAL_SECONDS..=FEISHU_MAX_PULL_INTERVAL_SECONDS)
                .contains(&feishu.pull_interval_seconds)
            {
                bail!(
                    "feishu.pull_interval_seconds 必须在 {FEISHU_MIN_PULL_INTERVAL_SECONDS}..={FEISHU_MAX_PULL_INTERVAL_SECONDS} 范围内"
                );
            }
            if !(FEISHU_MIN_ALERT_THRESHOLD..=FEISHU_MAX_ALERT_THRESHOLD)
                .contains(&feishu.alert_failure_threshold)
            {
                bail!(
                    "feishu.alert_failure_threshold 必须在 {FEISHU_MIN_ALERT_THRESHOLD}..={FEISHU_MAX_ALERT_THRESHOLD} 范围内"
                );
            }
            // 收件人逐个校验（空列表合法 = 不告警）。放在**启动期**而不是首次告警时：
            // 一个空白项或拼错的地址在投递那一刻只会静默失败，运维却以为告警在跑。
            crate::addon::feishu::domain::alert::validate_recipients(&feishu.alert_recipients)
                .map_err(|error| anyhow::anyhow!("feishu.alert_recipients 无效：{error}"))?;
            if feishu.can_pull() {
                let app_secret = feishu.app_secret.as_deref().unwrap_or_default();
                // **刻意不用 validate_token_secret**：那条规则（≥32 字节、非重复字符）
                // 是约束我们自己签发的密钥的，套到飞书 app_secret 上会把合法的第三方
                // 凭证判非法。出站凭证的「是否已配置」由 can_pull() 的占位值识别负责。
                validate_verification_secret(
                    "feishu.app_secret",
                    app_secret,
                    &self.token,
                    &self.step_up,
                )?;
                if let Some(totp) = &self.security.totp {
                    if app_secret == totp.aead_key {
                        bail!("feishu.app_secret 不得复用 security.totp.aead_key");
                    }
                }
            }
        }
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
    fn validate(
        &self,
        environment: DeploymentEnvironment,
        token: &TokenSettings,
        step_up: &StepUpSettings,
    ) -> anyhow::Result<()> {
        self.smtp.validate()?;
        self.verification.validate()?;
        self.password_reset.validate(environment)?;
        validate_verification_secret(
            "email.verification.secret",
            &self.verification.secret,
            token,
            step_up,
        )?;
        if let Some(change) = &self.change {
            change.validate()?;
            if change.secret == self.verification.secret {
                bail!("email.change.secret 不得复用注册验证码密钥（email.verification.secret）");
            }
            validate_verification_secret("email.change.secret", &change.secret, token, step_up)?;
        }
        if let Some(mfa) = &self.mfa {
            mfa.validate()?;
            if mfa.secret == self.verification.secret {
                bail!("email.mfa.secret 不得复用注册验证码密钥（email.verification.secret）");
            }
            if let Some(change) = &self.change {
                if mfa.secret == change.secret {
                    bail!("email.mfa.secret 不得复用换绑验证码密钥（email.change.secret）");
                }
            }
            validate_verification_secret("email.mfa.secret", &mfa.secret, token, step_up)?;
        }
        if let Some(login) = &self.login {
            login.validate()?;
            if login.secret == self.verification.secret {
                bail!("email.login.secret 不得复用注册验证码密钥（email.verification.secret）");
            }
            if let Some(change) = &self.change {
                if login.secret == change.secret {
                    bail!("email.login.secret 不得复用换绑验证码密钥（email.change.secret）");
                }
            }
            if let Some(mfa) = &self.mfa {
                if login.secret == mfa.secret {
                    bail!("email.login.secret 不得复用 MFA 验证码密钥（email.mfa.secret）");
                }
            }
            validate_verification_secret("email.login.secret", &login.secret, token, step_up)?;
        }
        Ok(())
    }
}

/// 校验一枚邮箱验证码密钥不与其他 keyring（Token/Step-up）冲突。
fn validate_verification_secret(
    name: &str,
    secret: &str,
    token: &TokenSettings,
    step_up: &StepUpSettings,
) -> anyhow::Result<()> {
    let collides_with_token = std::iter::once(token.active_secret.as_str())
        .chain(token.retiring_keys.iter().map(|key| key.secret.as_str()))
        .any(|candidate| candidate == secret);
    let collides_with_step_up = std::iter::once(step_up.active_secret.as_str())
        .chain(step_up.retiring_keys.iter().map(|key| key.secret.as_str()))
        .any(|candidate| candidate == secret);
    if collides_with_token || collides_with_step_up {
        bail!("{name} 不得复用 Token 或 Step-up 密钥");
    }
    Ok(())
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
    /// 转换为框架验证码引擎的运行时配置。
    ///
    /// Redis key 前缀包含应用名与部署命名空间；指标名保持 yang-system 既有契约。
    pub fn engine_config(&self) -> EmailVerificationConfig {
        EmailVerificationConfig {
            redis_key_prefix: format!("yang-system:{}:registration-email", self.namespace),
            secret: self.secret.clone(),
            ttl_seconds: self.ttl_seconds,
            resend_cooldown_seconds: self.resend_cooldown_seconds,
            max_attempts: self.max_attempts,
            code_digits: 6,
            send_window_seconds: self.send_window_seconds,
            send_ip_attempts: self.send_ip_attempts,
            send_email_attempts: self.send_email_attempts,
            send_global_attempts: self.send_global_attempts,
            send_metric_name: "yang_system_registration_email_total",
            verify_metric_name: "yang_system_registration_email_verify_total",
        }
    }

    /// 转换为邮箱换绑验证码引擎的运行时配置（独立 key 域与指标名）。
    pub fn change_engine_config(&self) -> EmailVerificationConfig {
        EmailVerificationConfig {
            redis_key_prefix: format!("yang-system:{}:change-email", self.namespace),
            secret: self.secret.clone(),
            ttl_seconds: self.ttl_seconds,
            resend_cooldown_seconds: self.resend_cooldown_seconds,
            max_attempts: self.max_attempts,
            code_digits: 6,
            send_window_seconds: self.send_window_seconds,
            send_ip_attempts: self.send_ip_attempts,
            send_email_attempts: self.send_email_attempts,
            send_global_attempts: self.send_global_attempts,
            send_metric_name: "yang_system_change_email_total",
            verify_metric_name: "yang_system_change_email_verify_total",
        }
    }

    /// 转换为登录 MFA 备用邮箱验证码引擎的运行时配置（独立 key 域与指标名）。
    pub fn mfa_engine_config(&self) -> EmailVerificationConfig {
        EmailVerificationConfig {
            redis_key_prefix: format!("yang-system:{}:mfa-email", self.namespace),
            secret: self.secret.clone(),
            ttl_seconds: self.ttl_seconds,
            resend_cooldown_seconds: self.resend_cooldown_seconds,
            max_attempts: self.max_attempts,
            code_digits: 6,
            send_window_seconds: self.send_window_seconds,
            send_ip_attempts: self.send_ip_attempts,
            send_email_attempts: self.send_email_attempts,
            send_global_attempts: self.send_global_attempts,
            send_metric_name: "yang_system_mfa_email_total",
            verify_metric_name: "yang_system_mfa_email_verify_total",
        }
    }

    /// 转换为邮箱验证码免密登录引擎的运行时配置（独立 key 域与指标名）。
    pub fn login_engine_config(&self) -> EmailVerificationConfig {
        EmailVerificationConfig {
            redis_key_prefix: format!("yang-system:{}:login-email", self.namespace),
            secret: self.secret.clone(),
            ttl_seconds: self.ttl_seconds,
            resend_cooldown_seconds: self.resend_cooldown_seconds,
            max_attempts: self.max_attempts,
            code_digits: 6,
            send_window_seconds: self.send_window_seconds,
            send_ip_attempts: self.send_ip_attempts,
            send_email_attempts: self.send_email_attempts,
            send_global_attempts: self.send_global_attempts,
            send_metric_name: "yang_system_login_email_total",
            verify_metric_name: "yang_system_login_email_verify_total",
        }
    }

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

impl PasswordResetEmailSettings {
    /// 转换为密码重置邮件链接的运行时配置（经 Tools config 槽注入）。
    pub fn link_config(&self) -> crate::addon::account::email_delivery::PasswordResetLinkConfig {
        crate::addon::account::email_delivery::PasswordResetLinkConfig {
            base_url: self.link_base_url.trim().to_string(),
        }
    }

    fn validate(&self, environment: DeploymentEnvironment) -> anyhow::Result<()> {
        let value = self.link_base_url.trim();
        let Some((scheme, authority)) = value.split_once("://") else {
            bail!("email.password_reset.link_base_url 必须包含 scheme（如 https://console.example.com）");
        };
        match scheme {
            "https" => {}
            "http" if environment != DeploymentEnvironment::Production => {}
            _ => bail!(
                "email.password_reset.link_base_url 必须使用 https（仅开发与测试环境允许 http）"
            ),
        }
        let authority_valid = !authority.is_empty()
            && authority.len() <= 253
            && authority
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':'));
        if !authority_valid {
            bail!(
                "email.password_reset.link_base_url 只能是 host[:port]，不允许路径、查询串、用户信息或非 ASCII 字符"
            );
        }
        if let Some((_, port)) = authority.rsplit_once(':') {
            let port_valid = !port.is_empty()
                && port.len() <= 5
                && port.bytes().all(|byte| byte.is_ascii_digit())
                && port.parse::<u16>().is_ok_and(|port| port > 0);
            if !port_valid {
                bail!("email.password_reset.link_base_url 的端口必须是 1..=65535");
            }
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
    /// 转换为框架认证限流器的运行时配置；Redis key 前缀与指标名保持既有契约。
    pub fn rate_limit_config(&self) -> AuthRateLimitConfig {
        AuthRateLimitConfig {
            window_seconds: self.auth_rate_limit_window_seconds,
            ip_attempts: self.auth_rate_limit_ip_attempts,
            username_attempts: self.auth_rate_limit_username_attempts,
            key_prefix: "yang-system".to_string(),
            metric_name: "yang_system_auth_rate_limit_total",
        }
    }

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
        if let Some(totp) = &self.totp {
            validate_totp_settings(totp)?;
        }
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

/// 判断（已 trim + 小写化的）密钥是否为示例/占位值。
///
/// 采用前缀式判定而非手工清单：`config.example.toml` / `config.show.toml` 里
/// 的占位值统一以 `replace-with-` / `replace_with_` 开头，前缀规则能在新增占位
/// 值时自动生效，不再需要同步维护一份易漏的精确匹配名单（历史缺陷：清单只覆盖
/// 了 6 个值中的 2 个，`step_up` / `email.change` / `email.mfa` / `email.login`
/// 的占位密钥以及 TOTP AEAD 占位密钥都能通过启动校验）。
fn is_placeholder_secret(normalized: &str) -> bool {
    const KNOWN: [&str; 3] = ["changeme", "replace-me", "example-secret"];
    KNOWN.contains(&normalized)
        || normalized.starts_with("replace-with")
        || normalized.starts_with("replace_with")
        || normalized.contains("placeholder")
}

fn validate_token_secret(secret: &str) -> anyhow::Result<()> {
    if secret.len() < 32 {
        bail!("token key secret 至少需要 32 字节");
    }
    let normalized = secret.trim().to_ascii_lowercase();
    let repeated_byte = secret
        .as_bytes()
        .first()
        .is_some_and(|first| secret.as_bytes().iter().all(|byte| byte == first));
    if is_placeholder_secret(&normalized) || repeated_byte {
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

fn validate_totp_settings(totp: &TotpSettings) -> anyhow::Result<()> {
    if !(6..=8).contains(&totp.digits) {
        bail!("security.totp.digits 必须在 6..=8 范围内");
    }
    if totp.aead_key.len() < 32 {
        bail!("security.totp.aead_key 至少需要 32 字节");
    }
    let normalized = totp.aead_key.trim().to_ascii_lowercase();
    if is_placeholder_secret(&normalized) {
        bail!("security.totp.aead_key 不能使用示例值或占位值");
    }
    let repeated_byte = totp
        .aead_key
        .as_bytes()
        .first()
        .is_some_and(|first| totp.aead_key.as_bytes().iter().all(|byte| byte == first));
    if repeated_byte {
        bail!("security.totp.aead_key 不能使用重复字符");
    }
    Ok(())
}

/// 邮箱换绑验证码的独立 config 槽类型。
///
/// `Tools` 的 config 按具体 Rust 类型索引，注册验证码与换绑验证码都是
/// [`EmailVerificationConfig`] 实例，必须用 distinct 类型区分两个槽。
#[derive(Clone, Debug)]
pub struct ChangeEmailVerificationConfig(pub EmailVerificationConfig);

impl ChangeEmailVerificationConfig {
    /// 取内部框架配置。
    pub fn engine_config(&self) -> &EmailVerificationConfig {
        &self.0
    }
}

/// 登录 MFA 备用邮箱验证码的独立 config 槽类型（与注册/换绑验证码隔离）。
#[derive(Clone, Debug)]
pub struct MfaEmailVerificationConfig(pub EmailVerificationConfig);

impl MfaEmailVerificationConfig {
    /// 取内部框架配置。
    pub fn engine_config(&self) -> &EmailVerificationConfig {
        &self.0
    }
}

/// 邮箱验证码免密登录的独立 config 槽类型（与注册/换绑/MFA 验证码隔离）。
#[derive(Clone, Debug)]
pub struct LoginEmailVerificationConfig(pub EmailVerificationConfig);

impl LoginEmailVerificationConfig {
    /// 取内部框架配置。
    pub fn engine_config(&self) -> &EmailVerificationConfig {
        &self.0
    }
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
[email.password_reset]
link_base_url = "http://localhost:5273"
[security]
argon2_max_concurrency = 4
auth_rate_limit_window_seconds = 60
auth_rate_limit_ip_attempts = 30
auth_rate_limit_username_attempts = 10
issue_refresh_credential_version = true
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

    /// 最小配置：只填环境事实与密钥，其余全部由内置默认值与派生规则承接。
    #[test]
    fn minimal_config_uses_safe_defaults_and_derives_namespaces() {
        let raw = r#"
[app]
environment = "development"
[authorization]
deployment = "test-local"
[mysql]
url = "mysql://config-user:config-password@config-mysql/config-database"
[redis]
url = "redis://config-redis/3"
[token]
active_key_id = "test-2026-07"
active_secret = "0123456789abcdef0123456789abcdef"
[step_up]
active_key_id = "step-up-test-2026-07"
active_secret = "step-up-0123456789abcdef0123456789abcdef"
[email.smtp]
relay = "smtp.example.test"
username = "test-smtp-user"
password = "test-smtp-password"
from_address = "no-reply@example.test"
from_name = "YANG Test"
[email.verification]
secret = "email-verification-0123456789abcdef0123456789abcdef"
[email.change]
secret = "change-email-0123456789abcdef0123456789abcdef"
[email.mfa]
secret = "mfa-email-0123456789abcdef0123456789abcdef"
[email.login]
secret = "login-email-0123456789abcdef0123456789abcdef"
[email.password_reset]
link_base_url = "http://localhost:5273"
"#;
        let settings =
            Settings::parse(raw).unwrap_or_else(|error| panic!("最小配置应解析成功: {error:#}"));

        assert_eq!(settings.app.name, "yang-system");
        assert_eq!(settings.http.bind, "127.0.0.1:8080");
        assert_eq!(settings.http.max_body_bytes, 1_048_576);
        assert_eq!(settings.mysql.max_connections, 20);
        assert_eq!(settings.mysql.max_lifetime_seconds, Some(1800));
        assert_eq!(settings.redis.wait_timeout_seconds, 10);
        assert_eq!(settings.token.issuer, "yang-system");
        assert_eq!(settings.token.audience, "yang-system-api");
        assert_eq!(settings.token.access_ttl_seconds, 3600);
        assert_eq!(settings.step_up.issuer, "yang-system-step-up");
        assert_eq!(settings.step_up.challenge_ttl_seconds, 120);
        assert_eq!(settings.email.smtp.port, 587);
        for (section, engine) in [
            ("verification", settings.email.verification.engine_config()),
            (
                "change",
                settings
                    .email
                    .change
                    .as_ref()
                    .unwrap_or_else(|| panic!("email.change 段应存在"))
                    .change_engine_config(),
            ),
            (
                "mfa",
                settings
                    .email
                    .mfa
                    .as_ref()
                    .unwrap_or_else(|| panic!("email.mfa 段应存在"))
                    .mfa_engine_config(),
            ),
            (
                "login",
                settings
                    .email
                    .login
                    .as_ref()
                    .unwrap_or_else(|| panic!("email.login 段应存在"))
                    .login_engine_config(),
            ),
        ] {
            assert_eq!(engine.ttl_seconds, 600, "{section} ttl 应取默认值");
            assert_eq!(
                engine.send_global_attempts, 1000,
                "{section} 全局额度应取默认值"
            );
        }
        // 四类验证码命名空间缺省继承 authorization.deployment，且 key 域相互隔离。
        assert_eq!(
            settings.email.verification.engine_config().redis_key_prefix,
            "yang-system:test-local:registration-email"
        );
        assert_eq!(
            settings
                .email
                .change
                .as_ref()
                .unwrap_or_else(|| panic!("email.change 段应存在"))
                .change_engine_config()
                .redis_key_prefix,
            "yang-system:test-local:change-email"
        );
        assert_eq!(
            settings
                .email
                .login
                .as_ref()
                .unwrap_or_else(|| panic!("email.login 段应存在"))
                .login_engine_config()
                .redis_key_prefix,
            "yang-system:test-local:login-email"
        );
        assert_eq!(settings.security.argon2_max_concurrency, 4);
        assert_eq!(settings.security.auth_rate_limit_ip_attempts, 30);
        assert!(!settings.security.issue_refresh_credential_version);
        assert_eq!(settings.logging.filter, "yang_system=info,tower_http=info");
        assert_eq!(settings.shutdown.total_timeout_seconds, 30);
    }

    /// config.show.toml 是全量配置参考：必须能反序列化为 Settings（字段名与结构同步），
    /// 且其中所有显式写出的值必须与代码内置默认值一致（占位密钥除外）。
    #[test]
    fn show_config_stays_in_sync_with_settings_schema_and_defaults() {
        let raw = include_str!("../../config.show.toml");
        let shown: Settings = crate::config::source::parse_file_only(raw)
            .unwrap_or_else(|error| panic!("config.show.toml 必须符合 Settings 结构: {error:#}"));

        // 把 show 文件中的占位密钥替换为合法值后必须能通过完整启动校验，
        // 证明参考文件里的默认值组合本身是可启动的。
        let launchable = raw
            .replace(
                "replace-with-at-least-32-random-bytes",
                "token-secret-0123456789abcdef0123456789abcdef",
            )
            .replace(
                "replace-with-independent-step-up-secret",
                "step-up-secret-0123456789abcdef0123456789abc",
            )
            .replace(
                "replace-with-independent-email-verification-secret",
                "verification-0123456789abcdef0123456789abcdef",
            )
            .replace(
                "replace-with-independent-change-email-secret",
                "change-email-0123456789abcdef0123456789abcde",
            )
            .replace(
                "replace-with-independent-mfa-email-secret",
                "mfa-email-0123456789abcdef0123456789abcdef0",
            )
            .replace(
                "replace-with-independent-login-email-secret",
                "login-email-0123456789abcdef0123456789abcdef",
            )
            .replace(
                "replace-with-an-independent-32-byte-totp-aead-key",
                "totp-aead-0123456789abcdef0123456789abcdef0",
            )
            .replace("replace-with-smtp-username", "show-smtp-user")
            .replace("replace-with-smtp-password", "show-smtp-password");
        Settings::parse(&launchable)
            .unwrap_or_else(|error| panic!("config.show.toml 替换占位值后应可启动: {error:#}"));

        // 与最小配置（全部走默认值）逐项比对，证明注释中标注的默认值真实。
        let minimal = Settings::parse(
            &launchable
                .lines()
                .filter(|line| {
                    let trimmed = line.trim();
                    !trimmed.starts_with("max_body_bytes")
                        && !trimmed.starts_with("request_timeout_seconds")
                        && !trimmed.starts_with("max_concurrency")
                        && !trimmed.starts_with("min_connections")
                        && !trimmed.starts_with("connect_timeout_seconds")
                        && !trimmed.starts_with("idle_timeout_seconds")
                        && !trimmed.starts_with("max_lifetime_seconds")
                        && !trimmed.starts_with("test_before_acquire")
                        && !trimmed.starts_with("wait_timeout_seconds")
                        && !trimmed.starts_with("outbox_")
                        && !trimmed.starts_with("issuer")
                        && !trimmed.starts_with("audience")
                        && !trimmed.starts_with("access_ttl_seconds")
                        && !trimmed.starts_with("refresh_ttl_seconds")
                        && !trimmed.starts_with("challenge_ttl_seconds")
                        && !trimmed.starts_with("proof_ttl_seconds")
                        && !trimmed.starts_with("port")
                        && !trimmed.starts_with("timeout_seconds")
                        && !trimmed.starts_with("namespace")
                        && !trimmed.starts_with("ttl_seconds")
                        && !trimmed.starts_with("resend_cooldown_seconds")
                        && !trimmed.starts_with("max_attempts")
                        && !trimmed.starts_with("send_")
                        && !trimmed.starts_with("argon2_max_concurrency")
                        && !trimmed.starts_with("auth_rate_limit_")
                        && !trimmed.starts_with("password_reset_ttl_seconds")
                        && !trimmed.starts_with("trusted_proxy_cidrs")
                        && !trimmed.starts_with("digits")
                        && !trimmed.starts_with("total_timeout_seconds")
                        && !trimmed.starts_with("metrics_bind")
                        && !trimmed.starts_with("traces_")
                        && !trimmed.starts_with("readiness_budget_ms")
                        && !trimmed.starts_with("filter")
                        && !trimmed.starts_with("name =")
                        && !trimmed.starts_with("retiring_keys")
                        && !trimmed.starts_with("max_connections")
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap_or_else(|error| panic!("剥离默认值后的 show 配置应可解析: {error:#}"));

        assert_eq!(shown.app.name, minimal.app.name);
        assert_eq!(shown.http, minimal.http);
        assert_eq!(shown.authorization, minimal.authorization);
        assert_eq!(shown.token.issuer, minimal.token.issuer);
        assert_eq!(shown.token.audience, minimal.token.audience);
        assert_eq!(
            shown.token.access_ttl_seconds,
            minimal.token.access_ttl_seconds
        );
        assert_eq!(
            shown.token.refresh_ttl_seconds,
            minimal.token.refresh_ttl_seconds
        );
        assert_eq!(shown.step_up.issuer, minimal.step_up.issuer);
        assert_eq!(shown.step_up.audience, minimal.step_up.audience);
        assert_eq!(
            shown.step_up.challenge_ttl_seconds,
            minimal.step_up.challenge_ttl_seconds
        );
        assert_eq!(
            shown.step_up.proof_ttl_seconds,
            minimal.step_up.proof_ttl_seconds
        );
        assert_eq!(shown.email.smtp.port, minimal.email.smtp.port);
        assert_eq!(
            shown.email.smtp.timeout_seconds,
            minimal.email.smtp.timeout_seconds
        );
        assert_eq!(
            shown.email.verification.namespace,
            minimal.email.verification.namespace
        );
        assert_eq!(
            shown.email.verification.ttl_seconds,
            minimal.email.verification.ttl_seconds
        );
        assert_eq!(
            shown.email.verification.send_global_attempts,
            minimal.email.verification.send_global_attempts
        );
        // security/totp 含密钥字段，只比对非密钥的默认值字段。
        assert_eq!(
            shown.security.argon2_max_concurrency,
            minimal.security.argon2_max_concurrency
        );
        assert_eq!(
            shown.security.auth_rate_limit_window_seconds,
            minimal.security.auth_rate_limit_window_seconds
        );
        assert_eq!(
            shown.security.auth_rate_limit_ip_attempts,
            minimal.security.auth_rate_limit_ip_attempts
        );
        assert_eq!(
            shown.security.auth_rate_limit_username_attempts,
            minimal.security.auth_rate_limit_username_attempts
        );
        assert_eq!(
            shown.security.password_reset_ttl_seconds,
            minimal.security.password_reset_ttl_seconds
        );
        assert_eq!(
            shown.security.trusted_proxy_cidrs,
            minimal.security.trusted_proxy_cidrs
        );
        assert_eq!(
            shown.security.totp.as_ref().map(|totp| totp.digits),
            minimal.security.totp.as_ref().map(|totp| totp.digits)
        );
        assert_eq!(shown.shutdown, minimal.shutdown);
        assert_eq!(shown.logging.filter, minimal.logging.filter);
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
        assert!(settings.security.issue_refresh_credential_version);
        assert_eq!(settings.security.password_reset_ttl_seconds, 900);
        assert_eq!(
            settings.email.password_reset.link_base_url,
            "http://localhost:5273"
        );
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

    #[test]
    fn rejects_password_reset_link_base_url_outside_the_safe_shape() {
        for bad in [
            "",
            "console.example.com",
            "https://console.example.com/reset",
            "https://console.example.com?x=1",
            "https://user@console.example.com",
            "https://console.example.com:99999",
        ] {
            let mut settings = Settings::parse(valid_config())
                .unwrap_or_else(|error| panic!("测试配置应可解析: {error}"));
            settings.email.password_reset.link_base_url = bad.to_string();
            let error = match settings.validate() {
                Ok(()) => panic!("非法 link_base_url 必须拒绝: {bad:?}"),
                Err(error) => error,
            };
            assert!(
                error
                    .to_string()
                    .contains("email.password_reset.link_base_url"),
                "错误应指明配置项: {error}"
            );
        }

        // 生产环境强制 https；开发/测试环境允许 http。
        let mut production = Settings::parse(valid_config())
            .unwrap_or_else(|error| panic!("测试配置应可解析: {error}"));
        production.app.environment = DeploymentEnvironment::Production;
        production.observability.metrics_enabled = true;
        production.email.password_reset.link_base_url = "http://console.example.com".to_string();
        let error = match production.validate() {
            Ok(()) => panic!("生产环境的 http 重置链接必须拒绝"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("email.password_reset.link_base_url"));

        let mut production_https = Settings::parse(valid_config())
            .unwrap_or_else(|error| panic!("测试配置应可解析: {error}"));
        production_https.app.environment = DeploymentEnvironment::Production;
        production_https.observability.metrics_enabled = true;
        production_https.email.password_reset.link_base_url =
            "https://console.example.com:8443".to_string();
        assert!(production_https.validate().is_ok());
    }

    /// MFA 备用邮箱验证码配置段：独立 key 域/指标名，密钥跨域复用一律拒绝。
    #[test]
    fn mfa_email_verification_section_is_isolated_and_cross_checked() {
        let with_mfa = |secret: &str| {
            format!(
                "{}\n[email.mfa]\nnamespace = \"test-local\"\nsecret = \"{secret}\"\nttl_seconds = 600\nresend_cooldown_seconds = 60\nmax_attempts = 5\nsend_window_seconds = 3600\nsend_ip_attempts = 20\nsend_email_attempts = 5\nsend_global_attempts = 1000\n",
                valid_config()
            )
        };
        // 合法独立密钥：解析成功且引擎配置使用独立 key 域与指标名。
        let settings = Settings::parse(&with_mfa("mfa-email-0123456789abcdef0123456789abcdef"))
            .unwrap_or_else(|error| panic!("合法 email.mfa 配置应解析成功: {error}"));
        let engine = settings
            .email
            .mfa
            .as_ref()
            .unwrap_or_else(|| panic!("email.mfa 段应解析存在"))
            .mfa_engine_config();
        assert_eq!(engine.redis_key_prefix, "yang-system:test-local:mfa-email");
        assert_eq!(engine.send_metric_name, "yang_system_mfa_email_total");
        assert_eq!(
            engine.verify_metric_name,
            "yang_system_mfa_email_verify_total"
        );

        // 复用注册验证码密钥 / Token 密钥 → 拒绝。
        for reused in [
            "email-verification-0123456789abcdef0123456789abcdef",
            "0123456789abcdef0123456789abcdef",
        ] {
            let error = match Settings::parse(&with_mfa(reused)) {
                Ok(_) => panic!("email.mfa 复用其他密钥域必须拒绝: {reused}"),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains("email.mfa.secret"),
                "错误应指明配置项: {error}"
            );
        }

        // 复用 TOTP AEAD 密钥 → 拒绝（跨段校验在 Settings::validate 顶层）。
        let mut settings = Settings::parse(&with_mfa("totp-aead-0123456789abcdef0123456789abcdef"))
            .unwrap_or_else(|error| panic!("测试配置应可解析: {error}"));
        settings.security.totp = Some(TotpSettings {
            aead_key: "totp-aead-0123456789abcdef0123456789abcdef".to_string(),
            digits: 6,
        });
        let error = match settings.validate() {
            Ok(()) => panic!("email.mfa 复用 TOTP AEAD 密钥必须拒绝"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("email.mfa.secret"),
            "错误应指明配置项: {error}"
        );
    }

    /// 免密登录邮箱验证码配置段：独立 key 域/指标名，密钥跨域复用一律拒绝。
    #[test]
    fn login_email_verification_section_is_isolated_and_cross_checked() {
        let with_login = |secret: &str| {
            format!(
                "{}\n[email.login]\nnamespace = \"test-local\"\nsecret = \"{secret}\"\nttl_seconds = 600\nresend_cooldown_seconds = 60\nmax_attempts = 5\nsend_window_seconds = 3600\nsend_ip_attempts = 20\nsend_email_attempts = 5\nsend_global_attempts = 1000\n",
                valid_config()
            )
        };
        // 合法独立密钥：解析成功且引擎配置使用独立 key 域与指标名。
        let settings = Settings::parse(&with_login("login-email-0123456789abcdef0123456789abcdef"))
            .unwrap_or_else(|error| panic!("合法 email.login 配置应解析成功: {error}"));
        let engine = settings
            .email
            .login
            .as_ref()
            .unwrap_or_else(|| panic!("email.login 段应解析存在"))
            .login_engine_config();
        assert_eq!(
            engine.redis_key_prefix,
            "yang-system:test-local:login-email"
        );
        assert_eq!(engine.send_metric_name, "yang_system_login_email_total");
        assert_eq!(
            engine.verify_metric_name,
            "yang_system_login_email_verify_total"
        );
        // config 槽 newtype 必须能包裹引擎配置并原样取回。
        let slot = crate::config::LoginEmailVerificationConfig(engine);
        assert_eq!(
            slot.engine_config().redis_key_prefix,
            "yang-system:test-local:login-email"
        );

        // 复用注册验证码密钥 / Token 密钥 → 拒绝。
        for reused in [
            "email-verification-0123456789abcdef0123456789abcdef",
            "0123456789abcdef0123456789abcdef",
        ] {
            let error = match Settings::parse(&with_login(reused)) {
                Ok(_) => panic!("email.login 复用其他密钥域必须拒绝: {reused}"),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains("email.login.secret"),
                "错误应指明配置项: {error}"
            );
        }

        // 复用换绑/MFA 验证码密钥 → 拒绝。
        let mut settings =
            Settings::parse(&with_login("login-email-0123456789abcdef0123456789abcdef"))
                .unwrap_or_else(|error| panic!("测试配置应可解析: {error}"));
        settings.email.change = Some(EmailVerificationSettings {
            namespace: "test-local".to_string(),
            secret: "login-email-0123456789abcdef0123456789abcdef".to_string(),
            ttl_seconds: 600,
            resend_cooldown_seconds: 60,
            max_attempts: 5,
            send_window_seconds: 3600,
            send_ip_attempts: 20,
            send_email_attempts: 5,
            send_global_attempts: 1000,
        });
        let error = match settings.validate() {
            Ok(()) => panic!("email.login 复用换绑验证码密钥必须拒绝"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("email.login.secret"),
            "错误应指明配置项: {error}"
        );

        // 复用 TOTP AEAD 密钥 → 拒绝（跨段校验在 Settings::validate 顶层）。
        let mut settings =
            Settings::parse(&with_login("totp-aead-0123456789abcdef0123456789abcdef"))
                .unwrap_or_else(|error| panic!("测试配置应可解析: {error}"));
        settings.security.totp = Some(TotpSettings {
            aead_key: "totp-aead-0123456789abcdef0123456789abcdef".to_string(),
            digits: 6,
        });
        let error = match settings.validate() {
            Ok(()) => panic!("email.login 复用 TOTP AEAD 密钥必须拒绝"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("email.login.secret"),
            "错误应指明配置项: {error}"
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
                &yang_base::action!("account.user.change_password"),
                "users:42:password",
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
            .replace("metrics_enabled = false", "metrics_enabled = true")
            .replace(
                "link_base_url = \"http://localhost:5273\"",
                "link_base_url = \"https://console.example.test\"",
            );
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
        let disabled = valid_config()
            .replace(
                "environment = \"development\"",
                "environment = \"production\"",
            )
            .replace(
                "link_base_url = \"http://localhost:5273\"",
                "link_base_url = \"https://console.example.test\"",
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
                .get("observability")
                .and_then(|observability| observability.get("metrics_enabled"))
                .and_then(toml::Value::as_bool),
            Some(true)
        );
        // 示例配置只保留必填项：调优键一律不出现，由代码内默认值承接。
        let mysql = value
            .get("mysql")
            .and_then(toml::Value::as_table)
            .unwrap_or_else(|| panic!("示例配置必须包含 [mysql] 段"));
        assert_eq!(mysql.len(), 1, "[mysql] 段只应保留 url: {mysql:?}");
        assert!(mysql.get("url").is_some());
        for section in ["verification", "change", "mfa", "login"] {
            let email_code = value
                .get("email")
                .and_then(|email| email.get(section))
                .and_then(toml::Value::as_table)
                .unwrap_or_else(|| panic!("示例配置必须包含 [email.{section}] 段"));
            assert_eq!(
                email_code.len(),
                1,
                "[email.{section}] 段只应保留 secret: {email_code:?}"
            );
            assert!(email_code.get("secret").is_some());
        }
        assert!(value.get("shutdown").is_none());
        assert!(value.get("logging").is_none());
        assert!(value
            .get("security")
            .and_then(|security| security.get("trusted_proxy_cidrs"))
            .is_none());
        assert_eq!(
            value
                .get("security")
                .and_then(|security| security.get("issue_refresh_credential_version"))
                .and_then(toml::Value::as_bool),
            Some(true)
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

    /// 示例配置里出现的**每一个**占位值都必须被密钥校验拒绝。
    ///
    /// AGENTS.md 承诺「config.example.toml 中的占位密钥会被启动校验拒绝」。本测试
    /// 直接从随仓库发布的示例/展示配置里抽取占位值逐一验证，因此新增占位值而校验
    /// 规则没跟上时会立刻失败（历史缺陷：`step_up` / `email.change` / `email.mfa` /
    /// `email.login` 的占位密钥与 TOTP AEAD 占位密钥都能通过启动校验）。
    #[test]
    fn every_shipped_placeholder_secret_is_rejected() {
        let mut checked = 0usize;
        for raw in [
            include_str!("../../config.example.toml"),
            include_str!("../../config.show.toml"),
        ] {
            for line in raw.lines() {
                let Some((_, value)) = line.split_once('=') else {
                    continue;
                };
                let value = value.trim().trim_matches('"');
                let normalized = value.trim().to_ascii_lowercase();
                if !(normalized.starts_with("replace-with")
                    || normalized.starts_with("replace_with"))
                {
                    continue;
                }
                checked += 1;
                assert!(
                    is_placeholder_secret(&normalized),
                    "示例配置中的占位值必须被判为占位: {value:?}"
                );
                // 长度达标的占位值必须真的被密钥校验拒绝；短于 32 字节的由长度规则拦下。
                if value.len() >= 32 {
                    assert!(
                        validate_token_secret(value).is_err(),
                        "占位密钥必须被 validate_token_secret 拒绝: {value:?}"
                    );
                }
            }
        }
        assert!(checked >= 8, "示例配置中的占位值数量异常: {checked}");
    }

    /// 逐台撤销的 jti 黑名单必须覆盖任何合法 refresh token 的完整寿命。
    ///
    /// 回归：黑名单曾硬编码 7 天，而 refresh token 有效期默认 30 天、上限 90 天，
    /// 被踢设备在第 7 天后可凭原 refresh cookie 重新轮换出新令牌对（撤销静默失效）。
    #[test]
    fn revocation_blacklist_outlives_every_valid_refresh_token() {
        let raw = valid_config().replace(
            "refresh_ttl_seconds = 120",
            &format!("refresh_ttl_seconds = {MAX_REFRESH_TTL_SECONDS}"),
        );
        let settings = match Settings::parse(&raw) {
            Ok(settings) => settings,
            Err(error) => panic!("取 refresh TTL 上限的配置必须可解析: {error:#}"),
        };
        assert!(
            REVOCATION_BLACKLIST_TTL_SECONDS >= settings.token.refresh_ttl_seconds,
            "黑名单 TTL({REVOCATION_BLACKLIST_TTL_SECONDS}) 必须覆盖 refresh TTL({})",
            settings.token.refresh_ttl_seconds
        );
        // 上限之上必须被拒绝，否则「黑名单覆盖任何合法令牌」的论证不成立。
        let over = valid_config().replace(
            "refresh_ttl_seconds = 120",
            &format!("refresh_ttl_seconds = {}", MAX_REFRESH_TTL_SECONDS + 1),
        );
        assert!(
            Settings::parse(&over).is_err(),
            "超过上限的 refresh TTL 必须被拒绝"
        );
    }

    /// 两个随仓库发布的配置必须**原样**无法启动。
    #[test]
    fn shipped_configs_cannot_boot_verbatim() {
        for (name, raw) in [
            (
                "config.example.toml",
                include_str!("../../config.example.toml"),
            ),
            ("config.show.toml", include_str!("../../config.show.toml")),
        ] {
            assert!(
                Settings::parse(raw).is_err(),
                "{name} 原样必须被启动校验拒绝（占位密钥不得放行）"
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

    /// 省略 `[feishu]` 段时必须能正常解析，且视为未启用。
    #[test]
    fn feishu_section_is_optional_and_absent_by_default() {
        let settings = Settings::parse(valid_config())
            .unwrap_or_else(|error| panic!("基准配置应解析成功: {error:#}"));
        assert!(
            settings.feishu.is_none(),
            "[feishu] 缺席时应为 None，而不是启用一个空配置"
        );
    }

    /// 占位密钥必须被拒绝，防止示例值被原样部署。
    #[test]
    fn feishu_rejects_placeholder_secrets() {
        let raw = valid_config().to_string()
            + "\n[feishu]\nenabled = true\nmanagement_api_token = \"replace-with-feishu-management-token\"\n";
        assert!(
            Settings::parse(&raw).is_err(),
            "占位 management_api_token 必须被拒绝"
        );
    }

    /// 密钥域隔离：飞书的密钥不得复用 Token / Step-up 的密钥。
    #[test]
    fn feishu_secrets_must_not_reuse_other_key_domains() {
        // 复用 Token 的活动密钥
        let reused_token = valid_config().to_string()
            + "\n[feishu]\nenabled = true\nmanagement_api_token = \"0123456789abcdef0123456789abcdef\"\n";
        let error = match Settings::parse(&reused_token) {
            Ok(_) => panic!("management_api_token 复用 Token 密钥必须被拒绝"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("feishu"),
            "报错应指明飞书段，实际: {error:#}"
        );

        // 复用 Step-up 的活动密钥
        let reused_step_up = valid_config().to_string()
            + "\n[feishu]\nenabled = true\nmanagement_api_token = \"a-real-management-token-value-1234\"\nencryption_key = \"step-up-0123456789abcdef0123456789abcdef\"\n";
        let error = match Settings::parse(&reused_step_up) {
            Ok(_) => panic!("encryption_key 复用 Step-up 密钥必须被拒绝"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("feishu"),
            "报错应指明飞书段，实际: {error:#}"
        );
    }

    /// `is_usable` 是路由注册的开关：关闭或 Token 为空时不得注册对外端点。
    #[test]
    fn feishu_is_usable_requires_enabled_and_non_blank_token() {
        let parse_feishu =
            |extra: &str| match Settings::parse(&(valid_config().to_string() + extra)) {
                Ok(settings) => match settings.feishu {
                    Some(feishu) => feishu,
                    None => panic!("[feishu] 段应被解析出来"),
                },
                Err(error) => panic!("配置应解析成功: {error:#}"),
            };

        let disabled = parse_feishu(
            "\n[feishu]\nenabled = false\nmanagement_api_token = \"a-real-management-token-value-1234\"\n",
        );
        assert!(!disabled.is_usable(), "enabled=false 时不可用");

        let blank = parse_feishu("\n[feishu]\nenabled = true\nmanagement_api_token = \"   \"\n");
        assert!(!blank.is_usable(), "Token 为空白时不可用");

        let enabled = parse_feishu(
            "\n[feishu]\nenabled = true\nmanagement_api_token = \"a-real-management-token-value-1234\"\n",
        );
        assert!(enabled.is_usable(), "enabled 且 Token 非空时可用");
        assert!(
            enabled.encryption_key.is_none(),
            "未配置 Key 时应为 None（表示明文返回）"
        );
    }

    /// 出站凭证必须能被解析。
    ///
    /// 这条是**部署能否起机**的守卫：`FeishuSettings` 开了 `deny_unknown_fields`，
    /// 而 `deploy/config.cloud.toml` 的 `[feishu]` 段里已经写了 `app_id` /
    /// `app_secret`（且 `enabled = true`）。在该结构体加上这两个字段之前，那份配置
    /// 会在**反序列化阶段**直接失败——比 validate 更早，且报的是 unknown field。
    #[test]
    fn feishu_section_accepts_outbound_credentials() {
        let settings = Settings::parse(
            &(valid_config().to_string()
                + "\n[feishu]\nenabled = true\n\
                   management_api_token = \"a-real-management-token-value-1234\"\n\
                   app_id = \"cli_a1b2c3d4e5f6g7h8\"\n\
                   app_secret = \"dskLLdkasdjlasdKK0123456789abcdef\"\n\
                   pull_interval_seconds = 600\n"),
        )
        .unwrap_or_else(|error| panic!("含出站凭证的 [feishu] 段必须可解析: {error:#}"));

        let feishu = settings
            .feishu
            .unwrap_or_else(|| panic!("[feishu] 段应被解析出来"));
        assert_eq!(feishu.app_id.as_deref(), Some("cli_a1b2c3d4e5f6g7h8"));
        assert_eq!(
            feishu.app_secret.as_deref(),
            Some("dskLLdkasdjlasdKK0123456789abcdef")
        );
        assert_eq!(feishu.pull_interval_seconds, 600);
    }

    /// `pull_interval_seconds` 省略时取默认值（900 秒）。
    #[test]
    fn feishu_pull_interval_defaults_to_fifteen_minutes() {
        let settings = Settings::parse(
            &(valid_config().to_string()
                + "\n[feishu]\nenabled = true\nmanagement_api_token = \"a-real-management-token-value-1234\"\n"),
        )
        .unwrap_or_else(|error| panic!("配置应解析成功: {error:#}"));
        let feishu = settings
            .feishu
            .unwrap_or_else(|| panic!("[feishu] 段应被解析出来"));
        assert_eq!(feishu.pull_interval_seconds, 900);
    }

    /// 出站可用性与入站可用性**必须相互独立**。
    ///
    /// 把出站凭证并进 `is_usable()` 会改变入站路由的注册条件——一次「还没配 app_id」
    /// 的滚动发布会把已经在跑的入站端点一起摘掉。
    #[test]
    fn feishu_can_pull_is_independent_of_is_usable() {
        let parse_feishu =
            |extra: &str| match Settings::parse(&(valid_config().to_string() + extra)) {
                Ok(settings) => match settings.feishu {
                    Some(feishu) => feishu,
                    None => panic!("[feishu] 段应被解析出来"),
                },
                Err(error) => panic!("配置应解析成功: {error:#}"),
            };

        // 只有入站凭证：入站可用、出站不可用
        let inbound_only = parse_feishu(
            "\n[feishu]\nenabled = true\nmanagement_api_token = \"a-real-management-token-value-1234\"\n",
        );
        assert!(inbound_only.is_usable(), "入站凭证齐备");
        assert!(
            !inbound_only.can_pull(),
            "没有 app_id/app_secret 就不能出站"
        );

        // 只有出站凭证：出站可用、入站不可用（management_api_token 有 serde default）
        let outbound_only = parse_feishu(
            "\n[feishu]\nenabled = true\n\
             app_id = \"cli_a1b2c3d4e5f6g7h8\"\n\
             app_secret = \"dskLLdkasdjlasdKK0123456789abcdef\"\n",
        );
        assert!(
            !outbound_only.is_usable(),
            "没有管理 Token 就不能注册入站路由"
        );
        assert!(outbound_only.can_pull(), "出站凭证齐备");

        // 段关闭时两者都不可用
        let disabled = parse_feishu(
            "\n[feishu]\nenabled = false\n\
             management_api_token = \"a-real-management-token-value-1234\"\n\
             app_id = \"cli_a1b2c3d4e5f6g7h8\"\n\
             app_secret = \"dskLLdkasdjlasdKK0123456789abcdef\"\n",
        );
        assert!(!disabled.is_usable() && !disabled.can_pull());
    }

    /// 占位与空白凭证一律视为**未配置**：worker 不得拿占位凭证反复出网。
    ///
    /// `CHANGE_ME_*` 必须被单独识别——`is_placeholder_secret` 只认 changeme /
    /// replace-with* / placeholder 三类，而部署模板用的正是带下划线的
    /// `CHANGE_ME_FEISHU_APP_ID`。
    #[test]
    fn feishu_can_pull_rejects_placeholder_and_blank_credentials() {
        let parse_feishu =
            |extra: &str| match Settings::parse(&(valid_config().to_string() + extra)) {
                Ok(settings) => match settings.feishu {
                    Some(feishu) => feishu,
                    None => panic!("[feishu] 段应被解析出来"),
                },
                Err(error) => panic!("配置应解析成功: {error:#}"),
            };

        for (app_id, app_secret) in [
            ("CHANGE_ME_FEISHU_APP_ID", "CHANGE_ME_FEISHU_APP_SECRET"),
            ("change_me_app", "change_me_secret"),
            ("replace-with-app-id", "replace-with-app-secret"),
            ("cli_real_looking", "   "),
            ("   ", "dskLLdkasdjlasdKK0123456789abcdef"),
        ] {
            let feishu = parse_feishu(&format!(
                "\n[feishu]\nenabled = true\napp_id = \"{app_id}\"\napp_secret = \"{app_secret}\"\n"
            ));
            assert!(
                !feishu.can_pull(),
                "占位/空白凭证不得开启出站: app_id={app_id:?} app_secret={app_secret:?}"
            );
        }
    }

    /// 轮询间隔越界必须被拒绝（段启用时）。
    #[test]
    fn feishu_rejects_out_of_range_pull_interval() {
        for interval in [0u64, 1, 9, 86_401, u64::MAX] {
            let raw = valid_config().to_string()
                + &format!(
                    "\n[feishu]\nenabled = true\n\
                     management_api_token = \"a-real-management-token-value-1234\"\n\
                     pull_interval_seconds = {interval}\n"
                );
            let error = match Settings::parse(&raw) {
                Ok(_) => panic!("pull_interval_seconds={interval} 必须被拒绝"),
                Err(error) => error,
            };
            assert!(
                format!("{error:#}").contains("pull_interval_seconds"),
                "报错必须定位到该字段: {error:#}"
            );
        }
    }

    /// 出站凭证同样受密钥域隔离约束。
    #[test]
    fn feishu_app_secret_must_not_reuse_other_key_domains() {
        let raw = valid_config().to_string()
            + "\n[feishu]\nenabled = true\n\
             app_id = \"cli_a1b2c3d4e5f6g7h8\"\n\
             app_secret = \"0123456789abcdef0123456789abcdef\"\n";
        let error = match Settings::parse(&raw) {
            Ok(_) => panic!("app_secret 复用 Token 密钥必须被拒绝"),
            Err(error) => error,
        };
        assert!(
            format!("{error:#}").contains("feishu.app_secret"),
            "报错应指明 feishu.app_secret，实际: {error:#}"
        );
    }

    /// 出站凭证**不得**套用我们自己签发密钥的强度规则。
    ///
    /// `validate_token_secret` 要求 ≥32 字节且非重复字符；飞书 app_secret 是**第三方**
    /// 凭证，套上去会把合法配置判非法，在 `[feishu]` 段上重新制造一次启动失败。
    #[test]
    fn feishu_app_secret_is_not_subject_to_our_key_strength_rules() {
        // 16 字节的短 secret：不经 validate_token_secret 就应通过
        let settings = Settings::parse(
            &(valid_config().to_string()
                + "\n[feishu]\nenabled = true\n\
                   app_id = \"cli_a1b2c3d4e5f6g7h8\"\n\
                   app_secret = \"short-but-real\"\n"),
        )
        .unwrap_or_else(|error| panic!("短的第三方凭证不该被密钥强度规则拒绝: {error:#}"));
        let feishu = settings
            .feishu
            .unwrap_or_else(|| panic!("[feishu] 段应被解析出来"));
        assert!(feishu.can_pull(), "非占位的短 secret 应视为已配置");
    }

    /// 告警默认值：**不配收件人 = 不告警**，阈值取 3。
    ///
    /// 「不配 = 不告警」是设计 D5 的选择，不是遗漏：告警要往哪儿发是部署方的决定，
    /// 我们不该默认往任何地址发信。
    #[test]
    fn feishu_alert_settings_default_to_silent() {
        let settings = Settings::parse(
            &(valid_config().to_string()
                + "\n[feishu]\nenabled = true\nmanagement_api_token = \"a-real-management-token-value-1234\"\n"),
        )
        .unwrap_or_else(|error| panic!("配置应解析成功: {error:#}"));
        let feishu = settings
            .feishu
            .unwrap_or_else(|| panic!("[feishu] 段应被解析出来"));
        assert!(
            feishu.alert_recipients.is_empty(),
            "默认不配收件人 = 不告警"
        );
        assert_eq!(feishu.alert_failure_threshold, 3);
    }

    /// 收件人逐项校验：空串与非法地址必须在**启动期**被拒。
    #[test]
    fn feishu_rejects_unusable_alert_recipients() {
        for entry in ["\"  \"", "\"not-an-email\"", "\"ops@example\""] {
            let raw = valid_config().to_string()
                + &format!(
                    "\n[feishu]\nenabled = true\n\
                     management_api_token = \"a-real-management-token-value-1234\"\n\
                     alert_recipients = [\"ops@example.com\", {entry}]\n"
                );
            let error = match Settings::parse(&raw) {
                Ok(_) => panic!("收件人 {entry} 必须被拒绝"),
                Err(error) => error,
            };
            assert!(
                format!("{error:#}").contains("alert_recipients"),
                "报错必须定位到该字段: {error:#}"
            );
        }
    }

    /// 收件人配全时按原样带出（顺序即投递顺序）。
    #[test]
    fn feishu_keeps_configured_alert_recipients() {
        let settings = Settings::parse(
            &(valid_config().to_string()
                + "\n[feishu]\nenabled = true\n\
                   management_api_token = \"a-real-management-token-value-1234\"\n\
                   alert_recipients = [\"ops@example.com\", \"oncall@example.com.cn\"]\n\
                   alert_failure_threshold = 5\n"),
        )
        .unwrap_or_else(|error| panic!("合法收件人应通过: {error:#}"));
        let feishu = settings
            .feishu
            .unwrap_or_else(|| panic!("[feishu] 段应被解析出来"));
        assert_eq!(
            feishu.alert_recipients,
            vec!["ops@example.com", "oncall@example.com.cn"]
        );
        assert_eq!(feishu.alert_failure_threshold, 5);
    }

    /// 阈值有下限：1 次失败不足以判定一张表坏了，允许等于把阈值机制关掉。
    #[test]
    fn feishu_rejects_out_of_range_alert_threshold() {
        for threshold in [0i64, 1, -1, 1_001] {
            let raw = valid_config().to_string()
                + &format!(
                    "\n[feishu]\nenabled = true\n\
                     management_api_token = \"a-real-management-token-value-1234\"\n\
                     alert_failure_threshold = {threshold}\n"
                );
            let error = match Settings::parse(&raw) {
                Ok(_) => panic!("alert_failure_threshold={threshold} 必须被拒绝"),
                Err(error) => error,
            };
            assert!(
                format!("{error:#}").contains("alert_failure_threshold"),
                "报错必须定位到该字段: {error:#}"
            );
        }
    }
}

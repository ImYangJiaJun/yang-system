//! 飞书**用户身份**（OAuth `user_access_token`）：授权 URL、state、令牌交换/刷新与单例行存取。
//!
//! # 为什么需要用户身份
//!
//! 姓名 → open_id 走的 `GET /open-apis/search/v1/user` **只接受** `user_access_token`
//! ——用 `tenant_access_token` 调必报 99991663（真机实测）。用户身份只能由管理员
//! 在控制台完成一次 OAuth 授权取得，凭证落在 `feishu_user_token`（单例行）。
//!
//! # 三条时钟
//!
//! - `access_token` 约 2 小时（**以响应里的 `expires_in` 为准**，官方明说非固定值）
//!   → 按需刷新，见 [`user_access_token`]；
//! - `refresh_token` **一次性**且寿命短（官方文档示例 7 天）→ 提醒 worker 每日
//!   无条件刷一次保活。只做「用到才刷」的话，静默期一长整条链就断了，而断链的
//!   唯一补救是让管理员重新授权；
//! - 飞书硬性规定：用户授权满 **365 天**后必须重新授权，继续刷新报 `20037`。
//!   刷新**不重置**这条时钟，只有完整重新授权才重置（见 `authorized_at`）。
//!
//! # 刷新必须串行
//!
//! `refresh_token` 一次性：两个并发刷新会让后一个拿到 `20073`，更糟的是把刚换到的
//! 新凭证写丢（响应丢了就再也拿不回来）。这里用**进程内**静态锁 + 锁后复查，与仓库
//! worker 的单实例假设一致；多实例部署要换成 Redis 锁（照 `TenantTokenCache`）。
//!
//! # state 为什么不落库
//!
//! state 一次性、10 分钟即废，且**失败是闭合的**：进程重启丢掉未消费的 state，
//! 管理员重新点一次「去授权」即可。为它引 Redis 或加列都不值。

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex as AsyncMutex;

use yang_base::table::Record;
use yang_base::BaseError;

use super::outbound::{
    send_with_retry, FailureKind, OutboundFailure, OutboundMethod, OutboundRequest,
    OutboundTransport, Sleeper, TOKEN_REQUEST_TIMEOUT_SECS, TOKEN_RETRY,
};
use super::repository::Repository;
use super::tenant_token::{FeishuCredentials, FEISHU_OPEN_BASE};
use crate::config::FeishuSettings;

/// 授权页（浏览器被重定向到这里）。
const AUTHORIZE_URL: &str = "https://accounts.feishu.cn/open-apis/authen/v1/authorize";

/// 令牌端点（RFC 6749 形态：`grant_type` + `client_id` + `client_secret` 在请求体里）。
///
/// 换 `code` 与刷 `refresh_token` 走**同一个** URL，只有 `grant_type` 不同。
const TOKEN_URL: &str = "https://accounts.feishu.cn/oauth/v3/token";

/// 授权页请求的 scope（空格分隔）。**每一个都必须在开发者后台「权限管理」里已开通**，
/// 否则授权页直接报 20027（拼了未开通的 scope）。
///
/// - `contact:user:search`：`GET /open-apis/search/v1/user` 要的**就是它**。
///   2026-10-13 真机取到：只授权 `contact:contact.base:readonly` 时该接口报 99991679，
///   `permission_violations` 点名 `{"type":"action_privilege_required",
///   "subject":"contact:user:search"}` —— 「通讯录基础信息只读」与「搜索用户」在飞书是
///   **两个独立权限**，前者不蕴含后者。
/// - `contact:contact.base:readonly`：保留。回调里 `authen/v1/user_info` 与后续可能用到的
///   通讯录基础读取走它，且它已在后台开通、已验证可授权，去掉只会平白多一次真机试错。
/// - `offline_access`：**不带上它，令牌响应里没有 `refresh_token`**，365 天内的
///   自动续期就无从谈起。
///
/// 再报缺 scope 时，照 99991679 / 99991672 返回的 `permission_violations` 在这里补。
pub(crate) const OAUTH_SCOPES: &str =
    "contact:user:search contact:contact.base:readonly offline_access";

/// state 的有效期（秒）。授权码本身 5 分钟有效，state 略长即可。
const STATE_TTL_SECONDS: i64 = 600;

/// access_token 剩余寿命低于这个秒数就提前刷新，避免「检查通过、调用时已过期」。
const ACCESS_TOKEN_REFRESH_MARGIN_SECONDS: i64 = 300;

/// 刷新 `refresh_token` 已过期（含 365 天红线触顶）。
const CODE_REFRESH_TOKEN_EXPIRED: i32 = 20037;
/// 刷新 `refresh_token` 无效。
const CODE_REFRESH_TOKEN_INVALID: i32 = 20026;
/// 刷新 `refresh_token` 已被撤销。
const CODE_REFRESH_TOKEN_REVOKED: i32 = 20064;
/// 刷新 `refresh_token` 已被使用（一次性）。
const CODE_REFRESH_TOKEN_USED: i32 = 20073;

/// 当前 unix 秒。秒级足够——三条时钟里最短的单位是分钟。
pub(crate) fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// 「该重新授权了」的提醒线（天）。
///
/// 飞书的硬性红线是 **365 天**：用户授权满 365 天后继续刷新报 20037，唯一补救是
/// 重新授权。提前 15 天开始提醒，给管理员留出反应时间；提醒**每天**发，直到重新
/// 授权（重新授权会重置 `authorized_at`，提醒自然停）。
pub(crate) const REAUTH_REMINDER_DAYS: i64 = 350;

/// 是否已过提醒线。
pub(crate) fn reauth_reminder_due(authorized_at: i64, now: i64) -> bool {
    now - authorized_at >= REAUTH_REMINDER_DAYS * 86_400
}

/// 一次 OAuth 流程所需的配置。
pub(crate) struct OAuthConfig {
    pub(crate) credentials: FeishuCredentials,
    pub(crate) redirect_uri: String,
}

/// 从集成配置里取 OAuth 配置；缺项时给出可行动文案。
///
/// 两个前提都要点明：没有 `app_id`/`app_secret` 换不到 token，没有
/// `oauth_redirect_uri` 连授权页都拼不出来（而它与开发者后台注册值必须逐字一致）。
pub(crate) fn oauth_config(settings: &FeishuSettings) -> Result<OAuthConfig, String> {
    if !settings.can_pull() {
        return Err("飞书出站凭证未配置（app_id / app_secret）".to_string());
    }
    let Some(redirect_uri) = settings
        .oauth_redirect_uri
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(
            "飞书用户身份授权未配置：请在 [feishu] 段设置 oauth_redirect_uri（须与开发者后台「安全设置 > 重定向 URL」注册值逐字一致）"
                .to_string(),
        );
    };
    Ok(OAuthConfig {
        credentials: FeishuCredentials {
            app_id: settings.app_id.clone().unwrap_or_default(),
            app_secret: settings.app_secret.clone().unwrap_or_default(),
        },
        redirect_uri: redirect_uri.to_string(),
    })
}

// ---------------------------------------------------------------------------
// 授权 URL 与 state
// ---------------------------------------------------------------------------

/// 按 RFC 3986 的 unreserved 集合做百分号编码。
///
/// 刻意不用 `serde_urlencoded`：它把空格编成 `+`，而官方文档给的 scope 例子是
/// `%20` 分隔。两者在标准解码下等价，但 scope 是**逐字**交给授权页的参数，
/// 不赌飞书侧的解码实现。
fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// 拼授权页 URL。管理员点「去授权」时打开它。
pub(crate) fn build_authorize_url(app_id: &str, redirect_uri: &str, state: &str) -> String {
    format!(
        "{AUTHORIZE_URL}?client_id={}&response_type=code&redirect_uri={}&scope={}&state={}&prompt=consent",
        percent_encode(app_id),
        percent_encode(redirect_uri),
        percent_encode(OAUTH_SCOPES),
        percent_encode(state),
    )
}

/// 已签发、尚未消费的 state（值 = 过期时刻的 unix 秒）。
static OAUTH_STATES: LazyLock<Mutex<HashMap<String, i64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 在锁内操作 state 表。中毒（持锁线程 panic）时取回内部值继续——这张表只装
/// 「随机串 → 过期时刻」，没有需要靠中毒保护的不变式。
fn with_states<T>(action: impl FnOnce(&mut HashMap<String, i64>) -> T) -> T {
    let mut states = OAUTH_STATES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    action(&mut states)
}

/// 签发一个新 state（128 位随机）并记下它的有效期。
pub(crate) fn mint_state(now: i64) -> String {
    let state = uuid::Uuid::new_v4().simple().to_string();
    with_states(|states| {
        // 顺手清掉过期的：这张表只在点「去授权」时写入，增长只可能来自
        // 「点了很多次都没走完授权」。
        states.retain(|_, expires_at| *expires_at > now);
        states.insert(state.clone(), now + STATE_TTL_SECONDS);
    });
    state
}

/// 校验并**消费**一个 state：命中即删（一次性），过期或未签发一律不认。
///
/// 这是官方明确要求的 CSRF 防护：没有它，攻击者可以把**自己的**授权码塞进回调，
/// 让系统的用户身份绑到攻击者的飞书账号上。
pub(crate) fn consume_state(state: &str, now: i64) -> bool {
    with_states(|states| match states.remove(state) {
        Some(expires_at) => expires_at > now,
        None => false,
    })
}

// ---------------------------------------------------------------------------
// 令牌端点
// ---------------------------------------------------------------------------

/// 令牌端点返回的原始报文。成功与失败共用一个结构：失败时只有 `code` 与
/// `error`/`error_description`。
#[derive(Debug, Deserialize)]
struct TokenResponse {
    code: i32,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    refresh_token_expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// 一对凭证 + 已算好的绝对过期时刻（unix 秒）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TokenBundle {
    pub(crate) access_token: String,
    pub(crate) access_token_expires_at: i64,
    pub(crate) refresh_token: String,
    pub(crate) refresh_token_expires_at: i64,
    pub(crate) scope: Option<String>,
}

/// 令牌端点/用户信息接口的失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TokenEndpointError {
    /// 飞书业务码；拿不到（网关错误页、传输层失败）时为 `0`。
    pub(crate) code: i32,
    pub(crate) message: String,
}

impl TokenEndpointError {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 只能靠**重新授权**补救的那一族：刷新凭证过期（含 365 天红线）、无效、
    /// 已撤销、已使用。
    pub(crate) fn needs_reauth(&self) -> bool {
        matches!(
            self.code,
            CODE_REFRESH_TOKEN_EXPIRED
                | CODE_REFRESH_TOKEN_INVALID
                | CODE_REFRESH_TOKEN_REVOKED
                | CODE_REFRESH_TOKEN_USED
        )
    }

    /// 已登记码的排查提示。文案的价值在于「码义不能望文生义」——`20074` 看着像
    /// 凭证问题，其实是后台开关没开。
    pub(crate) fn hint(&self) -> Option<&'static str> {
        match self.code {
            CODE_REFRESH_TOKEN_EXPIRED => {
                Some("刷新凭证已过期（用户授权满 365 天后飞书强制重新授权）")
            }
            CODE_REFRESH_TOKEN_INVALID | CODE_REFRESH_TOKEN_REVOKED | CODE_REFRESH_TOKEN_USED => {
                Some("刷新凭证已失效（一次性：用过即换新，旧值立即作废）")
            }
            20074 => {
                Some("开发者后台「安全设置 > 刷新 user_access_token」开关未开启（改动需发版生效）")
            }
            20071 => Some("redirect_uri 与开发者后台注册的重定向 URL 不一致"),
            20027 => Some("授权页 scope 含应用未开通的权限（去开发者后台开通后重试）"),
            20003 | 20065 => Some("授权码已被使用（一次性）"),
            20004 => Some("授权码已过期（有效期 5 分钟）"),
            20002 => Some("client_id 与 client_secret 不匹配"),
            20010 => Some("发起授权的用户没有该应用的使用权限"),
            _ => None,
        }
    }
}

impl From<&OutboundFailure> for TokenEndpointError {
    fn from(failure: &OutboundFailure) -> Self {
        let code = match failure.kind {
            FailureKind::Fatal { code } => code,
            _ => 0,
        };
        Self::new(code, failure.message.clone())
    }
}

/// 解析令牌响应；`code != 0` 折成错误。
fn parse_token_response(body: &str) -> Result<TokenResponse, TokenEndpointError> {
    let parsed: TokenResponse = serde_json::from_str(body)
        .map_err(|error| TokenEndpointError::new(0, format!("令牌端点响应解析失败: {error}")))?;
    if parsed.code != 0 {
        let message = parsed
            .error_description
            .clone()
            .or_else(|| parsed.error.clone())
            .unwrap_or_else(|| "令牌端点返回失败".to_string());
        return Err(TokenEndpointError::new(parsed.code, message));
    }
    Ok(parsed)
}

/// 令牌响应 → 落库用的凭证快照。
fn to_bundle(response: TokenResponse, now: i64) -> Result<TokenBundle, TokenEndpointError> {
    let access_token = response
        .access_token
        .ok_or_else(|| TokenEndpointError::new(0, "令牌响应里没有 access_token"))?;
    // `expires_in` 官方明说「非固定值，务必以响应为准」，缺了不能猜一个默认值。
    let expires_in = response
        .expires_in
        .ok_or_else(|| TokenEndpointError::new(0, "令牌响应里没有 expires_in"))?;
    let refresh_token = response.refresh_token.ok_or_else(|| {
        TokenEndpointError::new(
            0,
            "令牌响应里没有 refresh_token：授权链接的 scope 是否漏了 offline_access？",
        )
    })?;
    // 刷新凭证的寿命同样以响应为准；缺省时按 access_token 的寿命兜底（保守：宁可
    // 早刷一次，也不要拿着一个未知寿命的凭证等到断链）。
    let refresh_expires_in = response.refresh_token_expires_in.unwrap_or(expires_in);
    Ok(TokenBundle {
        access_token,
        access_token_expires_at: now + expires_in,
        refresh_token,
        refresh_token_expires_at: now + refresh_expires_in,
        scope: response.scope,
    })
}

/// 打令牌端点（换 code 与刷新共用）。
async fn request_tokens(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    body: serde_json::Value,
) -> Result<TokenResponse, TokenEndpointError> {
    let request = OutboundRequest {
        method: OutboundMethod::Post,
        url: TOKEN_URL.to_string(),
        query: Vec::new(),
        // v3 端点的凭证在**请求体**里（client_id/client_secret），不带 Authorization 头。
        bearer_token: None,
        // 端点两种请求体都收（`application/json` 是官方明说的兼容形态）。
        json_body: Some(body),
        raw_body: None,
        binary_limit: None,
        timeout_secs: Some(TOKEN_REQUEST_TIMEOUT_SECS),
        // **不重试**：`code` 与 `refresh_token` 都是一次性的。响应丢失时重发只会
        // 拿到「已使用」；刷新那条更糟——新凭证已经签发但响应丢了，重发拿不回来。
        idempotent: false,
    };
    let response = send_with_retry(transport, sleeper, &request, TOKEN_RETRY)
        .await
        .map_err(|failure| TokenEndpointError::from(&failure))?;
    parse_token_response(&response.body)
}

/// 用授权码换第一对凭证（授权回调专用）。
pub(crate) async fn exchange_code(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    credentials: &FeishuCredentials,
    code: &str,
    redirect_uri: &str,
    now: i64,
) -> Result<TokenBundle, TokenEndpointError> {
    let response = request_tokens(
        transport,
        sleeper,
        json!({
            "grant_type": "authorization_code",
            "client_id": credentials.app_id,
            "client_secret": credentials.app_secret,
            "code": code,
            // 必须与授权时传的那个**逐字一致**，否则 20071。
            "redirect_uri": redirect_uri,
        }),
    )
    .await?;
    to_bundle(response, now)
}

/// 用刷新凭证换新的一对（旧的 `refresh_token` 立即作废）。
pub(crate) async fn refresh_tokens(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    credentials: &FeishuCredentials,
    refresh_token: &str,
    now: i64,
) -> Result<TokenBundle, TokenEndpointError> {
    let response = request_tokens(
        transport,
        sleeper,
        json!({
            "grant_type": "refresh_token",
            "client_id": credentials.app_id,
            "client_secret": credentials.app_secret,
            "refresh_token": refresh_token,
        }),
    )
    .await?;
    to_bundle(response, now)
}

/// 取授权人的 open_id 与姓名（拿刚换到的 `user_access_token` 调）。
pub(crate) async fn fetch_user_info(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    access_token: &str,
) -> Result<(String, Option<String>), TokenEndpointError> {
    #[derive(Deserialize)]
    struct UserInfo {
        open_id: String,
        #[serde(default)]
        name: Option<String>,
    }
    #[derive(Deserialize)]
    struct Envelope {
        code: i32,
        #[serde(default)]
        msg: Option<String>,
        #[serde(default)]
        data: Option<UserInfo>,
    }

    let request = OutboundRequest {
        method: OutboundMethod::Get,
        url: format!("{FEISHU_OPEN_BASE}/open-apis/authen/v1/user_info"),
        query: Vec::new(),
        bearer_token: Some(access_token.to_string()),
        json_body: None,
        raw_body: None,
        binary_limit: None,
        timeout_secs: Some(TOKEN_REQUEST_TIMEOUT_SECS),
        idempotent: true, // 只读
    };
    let response = send_with_retry(transport, sleeper, &request, TOKEN_RETRY)
        .await
        .map_err(|failure| TokenEndpointError::from(&failure))?;
    let envelope: Envelope = serde_json::from_str(&response.body)
        .map_err(|error| TokenEndpointError::new(0, format!("用户信息响应解析失败: {error}")))?;
    if envelope.code != 0 {
        return Err(TokenEndpointError::new(
            envelope.code,
            envelope.msg.unwrap_or_else(|| "取用户信息失败".to_string()),
        ));
    }
    let info = envelope
        .data
        .ok_or_else(|| TokenEndpointError::new(0, "用户信息响应里没有 data"))?;
    Ok((info.open_id, info.name))
}

// ---------------------------------------------------------------------------
// 单例行存取
// ---------------------------------------------------------------------------

/// 读出来的唯一一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UserTokenRow {
    pub(crate) id: i64,
    pub(crate) open_id: String,
    pub(crate) user_name: Option<String>,
    pub(crate) access_token: String,
    pub(crate) access_token_expires_at: i64,
    pub(crate) refresh_token: String,
    pub(crate) refresh_token_expires_at: i64,
    pub(crate) authorized_at: i64,
}

/// 读唯一一行；`None` = 从未授权。
///
/// 不按主键取值：这张表的不变式是「**至多一行**」（见 `user_token_table` 的模块文档），
/// 按「第一行」读写就不必依赖「插入时能显式指定自增主键」这一 DSL 行为。
pub(crate) async fn load_single(tokens: &Repository) -> Result<Option<UserTokenRow>, BaseError> {
    // 两个凭证列是 `secret`（`hidden = true`），**不进默认投影**：不显式点名就查不出来，
    // 报「字段未找到 [表: record]: access_token」。`select_fields` 只校验字段存在与角色
    // 读权限、不看 `hidden`，所以这里是读 secret 列的唯一入口（同 `reveal_token` 读
    // `token_cipher`；见 yang-base/src/table/table_query/validation.rs:34 `default_read_fields`）。
    let rows = tokens
        .query()
        .select_fields(&[
            "id",
            "open_id",
            "user_name",
            "access_token",
            "access_token_expires_at",
            "refresh_token",
            "refresh_token_expires_at",
            "authorized_at",
        ])?
        .page(1, 1)?
        .all()
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(UserTokenRow {
        id: row.require("id")?,
        open_id: row.require("open_id")?,
        user_name: row.optional("user_name")?,
        access_token: row.require("access_token")?,
        access_token_expires_at: row.require("access_token_expires_at")?,
        refresh_token: row.require("refresh_token")?,
        refresh_token_expires_at: row.require("refresh_token_expires_at")?,
        authorized_at: row.require("authorized_at")?,
    }))
}

/// 落一次**完整授权**的结果（授权回调专用）。
///
/// 已有行就整体替换（换账号授权也是替换，不是新增），没有就插入。
/// `authorized_at` 只在这里写——**刷新不重置它**，那正是飞书 365 天规则的判据。
pub(crate) async fn store_authorization(
    tokens: &Repository,
    open_id: &str,
    user_name: Option<&str>,
    bundle: &TokenBundle,
    authorized_at: i64,
) -> Result<(), BaseError> {
    let mut row = Record::new();
    row.insert("open_id", json!(open_id));
    row.insert("user_name", json!(user_name));
    row.insert("access_token", json!(&bundle.access_token));
    row.insert(
        "access_token_expires_at",
        json!(bundle.access_token_expires_at),
    );
    row.insert("refresh_token", json!(&bundle.refresh_token));
    row.insert(
        "refresh_token_expires_at",
        json!(bundle.refresh_token_expires_at),
    );
    row.insert("scope", json!(bundle.scope));
    row.insert("authorized_at", json!(authorized_at));

    match load_single(tokens).await? {
        Some(existing) => {
            tokens
                .query()
                .where_eq("id", json!(existing.id))?
                .update(row)
                .await?;
        }
        None => {
            tokens.query().insert(row).await?;
        }
    }
    Ok(())
}

/// 刷新之后写回凭证。**不碰 `authorized_at`**（见 [`store_authorization`]）。
async fn update_tokens(
    tokens: &Repository,
    id: i64,
    bundle: &TokenBundle,
) -> Result<(), BaseError> {
    let mut row = Record::new();
    row.insert("access_token", json!(&bundle.access_token));
    row.insert(
        "access_token_expires_at",
        json!(bundle.access_token_expires_at),
    );
    row.insert("refresh_token", json!(&bundle.refresh_token));
    row.insert(
        "refresh_token_expires_at",
        json!(bundle.refresh_token_expires_at),
    );
    row.insert("scope", json!(bundle.scope));
    tokens
        .query()
        .where_eq("id", json!(id))?
        .update(row)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 取用（派发路径的门面）
// ---------------------------------------------------------------------------

/// 取用户身份时的失败。
#[derive(Debug, Clone)]
pub(crate) enum UserTokenError {
    /// 系统里还没有飞书用户身份：管理员没授权过。
    NotAuthorized,
    /// 必须由管理员**重新授权**（刷新凭证失效，或已满 365 天红线）。
    ReauthRequired { code: i32, message: String },
    /// 其它失败：网络、飞书 5xx、库读写。可重试。
    Failed { message: String },
}

impl UserTokenError {
    /// 给工作流调用方看的可行动文案。
    pub(crate) fn message(&self) -> String {
        match self {
            Self::NotAuthorized => {
                "飞书用户身份未授权：请在「飞书审批配置」页点「去授权」完成一次飞书授权".to_string()
            }
            Self::ReauthRequired { code, message } => format!(
                "飞书用户授权已失效（错误码 {code}）：{message}。请重新授权（授权满 365 天后飞书强制重新授权）"
            ),
            Self::Failed { message } => format!("飞书用户凭证刷新失败：{message}"),
        }
    }
}

impl From<TokenEndpointError> for UserTokenError {
    fn from(error: TokenEndpointError) -> Self {
        let needs_reauth = error.needs_reauth();
        let hint = error.hint().unwrap_or_default();
        let message = if hint.is_empty() {
            error.message
        } else {
            format!("{}（{hint}）", error.message)
        };
        if needs_reauth {
            Self::ReauthRequired {
                code: error.code,
                message,
            }
        } else {
            Self::Failed { message }
        }
    }
}

/// access_token 是否还能直接用（留出 [`ACCESS_TOKEN_REFRESH_MARGIN_SECONDS`] 余量）。
pub(crate) fn access_token_is_fresh(expires_at: i64, now: i64) -> bool {
    expires_at - ACCESS_TOKEN_REFRESH_MARGIN_SECONDS > now
}

/// 进程内串行化刷新。
///
/// `ponytail:` 单实例假设（与仓库三个 worker 一致）。多实例部署要换成 Redis 锁，
/// 照 `TenantTokenCache` 的 `acquire_lock`/`release_lock`。
static REFRESH_LOCK: LazyLock<AsyncMutex<()>> = LazyLock::new(|| AsyncMutex::new(()));

/// 取一个可用的 `user_access_token`；快过期就先刷新。
///
/// 刷新在锁内**复查**一次：并发的两个调用里只有一个真去打飞书，另一个直接拿到
/// 刚写回的新凭证。
pub(crate) async fn user_access_token(
    tokens: &Repository,
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    credentials: &FeishuCredentials,
) -> Result<String, UserTokenError> {
    let storage = |error: BaseError| UserTokenError::Failed {
        message: format!("用户身份表读写失败：{error}"),
    };

    let Some(row) = load_single(tokens).await.map_err(storage)? else {
        return Err(UserTokenError::NotAuthorized);
    };
    if access_token_is_fresh(row.access_token_expires_at, now_unix_secs()) {
        return Ok(row.access_token);
    }

    let _guard = REFRESH_LOCK.lock().await;
    let Some(row) = load_single(tokens).await.map_err(storage)? else {
        return Err(UserTokenError::NotAuthorized);
    };
    let now = now_unix_secs();
    if access_token_is_fresh(row.access_token_expires_at, now) {
        return Ok(row.access_token);
    }

    let bundle = refresh_tokens(transport, sleeper, credentials, &row.refresh_token, now)
        .await
        .map_err(UserTokenError::from)?;
    update_tokens(tokens, row.id, &bundle)
        .await
        .map_err(storage)?;
    Ok(bundle.access_token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::time::Duration;

    use crate::addon::feishu::domain::outbound::OutboundResponse;

    /// 按脚本回放的传输：每条脚本是一对 (HTTP 状态, 响应体)。
    struct ScriptedTransport {
        script: Mutex<VecDeque<(u16, String)>>,
        seen: Mutex<Vec<OutboundRequest>>,
    }

    impl ScriptedTransport {
        fn new(script: Vec<(u16, String)>) -> Self {
            Self {
                script: Mutex::new(script.into_iter().collect()),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn seen(&self) -> Vec<OutboundRequest> {
            self.seen
                .lock()
                .map(|seen| seen.clone())
                .unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl OutboundTransport for ScriptedTransport {
        async fn send(&self, request: OutboundRequest) -> Result<OutboundResponse, BaseError> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(request);
            }
            let (status, body) = self
                .script
                .lock()
                .ok()
                .and_then(|mut script| script.pop_front())
                .unwrap_or_else(|| panic!("脚本已耗尽：不该有第二次出站调用"));
            Ok(OutboundResponse {
                status,
                body,
                bytes: None,
                headers: Default::default(),
            })
        }
    }

    struct NoSleep;

    #[async_trait::async_trait]
    impl Sleeper for NoSleep {
        async fn sleep(&self, _duration: Duration) {}
    }

    fn credentials() -> FeishuCredentials {
        FeishuCredentials {
            app_id: "cli_test_app".to_string(),
            app_secret: "test-app-secret".to_string(),
        }
    }

    fn success_body() -> String {
        json!({
            "code": 0,
            "access_token": "u-access-1",
            "expires_in": 7200,
            "refresh_token": "ur-refresh-1",
            "refresh_token_expires_in": 604800,
            "scope": "contact:contact.base:readonly offline_access",
            "token_type": "Bearer"
        })
        .to_string()
    }

    #[test]
    fn authorize_url_carries_offline_access_and_encodes_the_redirect() {
        let url = build_authorize_url(
            "cli_a5d611352af9d00b",
            "http://47.109.148.207:18654/api/v1/feishu/oauth/callback",
            "state-1",
        );
        assert!(url.starts_with(AUTHORIZE_URL), "{url}");
        assert!(url.contains("client_id=cli_a5d611352af9d00b"), "{url}");
        assert!(url.contains("response_type=code"), "{url}");
        // 逐个点名而不是比整串：以后补 scope 时这条测试该继续有效，
        // 而漏掉任何一个都会红。`contact:user:search` 是 2026-10-13 真机 99991679
        // 点名的那个——`users/search` 只认它，`contact.base:readonly` 不蕴含它。
        assert!(
            url.contains("scope=contact%3Auser%3Asearch%20"),
            "缺 contact:user:search：{url}"
        );
        assert!(
            url.contains("%20contact%3Acontact.base%3Areadonly%20"),
            "缺 contact:contact.base:readonly：{url}"
        );
        assert!(
            url.contains("%20offline_access"),
            "缺 offline_access：{url}"
        );
        assert!(
            url.contains(
                "redirect_uri=http%3A%2F%2F47.109.148.207%3A18654%2Fapi%2Fv1%2Ffeishu%2Foauth%2Fcallback"
            ),
            "redirect_uri 必须整体编码：{url}"
        );
        assert!(url.contains("state=state-1"), "{url}");
        assert!(url.contains("prompt=consent"), "{url}");
    }

    #[test]
    fn state_is_single_use_and_expires() {
        let now = 1_700_000_000;
        let state = mint_state(now);
        assert!(consume_state(&state, now), "刚签发的 state 必须通过");
        assert!(!consume_state(&state, now), "同一个 state 不得二次通过");

        let expired = mint_state(now);
        assert!(
            !consume_state(&expired, now + STATE_TTL_SECONDS),
            "过期边界必须拒绝"
        );

        let fresh = mint_state(now);
        assert!(consume_state(&fresh, now + STATE_TTL_SECONDS - 1));
        assert!(!consume_state("从未签发", now), "未签发的 state 一律不认");
    }

    #[test]
    fn access_token_freshness_keeps_a_margin() {
        let now = 1_700_000_000;
        assert!(access_token_is_fresh(now + 3_600, now));
        assert!(!access_token_is_fresh(now + 60, now), "余量内必须提前刷新");
        assert!(!access_token_is_fresh(now - 1, now));
    }

    #[tokio::test]
    async fn exchange_code_sends_the_code_and_computes_expiries() {
        let transport = ScriptedTransport::new(vec![(200, success_body())]);
        let now = 1_700_000_000;
        let bundle = exchange_code(
            &transport,
            &NoSleep,
            &credentials(),
            "code-1",
            "http://cb.example.com/x",
            now,
        )
        .await
        .unwrap_or_else(|error| panic!("换码应成功: {error:?}"));

        assert_eq!(bundle.access_token, "u-access-1");
        assert_eq!(bundle.access_token_expires_at, now + 7200);
        assert_eq!(bundle.refresh_token, "ur-refresh-1");
        assert_eq!(bundle.refresh_token_expires_at, now + 604_800);

        let seen = transport.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].url, TOKEN_URL);
        assert!(
            seen[0].bearer_token.is_none(),
            "v3 端点不带 Authorization 头"
        );
        let body = seen[0]
            .json_body
            .clone()
            .unwrap_or_else(|| panic!("请求体必须有"));
        assert_eq!(body["grant_type"], "authorization_code");
        assert_eq!(body["code"], "code-1");
        assert_eq!(body["redirect_uri"], "http://cb.example.com/x");
        assert_eq!(body["client_secret"], "test-app-secret");
    }

    #[tokio::test]
    async fn a_missing_refresh_token_is_reported_as_a_scope_problem() {
        // 授权时没带 offline_access：令牌响应里没有 refresh_token。这条错误必须
        // 点名 scope，否则会被读成「飞书抽风」。
        let body = json!({"code": 0, "access_token": "u-1", "expires_in": 7200}).to_string();
        let transport = ScriptedTransport::new(vec![(200, body)]);
        let error = match exchange_code(
            &transport,
            &NoSleep,
            &credentials(),
            "code-1",
            "http://cb.example.com/x",
            0,
        )
        .await
        {
            Ok(_) => panic!("缺 refresh_token 必须失败"),
            Err(error) => error,
        };
        assert!(error.message.contains("offline_access"), "{error:?}");
    }

    #[tokio::test]
    async fn refresh_failures_are_classified_by_whether_reauth_is_needed() {
        for (code, needs_reauth) in [
            (20037, true),
            (20026, true),
            (20064, true),
            (20073, true),
            (20050, false),
        ] {
            let body = json!({
                "code": code,
                "error": "server_error",
                "error_description": "boom"
            })
            .to_string();
            let transport = ScriptedTransport::new(vec![(400, body)]);
            let error =
                match refresh_tokens(&transport, &NoSleep, &credentials(), "ur-refresh-1", 0).await
                {
                    Ok(_) => panic!("{code} 必须失败"),
                    Err(error) => error,
                };
            assert_eq!(error.code, code);
            assert_eq!(error.needs_reauth(), needs_reauth, "错误码 {code}");
        }
    }

    #[tokio::test]
    async fn refresh_is_not_retried_because_the_token_is_single_use() {
        // 脚本只有一条：若实现里带了重试，第二次出站会让脚本耗尽并 panic。
        let body = json!({"code": 20050, "error": "server_error"}).to_string();
        let transport = ScriptedTransport::new(vec![(500, body)]);
        let _ = refresh_tokens(&transport, &NoSleep, &credentials(), "ur-1", 0).await;
        assert_eq!(transport.seen().len(), 1, "一次性凭证不得重发");
    }

    #[tokio::test]
    async fn user_info_returns_open_id_and_name() {
        let body = json!({
            "code": 0,
            "msg": "success",
            "data": {"open_id": "ou_1", "name": "杨佳俊"}
        })
        .to_string();
        let transport = ScriptedTransport::new(vec![(200, body)]);
        let (open_id, name) = fetch_user_info(&transport, &NoSleep, "u-access-1")
            .await
            .unwrap_or_else(|error| panic!("取用户信息应成功: {error:?}"));
        assert_eq!(open_id, "ou_1");
        assert_eq!(name.as_deref(), Some("杨佳俊"));
        assert_eq!(
            transport.seen()[0].bearer_token.as_deref(),
            Some("u-access-1")
        );
    }

    #[test]
    fn reminder_line_starts_at_350_days() {
        let day = 86_400;
        let authorized_at = 1_700_000_000;
        assert!(!reauth_reminder_due(
            authorized_at,
            authorized_at + 349 * day
        ));
        assert!(reauth_reminder_due(
            authorized_at,
            authorized_at + 350 * day
        ));
        assert!(reauth_reminder_due(
            authorized_at,
            authorized_at + 365 * day
        ));
        // 重新授权后 authorized_at 前移，提醒自然停。
        assert!(!reauth_reminder_due(
            authorized_at + 350 * day,
            authorized_at + 350 * day
        ));
    }

    #[test]
    fn error_messages_are_actionable() {
        let not_authorized = UserTokenError::NotAuthorized.message();
        assert!(not_authorized.contains("去授权"), "{not_authorized}");

        let expired = UserTokenError::from(TokenEndpointError::new(20037, "expired"));
        assert!(matches!(expired, UserTokenError::ReauthRequired { .. }));
        assert!(
            expired.message().contains("重新授权"),
            "{}",
            expired.message()
        );

        let hint = TokenEndpointError::new(20074, "not allowed");
        assert!(
            UserTokenError::from(hint).message().contains("开关"),
            "20074 必须点名后台开关"
        );
    }

    /// 造一个凭证对；`n` 只用来让各次取值互不相同。
    fn bundle(access: &str, refresh: &str, n: i64) -> TokenBundle {
        TokenBundle {
            access_token: access.to_owned(),
            access_token_expires_at: n,
            refresh_token: refresh.to_owned(),
            refresh_token_expires_at: n + 10_000,
            scope: Some("contact:contact.base:readonly offline_access".to_owned()),
        }
    }

    /// 读唯一一行；读不到就以 `why` 报错（仓库 deny `expect_used`，测试里不 panic）。
    async fn expect_row(tokens: &Repository, why: &str) -> anyhow::Result<UserTokenRow> {
        match load_single(tokens).await? {
            Some(row) => Ok(row),
            None => anyhow::bail!("{why}"),
        }
    }

    /// 全表行数（单例行不变式的探针）。
    async fn row_count(tokens: &Repository) -> anyhow::Result<usize> {
        let rows = tokens
            .query()
            .select_fields(&["id"])?
            .page(1, yang_base::table::MAX_TABLE_QUERY_PAGE_SIZE)?
            .all()
            .await?;
        Ok(rows.len())
    }

    /// 真实 MySQL：授权落库 → 读回，**两个 secret 列必须原样回来**。
    ///
    /// 钉住一个真机踩到的坑：`access_token` / `refresh_token` 标了 `secret`（即
    /// `hidden`），而框架的默认投影会**剔除 hidden 字段**
    /// （`yang-base/src/table/table_query/validation.rs:34` 的 `default_read_fields`）。
    /// 于是 `load_single` 用裸 `.query()` 时写入成功、读回必败，报
    /// 「字段未找到 [表: record]: access_token」——只有显式 `select_fields` 点名才读得到。
    /// 这条测试就是那道防线：把 `load_single` 的字段列表删掉，它会立刻红。
    #[tokio::test]
    #[ignore = "需要真实 MySQL 测试库"]
    async fn real_mysql_round_trips_secret_token_columns() -> anyhow::Result<()> {
        use yang_db::{Database, DatabaseConfig};

        let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")?;
        let database = Database::connect(&url).await?;
        let name: String = sqlx::query_scalar("SELECT DATABASE()")
            .fetch_one(database.pool())
            .await?;
        anyhow::ensure!(name.ends_with("_test"), "拒绝在非测试库运行");
        crate::schema::sync_with_database(
            database,
            DatabaseConfig::default(),
            std::sync::Arc::new(crate::config::SecuritySettings::default()),
        )
        .await?;
        let security = std::sync::Arc::new(crate::config::SecuritySettings::default());
        let database = Database::connect(&url).await?;
        let context = crate::addon::feishu::build_context(
            std::sync::Arc::new(database.pool().clone()),
            security,
            None,
        )?;
        let tokens = context.user_tokens();

        // 清成空表，保证下面先走**插入**分支（单例行表，全表至多一行）。
        while let Some(existing) = load_single(tokens).await? {
            tokens
                .query()
                .where_eq("id", json!(existing.id))?
                .delete()
                .await?;
        }
        assert!(load_single(tokens).await?.is_none(), "清表后应读不到行");

        // ---- 插入分支 ----
        let first = bundle("access-first", "refresh-first", 100);
        store_authorization(tokens, "ou_first", Some("张三"), &first, 1_700_000_000).await?;
        let loaded = expect_row(tokens, "刚存进去的行应能读回").await?;
        assert_eq!(loaded.open_id, "ou_first");
        assert_eq!(loaded.user_name.as_deref(), Some("张三"));
        assert_eq!(loaded.access_token, first.access_token);
        assert_eq!(loaded.refresh_token, first.refresh_token);
        assert_eq!(
            loaded.access_token_expires_at,
            first.access_token_expires_at
        );
        assert_eq!(
            loaded.refresh_token_expires_at,
            first.refresh_token_expires_at
        );
        assert_eq!(loaded.authorized_at, 1_700_000_000);
        assert_eq!(row_count(tokens).await?, 1);
        // 自增计数器不随删表归零，所以这里记下实际主键，用它验「替换而非新增」。
        let first_id = loaded.id;

        // ---- 重授权 = 更新分支：整体替换，且 `authorized_at` 必须重置 ----
        let second = bundle("access-second", "refresh-second", 200);
        store_authorization(tokens, "ou_second", Some("李四"), &second, 1_800_000_000).await?;
        let loaded = expect_row(tokens, "重授权后应仍有一行").await?;
        assert_eq!(loaded.access_token, second.access_token);
        assert_eq!(loaded.refresh_token, second.refresh_token);
        assert_eq!(
            loaded.authorized_at, 1_800_000_000,
            "重授权必须重置 350 天时钟"
        );
        assert_eq!(loaded.id, first_id, "重授权是替换而不是新增（主键不变）");
        assert_eq!(row_count(tokens).await?, 1, "单例行不变式：全表只有一行");

        // ---- 刷新分支：只换凭证，**不碰** `authorized_at` ----
        let third = bundle("access-third", "refresh-third", 300);
        update_tokens(tokens, loaded.id, &third).await?;
        let loaded = expect_row(tokens, "刷新后应仍有一行").await?;
        assert_eq!(loaded.access_token, third.access_token);
        assert_eq!(loaded.refresh_token, third.refresh_token);
        assert_eq!(loaded.authorized_at, 1_800_000_000, "刷新不得重置授权时钟");

        Ok(())
    }
}

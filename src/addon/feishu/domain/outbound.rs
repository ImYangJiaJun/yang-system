//! 飞书开放平台**出站**调用的传输抽象、失败分类与重试。
//!
//! # 方向说明（本文件里的「入站/出站」以本服务为参照系）
//!
//! 本 addon 原有的两条凭证都是**入站**的（飞书来调我们）：`feishu_datasource.token_hash`
//! 保护取选项端点，`management_api_token` 保护多维表格写入端点。本模块开始引入**出站**
//! 方向——本服务持 `app_id`/`app_secret` 主动调飞书开放平台。两者的凭证、失败面与
//! 安全影响完全不同，注释里不要混用。
//!
//! # 为什么不用 `yang_base::http::RetryConfig`
//!
//! 框架的重试判据只有 `retry.retry_on.contains(&resp.status())`（`http/request.rs:806`）
//! 加纯指数退避：**读不到响应头，也读不到响应体**。而飞书的失败语义恰好两样都需要：
//!
//! - 频控等待时长在响应头 `x-ogw-ratelimit-reset`（秒）里，**没有** `Retry-After`；
//! - 多数业务失败是 **HTTP 200 + 非 0 业务码**，按状态码判会把「app_token 填错」
//!   当成成功；反向地，可重试的 `1254607` 又是 **HTTP 400**，按状态码判会漏掉它。
//!
//! 所以重试在这条路径上是自建的。`RetryConfig` 仍然可用，但不用于飞书出站。
//!
//! # 判成败的唯一判据是响应体的业务码
//!
//! 官方《列出记录》错误码表里 `1254003 WrongBaseToken`、`1254040 BaseTokenNotFound`、
//! `1254041 TableIdNotFound`、`1254249/1254290` 频控、`1254302 Permission denied`
//! **全部标注 HTTP 200**。任何以 HTTP 状态码为成败判据的出站封装都会把配置错误、
//! 权限错误当成读取成功，使「连续失败次数」永不增长、告警彻底失效。
//!
//! 状态码只在**拿不到业务码**时兜底（网关 HTML 错误页、非 JSON 响应）。

#![allow(dead_code)] // 本模块先交付出站能力本身；消费者（拉取 worker）在后续批次接入。

use std::collections::BTreeMap;
use std::time::Duration;

use yang_base::http::HttpClient;
use yang_base::BaseError;

// ---------------------------------------------------------------------------
// 飞行契约常量：本文件是**唯一**出现飞书错误码字面量的地方
// ---------------------------------------------------------------------------

/// 通用频控（HTTP 429；部分旧版 OpenAPI 表现为 HTTP 400）。
pub(crate) const CODE_RATE_LIMITED: i32 = 99991400;
/// 多维表格自身的频控：`1254290 TooManyRequest`「请求过快，稍后重试」，**HTTP 200**。
pub(crate) const CODE_TOO_MANY_REQUEST: i32 = 1254290;
/// 响应体过大（HTTP 200）。**确定性失败**：同一请求重试必然同样过大，只会白烧频控配额。
pub(crate) const CODE_TOO_LARGE_RESPONSE: i32 = 1254030;

/// 重新换取 token **能**修好的：凭证过期/失效。
///
/// `99991663 Invalid access token` 是主判据。`20013`（`tenant access token` 无效）同类。
pub(crate) const CODES_TOKEN_EXPIRED: &[i32] = &[99991663, 20013];

/// 重新换取 token **修不了**的：请求里没带 token / 格式错 / 类型错。
///
/// 把这一族并进「token 失效」是有害的——那会触发一次「清缓存 + 强制刷新」，
/// 失败依旧，却把真正的问题（Authorization 拼装错误、误用 user_access_token）
/// 掩盖成一次多余的 token 轮换：
/// - `99991661` `Need a token`：请求头里根本没填 `Authorization`；
/// - `99991664` `invalid app token` / `99991665` `invalid tenant code`：凭证类型不对；
/// - `99991668`：`user_access_token` 形态的无效凭证（本链路只发 tenant_access_token）；
/// - `99991671` `must start with t-/u-`：值格式错误。
pub(crate) const CODES_TOKEN_MALFORMED: &[i32] =
    &[99991661, 99991664, 99991665, 99991668, 99991671];

/// 官方明确写了「稍后重试」的业务码。
///
/// - `1254036` `Base is copying, please try again later.`（HTTP 400）
/// - `1254607` `Data not ready, please try again later`（HTTP 400，
///   官方排查建议原文即「建议等待一段时间后重试」）
/// - `1255001` `InternalError` / `1255002` `RpcError`（HTTP 200，官方「有疑问可咨询客服」）
///
/// 注意 `1254030 TooLargeResponse` **不在**这里——它不是瞬态失败。
pub(crate) const CODES_RETRYABLE: &[i32] = &[1254036, 1254607, 1255001, 1255002];

/// 权限类失败，**HTTP 200**。
///
/// 这一族的存在是「不能用 401/403 判权限」的直接证据：多维表格的权限问题
/// （尤其开启高级权限后应用不在授权群里）走的是 `1254302`，而不是 HTTP 403。
pub(crate) const CODES_PERMISSION_DENIED: &[i32] = &[1254302, 1254303];

// ---------------------------------------------------------------------------
// 请求/响应
// ---------------------------------------------------------------------------

/// 出站方法。只支持两种：取 token 与取记录都是 GET/POST 的简单形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutboundMethod {
    Get,
    Post,
}

/// 传输中立的请求描述。
///
/// `query` 以**键值对**携带而不是预拼进 URL：百分号编码交给真传输
/// （`reqwest` 的 `.queries()` 会把 `field_names` 值里的 `["` 与中文正确编码），
/// 上层不手写编码，也就不可能出现「手工拼串把 `&` 当分隔符」这类注入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutboundRequest {
    pub(crate) method: OutboundMethod,
    pub(crate) url: String,
    pub(crate) query: Vec<(String, String)>,
    /// 取 `tenant_access_token` 的请求**必须为 `None`**：该接口的凭证在请求体里，
    /// 不带 `Authorization` 头。
    pub(crate) bearer_token: Option<String>,
    pub(crate) json_body: Option<serde_json::Value>,
    /// 请求级超时（秒）。为 `None` 时用客户端默认值。
    pub(crate) timeout_secs: Option<u64>,
    /// 失败后能否安全重发。取 token 与取记录都是幂等的：飞书换 token 时新旧并存，
    /// 读接口无副作用。
    pub(crate) idempotent: bool,
}

/// 传输中立的响应描述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutboundResponse {
    pub(crate) status: u16,
    pub(crate) body: String,
    /// 键一律小写。本模块只消费 `x-ogw-ratelimit-reset`。
    pub(crate) headers: BTreeMap<String, String>,
}

/// 出站传输。
///
/// 实现**不做重试**：重试统一由 [`send_with_retry`] 负责。两处都开会让一次逻辑调用
/// 被放大成 N×M 次真实请求，既烧频控配额，又让 `max_attempts` 不再是可推理的上界。
#[async_trait::async_trait]
pub(crate) trait OutboundTransport: Send + Sync {
    async fn send(&self, request: OutboundRequest) -> Result<OutboundResponse, BaseError>;
}

/// 基于框架 `HttpClient` 的真实传输。
pub(crate) struct HttpClientTransport {
    client: HttpClient,
}

impl HttpClientTransport {
    pub(crate) fn new(client: HttpClient) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl OutboundTransport for HttpClientTransport {
    async fn send(&self, request: OutboundRequest) -> Result<OutboundResponse, BaseError> {
        let builder = match request.method {
            OutboundMethod::Get => self.client.get(&request.url),
            OutboundMethod::Post => self.client.post(&request.url),
        };

        let mut builder = builder.queries(
            request
                .query
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect(),
        );
        if let Some(token) = request.bearer_token.as_deref() {
            builder = builder.bearer_token(token);
        }
        if let Some(body) = request.json_body.as_ref() {
            builder = builder.json(body)?;
            // 官方把置换凭证接口的 `Content-Type` 标为**固定值**
            // `application/json; charset=utf-8`，而框架的 `.json()` 只写
            // `application/json`。这里补上 charset，让请求逐字对齐文档——
            // 这类「网关按字节比头」的差异一旦出问题极难排查。
            builder = builder.content_type("application/json; charset=utf-8");
        }
        if let Some(timeout_secs) = request.timeout_secs {
            builder = builder.timeout(timeout_secs);
        }

        // 框架的重试在这里显式关闭：本模块自己实现（见文件头）。不关的话
        // `RetryConfig` 的默认 `retry_on` 只覆盖 502/503/504，却又会对所有
        // 非幂等方法静默降级——两套策略叠加后行为无法从任一处的配置推理出来。
        let response = builder.send().await?;

        let status = response.status();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
            })
            .collect();
        let body = response.text().await?;

        Ok(OutboundResponse {
            status,
            body,
            headers,
        })
    }
}

/// 可替换的睡眠，**唯一目的是可测性**：让「按服务端建议等 N 秒」在单测里可断言而不真等。
#[async_trait::async_trait]
pub(crate) trait Sleeper: Send + Sync {
    async fn sleep(&self, duration: Duration);
}

/// 生产实现。
pub(crate) struct TokioSleeper;

#[async_trait::async_trait]
impl Sleeper for TokioSleeper {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

// ---------------------------------------------------------------------------
// 失败分类
// ---------------------------------------------------------------------------

/// 一次出站失败的处置类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureKind {
    /// 可退避重试；服务端给了建议秒数则优先采信。
    Retry { retry_after_seconds: Option<u64> },
    /// 凭证失效：补救是「清缓存 + 强制刷新一次 + 重试一次」，**不是**退避重试
    /// （重复退避换不出新 token）。
    TokenExpired,
    /// 绝不重试，且**不动** token 缓存。
    Fatal { code: i32 },
}

/// 判成败的唯一入口。返回 `None` 表示成功。
///
/// 判定顺序是有讲究的：**业务码先于 HTTP 状态码**。
///
/// 尤其注意 token 族必须先于 `401/403`：网关对失效 token 返回的是 **HTTP 400 +
/// `99991663`**（实测），但不同租户/网关路径也可能返回 401。若先判 401/403 并归为
/// 「不可重试的鉴权失败」，token 过期就会永远清不掉缓存、刷不出新值，在缓存 TTL
/// 到期前全线失败且不自愈。
pub(crate) fn classify(
    status: u16,
    headers: &BTreeMap<String, String>,
    body: &str,
) -> Option<FailureKind> {
    // ① 429 无条件是频控。它没有业务码可读，且部分旧版 OpenAPI 把频控放在
    //    `400 + 99991400`——那一种走 ② 的业务码分支。
    if status == 429 {
        return Some(FailureKind::Retry {
            retry_after_seconds: rate_limit_reset_seconds(headers),
        });
    }

    // ② 业务码优先。
    if let Some(code) = business_code(body) {
        if code == 0 {
            // 业务成功。HTTP 非 2xx 属异常形态（网关加了层但 body 是飞书的），
            // 落到 ③ 按状态码兜底更保守。
            if (200..300).contains(&status) {
                return None;
            }
        } else if CODES_TOKEN_EXPIRED.contains(&code) {
            return Some(FailureKind::TokenExpired);
        } else if CODES_TOKEN_MALFORMED.contains(&code) {
            // 换 token 修不了，所以是 Fatal（不触发 token 轮换）。
            return Some(FailureKind::Fatal { code });
        } else if code == CODE_RATE_LIMITED
            || code == CODE_TOO_MANY_REQUEST
            || CODES_RETRYABLE.contains(&code)
        {
            return Some(FailureKind::Retry {
                retry_after_seconds: rate_limit_reset_seconds(headers),
            });
        } else {
            // 1254030（确定性过大）/ 1254024（字段名不匹配）/ 1254302（无权限）/
            // 1254003 / 1254040 / 1254041 …：都是重试无用的配置或权限问题。
            return Some(FailureKind::Fatal { code });
        }
    }

    // ③ 拿不到业务码（网关 HTML 错误页、非 JSON）才退回状态码。
    match status {
        200..=299 => None,
        500..=599 => Some(FailureKind::Retry {
            retry_after_seconds: None,
        }),
        _ => Some(FailureKind::Fatal { code: 0 }),
    }
}

/// 频控建议等待秒数。
///
/// 飞书频控文档给的是响应头 `x-ogw-ratelimit-reset`（**秒**），全篇没有 `Retry-After`。
/// 写成只读 `Retry-After` 会恒读不到，静默退化成纯指数退避——在限流场景下等于
/// 用错误的节奏继续打对方接口。`Retry-After` 只作为兜底（部分网关会补它）。
pub(crate) fn rate_limit_reset_seconds(headers: &BTreeMap<String, String>) -> Option<u64> {
    headers
        .get("x-ogw-ratelimit-reset")
        .or_else(|| headers.get("retry-after"))
        .and_then(|value| value.trim().parse::<u64>().ok())
}

/// 从响应体里取业务码。非 JSON、缺 `code`、非整数一律 `None`（交给状态码兜底）。
pub(crate) fn business_code(body: &str) -> Option<i32> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    i32::try_from(value.get("code")?.as_i64()?).ok()
}

/// 可执行的排查文案。
///
/// 存在的理由是「码义不能望文生义」：`1254040` 是 `BaseTokenNotFound`（app_token
/// 不存在），**不是**无文档权限；把前者说成后者会把运维引向错误的排查方向。
pub(crate) fn fatal_hint(code: i32) -> Option<&'static str> {
    match code {
        1254003 | 1254040 => Some("bitable 的 app_token（base token）不存在或填错"),
        1254004 | 1254009 | 1254044 => Some("table_id 或 field_id 不存在或填错"),
        1254024 => Some(
            "field_names 与表格字段名不完全匹配：先调「列出字段」取精确字段名（\
             界面上的名称可能忽略空格、换行或特殊符号差异）",
        ),
        CODE_TOO_LARGE_RESPONSE => Some(
            "响应体过大：降低 page_size 或收窄 field_names 后重试；\
             原样重试同一请求无用（确定性失败，重试只会白烧频控配额）",
        ),
        c if CODES_PERMISSION_DENIED.contains(&c) => Some(
            "无该文档权限（常由多维表格开启高级权限造成）：\
             在高级权限设置里加一个包含本应用的群，或关闭高级权限",
        ),
        c if CODES_TOKEN_MALFORMED.contains(&c) => Some(
            "凭证未携带 / 格式错 / 类型错：重新换取 token 修不了，\
             检查 Authorization 头的拼装与凭证类型（本链路只发 tenant_access_token）",
        ),
        _ => None,
    }
}

/// 截断响应体用于日志与 `last_error`。
///
/// 按**字符**截断而不是字节：`msg` 是中文，`&body[..500]` 会切在 UTF-8 边界中间
/// 直接 panic；而把整页 JSON（可达数百 KB）写进 `last_error` 也没有排查价值。
pub(crate) fn summarize(status: u16, body: &str) -> String {
    const LIMIT: usize = 500;
    let total = body.chars().count();
    let head: String = body.chars().take(LIMIT).collect();
    if total > LIMIT {
        format!("HTTP {status}: {head}…（共 {total} 字符，已截断）")
    } else {
        format!("HTTP {status}: {head}")
    }
}

/// 出站失败。
#[derive(Debug)]
pub(crate) struct OutboundFailure {
    pub(crate) kind: FailureKind,
    pub(crate) message: String,
}

impl std::fmt::Display for OutboundFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hint = match self.kind {
            FailureKind::Fatal { code } => fatal_hint(code),
            _ => None,
        };
        match hint {
            Some(hint) => write!(formatter, "飞书出站调用失败: {}；{hint}", self.message),
            None => write!(formatter, "飞书出站调用失败: {}", self.message),
        }
    }
}

impl std::error::Error for OutboundFailure {}

// ---------------------------------------------------------------------------
// 重试策略
// ---------------------------------------------------------------------------

/// 自建重试策略。类型全用 `u64`：混入 `u32` 会让「最坏耗时」这种跨字段算术
/// 出现 `u64 * u32` 的类型错误（`impl Mul<u32> for u64` 不存在）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RetryPolicy {
    /// 总尝试次数（含首次）。1 表示不重试。
    pub(crate) max_attempts: u64,
    pub(crate) base_backoff_ms: u64,
    pub(crate) max_backoff_ms: u64,
    /// 采信服务端建议等待的上限。一个异常大的 reset 头不能把整个轮次卡死。
    pub(crate) max_retry_after_ms: u64,
}

/// 换取 token：尝试少、退避短。
///
/// 这是**不变式**的一半——`TOKEN_RETRY.worst_case_seconds(TOKEN_REQUEST_TIMEOUT_SECS)`
/// 必须小于 [`crate::addon::feishu::domain::tenant_token::LOCK_TTL_SECONDS`]，
/// 否则锁会在刷新完成前过期，别的实例抢到锁重复换取 token（飞书对签发有频控，
/// 重复刷新只会加剧限流）。
pub(crate) const TOKEN_RETRY: RetryPolicy = RetryPolicy {
    max_attempts: 3,
    base_backoff_ms: 200,
    max_backoff_ms: 1_000,
    max_retry_after_ms: 2_000,
};

/// 单次换取 token 的请求超时（秒）。
pub(crate) const TOKEN_REQUEST_TIMEOUT_SECS: u64 = 5;

/// 拉取记录：在**锁外**执行，因此可以放宽。
pub(crate) const PULL_RETRY: RetryPolicy = RetryPolicy {
    max_attempts: 3,
    base_backoff_ms: 500,
    max_backoff_ms: 4_000,
    max_retry_after_ms: 30_000,
};

/// 单页拉取的请求超时（秒）。比 token 那次宽松：一页最多 500 行、可能含长文本。
pub(crate) const PULL_REQUEST_TIMEOUT_SECS: u64 = 20;

impl RetryPolicy {
    /// 单轮的最坏耗时上界（秒）。用于证明锁 TTL 足够覆盖临界区。
    pub(crate) fn worst_case_seconds(&self, request_timeout_secs: u64) -> u64 {
        request_timeout_secs * self.max_attempts
            + (self.max_retry_after_ms / 1_000) * self.max_attempts.saturating_sub(1)
    }

    /// 第 `attempt` 次（1 起）失败后的等待毫秒数。服务端建议值优先，并被上限截断。
    pub(crate) fn backoff_ms(&self, attempt: u64, retry_after_seconds: Option<u64>) -> u64 {
        if let Some(seconds) = retry_after_seconds {
            return seconds
                .saturating_mul(1_000)
                .min(self.max_retry_after_ms)
                .min(self.max_backoff_ms.max(self.max_retry_after_ms));
        }
        let shift = attempt.saturating_sub(1).min(20);
        self.base_backoff_ms
            .saturating_mul(1u64 << shift)
            .min(self.max_backoff_ms)
    }
}

/// 一次失败后该怎么办。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Disposition {
    /// 等待这么多毫秒后重发。
    RetryAfter(u64),
    /// 放弃，把失败返回给调用方。
    Give,
}

/// 纯函数：把「失败类别 + 已用尝试数 + 是否幂等 + 策略」折成处置。
///
/// 非幂等请求一律不重试（`idempotent == false` 时直接 `Give`）：这条闸门与框架
/// `RetryConfig::retry_non_idempotent` 的取向一致，不重发可能有副作用的请求。
pub(crate) fn disposition(
    kind: FailureKind,
    attempt: u64,
    idempotent: bool,
    policy: RetryPolicy,
) -> Disposition {
    match kind {
        // 凭证失效与确定性失败都不退避重试。前者的补救是「清缓存 + 强制刷新一次」，
        // 由调用方在上层做（见 `list_all_records`），在这里退避只会白等。
        FailureKind::TokenExpired | FailureKind::Fatal { .. } => Disposition::Give,
        FailureKind::Retry {
            retry_after_seconds,
        } => {
            if !idempotent || attempt >= policy.max_attempts {
                return Disposition::Give;
            }
            Disposition::RetryAfter(policy.backoff_ms(attempt, retry_after_seconds))
        }
    }
}

/// **唯一的重试入口。**
///
/// 成功返回响应；失败返回最后一次的 [`OutboundFailure`]（连同它的 `kind`，
/// 让上层能区分「token 失效」与「别放弃」）。
pub(crate) async fn send_with_retry(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    request: &OutboundRequest,
    policy: RetryPolicy,
) -> Result<OutboundResponse, OutboundFailure> {
    let mut attempt: u64 = 0;
    loop {
        attempt += 1;
        let (kind, message) = match transport.send(request.clone()).await {
            Ok(response) => match classify(response.status, &response.headers, &response.body) {
                None => return Ok(response),
                Some(kind) => (kind, summarize(response.status, &response.body)),
            },
            Err(error) => {
                // 传输层错误按框架自己的可重试性分类：`HttpTimeout`(300004) 与
                // `HttpRequestFailed`(300002) 是 Transient，其余（如 URL 非法）是 Client。
                let kind = if error.is_retryable() {
                    FailureKind::Retry {
                        retry_after_seconds: None,
                    }
                } else {
                    FailureKind::Fatal { code: error.code() }
                };
                (kind, error.to_string())
            }
        };

        match disposition(kind, attempt, request.idempotent, policy) {
            Disposition::RetryAfter(wait_ms) => {
                tracing::warn!(
                    attempt,
                    max_attempts = policy.max_attempts,
                    wait_ms,
                    ?kind,
                    "飞书出站调用失败，退避后重试"
                );
                sleeper.sleep(Duration::from_millis(wait_ms)).await;
            }
            Disposition::Give => {
                metrics::counter!("feishu_outbound_request_total", "result" => "failed")
                    .increment(1);
                return Err(OutboundFailure { kind, message });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    fn headers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    fn empty_headers() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    fn body_with_code(code: i32) -> String {
        format!(r#"{{"code":{code},"msg":"x"}}"#)
    }

    // ---- classify ----

    #[test]
    fn success_requires_zero_business_code() {
        assert_eq!(
            classify(200, &empty_headers(), r#"{"code":0,"msg":"ok"}"#),
            None
        );
    }

    #[test]
    fn business_errors_on_http_200_are_not_success() {
        // 这四类官方都标 HTTP 200；按状态码判会把它们全当成功
        for code in [1254003, 1254040, 1254041, 1254302] {
            let kind = classify(200, &empty_headers(), &body_with_code(code));
            assert!(
                matches!(kind, Some(FailureKind::Fatal { code: got }) if got == code),
                "HTTP 200 + code={code} 必须判为失败，实际: {kind:?}"
            );
        }
    }

    #[test]
    fn too_large_response_is_fatal_not_retryable() {
        // 确定性失败：同一请求重试必然同样过大，归 Retry 只会白烧频控
        assert_eq!(
            classify(
                200,
                &empty_headers(),
                &body_with_code(CODE_TOO_LARGE_RESPONSE)
            ),
            Some(FailureKind::Fatal {
                code: CODE_TOO_LARGE_RESPONSE
            })
        );
    }

    #[test]
    fn bitable_native_rate_limit_is_retryable() {
        // 1254290 是 bitable 自己的频控，HTTP 200
        assert_eq!(
            classify(
                200,
                &empty_headers(),
                &body_with_code(CODE_TOO_MANY_REQUEST)
            ),
            Some(FailureKind::Retry {
                retry_after_seconds: None
            })
        );
    }

    #[test]
    fn http_429_is_retryable_and_reads_the_reset_header() {
        let kind = classify(
            429,
            &headers(&[("x-ogw-ratelimit-reset", "7")]),
            "rate limited",
        );
        assert_eq!(
            kind,
            Some(FailureKind::Retry {
                retry_after_seconds: Some(7)
            })
        );
    }

    #[test]
    fn rate_limit_reset_header_is_seconds_not_retry_after() {
        // 飞书给的是 x-ogw-ratelimit-reset；Retry-After 只是兜底
        assert_eq!(
            rate_limit_reset_seconds(&headers(&[("x-ogw-ratelimit-reset", "12")])),
            Some(12)
        );
        assert_eq!(
            rate_limit_reset_seconds(&headers(&[("retry-after", "3")])),
            Some(3)
        );
        // 官方头优先于兜底头
        assert_eq!(
            rate_limit_reset_seconds(&headers(&[
                ("x-ogw-ratelimit-reset", "9"),
                ("retry-after", "1")
            ])),
            Some(9)
        );
        assert_eq!(rate_limit_reset_seconds(&empty_headers()), None);
        // 非数字不 panic，按「没有建议值」处理
        assert_eq!(
            rate_limit_reset_seconds(&headers(&[("x-ogw-ratelimit-reset", "soon")])),
            None
        );
    }

    #[test]
    fn legacy_rate_limit_is_http_400_with_business_code() {
        assert_eq!(
            classify(400, &empty_headers(), &body_with_code(CODE_RATE_LIMITED)),
            Some(FailureKind::Retry {
                retry_after_seconds: None
            })
        );
    }

    #[test]
    fn vendor_advised_retry_codes_are_retryable_even_on_http_400() {
        // 1254607「建议等待一段时间后重试」/ 1254036「Base is copying」官方都是 400
        for code in CODES_RETRYABLE {
            let kind = classify(400, &empty_headers(), &body_with_code(*code));
            assert!(
                matches!(kind, Some(FailureKind::Retry { .. })),
                "code={code} 官方建议重试，实际: {kind:?}"
            );
        }
    }

    #[test]
    fn token_expiry_is_detected_regardless_of_http_status() {
        // 实测网关对失效 token 回 400；不同租户路径也可能回 401。
        // 两种状态码都必须落到 TokenExpired，否则 token 过期永远不自愈。
        for status in [200u16, 400, 401, 403] {
            assert_eq!(
                classify(status, &empty_headers(), &body_with_code(99991663)),
                Some(FailureKind::TokenExpired),
                "HTTP {status} + 99991663 必须判为 TokenExpired（token 族先于 401/403）"
            );
        }
    }

    #[test]
    fn malformed_token_codes_do_not_trigger_a_refresh() {
        // 这几种换 token 修不了：触发刷新只会掩盖真正的拼装错误
        for code in CODES_TOKEN_MALFORMED {
            assert_eq!(
                classify(200, &empty_headers(), &body_with_code(*code)),
                Some(FailureKind::Fatal { code: *code }),
                "code={code} 换 token 修不了，必须 Fatal 而不是 TokenExpired"
            );
        }
    }

    #[test]
    fn no_business_code_falls_back_to_http_status() {
        // 网关 HTML 错误页 / 非 JSON
        assert_eq!(classify(200, &empty_headers(), "<html>ok</html>"), None);
        assert_eq!(
            classify(502, &empty_headers(), "<html>bad gateway</html>"),
            Some(FailureKind::Retry {
                retry_after_seconds: None
            })
        );
        assert_eq!(
            classify(404, &empty_headers(), "<html>not found</html>"),
            Some(FailureKind::Fatal { code: 0 })
        );
    }

    #[test]
    fn truncated_json_is_treated_as_no_business_code() {
        assert_eq!(business_code(r#"{"code":"#), None);
        assert_eq!(business_code(""), None);
        assert_eq!(business_code(r#"{"msg":"no code"}"#), None);
        assert_eq!(business_code(r#"{"code":"not-a-number"}"#), None);
        assert_eq!(business_code(r#"{"code":0}"#), Some(0));
    }

    #[test]
    fn fatal_hints_do_not_confuse_token_not_found_with_permission_denied() {
        // 1254040 是 app_token 不存在，不是无权限；文案错会把运维引向错误方向
        let app_token_hint = fatal_hint(1254040).unwrap_or_default();
        assert!(
            app_token_hint.contains("app_token"),
            "实际: {app_token_hint}"
        );
        assert!(
            !app_token_hint.contains("权限"),
            "1254040 与权限无关，不得出现权限措辞: {app_token_hint}"
        );
        let permission_hint = fatal_hint(1254302).unwrap_or_default();
        assert!(permission_hint.contains("权限"), "实际: {permission_hint}");
        // 没有专门文案的码返回 None，而不是瞎编一条
        assert_eq!(fatal_hint(1254103), None);
    }

    #[test]
    fn summarize_truncates_on_char_boundaries() {
        // 纯中文（3 字节/字）在 500 字符处截断：按字节切会 panic
        let long = "通".repeat(600);
        let text = summarize(200, &long);
        assert!(text.contains("已截断"), "实际: {text}");
        let short = summarize(200, "短");
        assert_eq!(short, "HTTP 200: 短");
    }

    // ---- RetryPolicy / disposition ----

    #[test]
    fn backoff_grows_exponentially_then_clamps() {
        let policy = RetryPolicy {
            max_attempts: 10,
            base_backoff_ms: 100,
            max_backoff_ms: 800,
            max_retry_after_ms: 5_000,
        };
        assert_eq!(policy.backoff_ms(1, None), 100);
        assert_eq!(policy.backoff_ms(2, None), 200);
        assert_eq!(policy.backoff_ms(3, None), 400);
        assert_eq!(policy.backoff_ms(4, None), 800);
        assert_eq!(policy.backoff_ms(5, None), 800, "超过上限后截断");
    }

    #[test]
    fn server_suggested_wait_wins_and_is_capped() {
        let policy = RetryPolicy {
            max_attempts: 3,
            base_backoff_ms: 100,
            max_backoff_ms: 800,
            max_retry_after_ms: 5_000,
        };
        assert_eq!(policy.backoff_ms(1, Some(2)), 2_000, "建议值优先于指数退避");
        assert_eq!(
            policy.backoff_ms(1, Some(600)),
            5_000,
            "异常大的建议值必须被上限截断，否则一个坏响应头能把轮次卡死"
        );
    }

    #[test]
    fn disposition_gives_up_on_terminal_failures() {
        let policy = TOKEN_RETRY;
        for kind in [
            FailureKind::TokenExpired,
            FailureKind::Fatal { code: 1254040 },
            FailureKind::Fatal { code: 0 },
        ] {
            assert_eq!(
                disposition(kind, 1, true, policy),
                Disposition::Give,
                "{kind:?} 不该进退避重试"
            );
        }
    }

    #[test]
    fn disposition_respects_the_attempt_budget() {
        let policy = RetryPolicy {
            max_attempts: 3,
            base_backoff_ms: 10,
            max_backoff_ms: 100,
            max_retry_after_ms: 100,
        };
        let kind = FailureKind::Retry {
            retry_after_seconds: None,
        };
        assert!(matches!(
            disposition(kind, 1, true, policy),
            Disposition::RetryAfter(_)
        ));
        assert!(matches!(
            disposition(kind, 2, true, policy),
            Disposition::RetryAfter(_)
        ));
        assert_eq!(
            disposition(kind, 3, true, policy),
            Disposition::Give,
            "到达 max_attempts 必须停止"
        );
    }

    #[test]
    fn non_idempotent_requests_are_never_retried() {
        let kind = FailureKind::Retry {
            retry_after_seconds: None,
        };
        assert_eq!(disposition(kind, 1, false, PULL_RETRY), Disposition::Give);
    }

    #[test]
    fn token_refresh_lock_ttl_outlives_the_worst_case_critical_section() {
        // 这是 tenant_token::LOCK_TTL_SECONDS 取值的唯一依据：
        // 3 次尝试 × 5 秒超时 + 2 次退避 × 2 秒 = 19 秒
        let worst = TOKEN_RETRY.worst_case_seconds(TOKEN_REQUEST_TIMEOUT_SECS);
        assert_eq!(worst, 19);
        assert!(
            worst < crate::addon::feishu::domain::tenant_token::LOCK_TTL_SECONDS as u64,
            "锁 TTL({}) 必须严格大于最坏临界区耗时({worst}s)，否则锁会提前过期、\
             其它实例抢到锁重复换取 token",
            crate::addon::feishu::domain::tenant_token::LOCK_TTL_SECONDS
        );
    }

    // ---- send_with_retry ----

    /// 按脚本依次返回结果的假传输，并记录每次收到的请求。
    struct ScriptedTransport {
        responses: Mutex<VecDeque<Result<OutboundResponse, BaseError>>>,
        seen: Mutex<Vec<OutboundRequest>>,
    }

    impl ScriptedTransport {
        fn new(responses: Vec<Result<OutboundResponse, BaseError>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.seen.lock().map(|seen| seen.len()).unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl OutboundTransport for ScriptedTransport {
        async fn send(&self, request: OutboundRequest) -> Result<OutboundResponse, BaseError> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(request);
            }
            let mut responses = self
                .responses
                .lock()
                .map_err(|_| BaseError::ParamInvalid("test".to_string(), "锁中毒".to_string()))?;
            responses.pop_front().unwrap_or_else(|| {
                Err(BaseError::ParamInvalid(
                    "test".to_string(),
                    "脚本已耗尽".to_string(),
                ))
            })
        }
    }

    /// 只记录等待时长，不真等。
    struct RecordingSleeper {
        waits: Mutex<Vec<Duration>>,
    }

    impl RecordingSleeper {
        fn new() -> Self {
            Self {
                waits: Mutex::new(Vec::new()),
            }
        }

        fn waits_ms(&self) -> Vec<u64> {
            self.waits
                .lock()
                .map(|waits| waits.iter().map(|d| d.as_millis() as u64).collect())
                .unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl Sleeper for RecordingSleeper {
        async fn sleep(&self, duration: Duration) {
            if let Ok(mut waits) = self.waits.lock() {
                waits.push(duration);
            }
        }
    }

    fn response(status: u16, body: &str) -> OutboundResponse {
        OutboundResponse {
            status,
            body: body.to_string(),
            headers: BTreeMap::new(),
        }
    }

    fn get_request() -> OutboundRequest {
        OutboundRequest {
            method: OutboundMethod::Get,
            url: "https://example.invalid/x".to_string(),
            query: Vec::new(),
            bearer_token: None,
            json_body: None,
            timeout_secs: None,
            idempotent: true,
        }
    }

    #[tokio::test]
    async fn retries_within_budget_then_succeeds() {
        let transport = ScriptedTransport::new(vec![
            Ok(response(200, &body_with_code(CODE_TOO_MANY_REQUEST))),
            Ok(response(200, r#"{"code":0,"msg":"ok"}"#)),
        ]);
        let sleeper = RecordingSleeper::new();

        let result = send_with_retry(&transport, &sleeper, &get_request(), PULL_RETRY).await;

        assert!(result.is_ok(), "第二次应成功: {result:?}");
        assert_eq!(transport.call_count(), 2);
        assert_eq!(sleeper.waits_ms().len(), 1, "只应退避一次");
    }

    #[tokio::test]
    async fn exhausts_the_attempt_budget_and_reports_the_last_failure() {
        let transport = ScriptedTransport::new(vec![
            Ok(response(200, &body_with_code(CODE_TOO_MANY_REQUEST))),
            Ok(response(200, &body_with_code(CODE_TOO_MANY_REQUEST))),
            Ok(response(200, &body_with_code(CODE_TOO_MANY_REQUEST))),
        ]);
        let sleeper = RecordingSleeper::new();

        let failure = match send_with_retry(&transport, &sleeper, &get_request(), PULL_RETRY).await
        {
            Ok(_) => panic!("应耗尽重试后失败"),
            Err(failure) => failure,
        };

        assert_eq!(
            transport.call_count(),
            PULL_RETRY.max_attempts as usize,
            "尝试次数必须等于 max_attempts"
        );
        assert_eq!(
            sleeper.waits_ms().len(),
            PULL_RETRY.max_attempts as usize - 1,
            "最后一次失败后不应再等待"
        );
        assert!(matches!(failure.kind, FailureKind::Retry { .. }));
    }

    #[tokio::test]
    async fn token_expiry_is_not_retried_in_the_backoff_loop() {
        // TokenExpired 的补救是「清缓存 + 强制刷新一次」，由上层做；
        // 在退避循环里重试只会白等且换不出新 token。
        let transport = ScriptedTransport::new(vec![Ok(response(400, &body_with_code(99991663)))]);
        let sleeper = RecordingSleeper::new();

        let failure = match send_with_retry(&transport, &sleeper, &get_request(), PULL_RETRY).await
        {
            Ok(_) => panic!("token 失效必须失败"),
            Err(failure) => failure,
        };

        assert_eq!(failure.kind, FailureKind::TokenExpired);
        assert_eq!(transport.call_count(), 1, "不得在退避循环里重试");
        assert!(sleeper.waits_ms().is_empty(), "不得退避等待");
    }

    #[tokio::test]
    async fn framework_transport_errors_are_retried_when_they_are_transient() {
        // 只用 HttpTimeout 造错：`HttpRequestFailed` 的载荷形状随 http feature 改变
        // （String ↔ reqwest::Error），写进测试会让 `--all-features` 编译失败。
        let transport = ScriptedTransport::new(vec![
            Err(BaseError::HttpTimeout),
            Ok(response(200, r#"{"code":0}"#)),
        ]);
        let sleeper = RecordingSleeper::new();

        let result = send_with_retry(&transport, &sleeper, &get_request(), PULL_RETRY).await;
        assert!(result.is_ok(), "瞬态传输错误应重试: {result:?}");
        assert_eq!(transport.call_count(), 2);
    }

    #[tokio::test]
    async fn non_retryable_transport_errors_fail_immediately() {
        let transport = ScriptedTransport::new(vec![
            Err(BaseError::ParamInvalid(
                "url".to_string(),
                "非法".to_string(),
            )),
            Ok(response(200, r#"{"code":0}"#)),
        ]);
        let sleeper = RecordingSleeper::new();

        let failure = match send_with_retry(&transport, &sleeper, &get_request(), PULL_RETRY).await
        {
            Ok(_) => panic!("Client 类错误不该重试"),
            Err(failure) => failure,
        };
        assert_eq!(transport.call_count(), 1);
        assert!(matches!(failure.kind, FailureKind::Fatal { .. }));
    }

    #[tokio::test]
    async fn server_suggested_wait_is_used_verbatim() {
        let mut limited = response(429, "rate limited");
        limited
            .headers
            .insert("x-ogw-ratelimit-reset".to_string(), "3".to_string());
        let transport =
            ScriptedTransport::new(vec![Ok(limited), Ok(response(200, r#"{"code":0}"#))]);
        let sleeper = RecordingSleeper::new();

        let result = send_with_retry(&transport, &sleeper, &get_request(), PULL_RETRY).await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(sleeper.waits_ms(), vec![3_000], "必须按响应头的秒数等待");
    }
}

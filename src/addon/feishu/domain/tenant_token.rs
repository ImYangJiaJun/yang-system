//! 自建应用的 `tenant_access_token`：换取、缓存与短锁去重。
//!
//! 凭证形态、失败契约与重试见 [`super::outbound`]。本文件只负责「拿到一个可用的
//! tenant_access_token」这一件事。
//!
//! # 官方语义（决定了下面所有取值）
//!
//! - `POST /open-apis/auth/v3/tenant_access_token/internal`，请求体只有
//!   `app_id` / `app_secret`，**不带 `Authorization` 头**；
//! - 响应是**扁平**的 `{code, msg, tenant_access_token, expire}`——
//!   凭证与 `expire` 都在顶层，**不套 `data`**。复用带 `data` 的信封会让
//!   `tenant_access_token` 恒为 `None`；
//! - 最大有效期 2 小时。**剩余有效期 ≥ 30 分钟时调用会返回原有的 token**，
//!   小于 30 分钟才签发新的（新旧并存合法）。因此缓存 TTL 取 `expire - 300` 是安全的：
//!   它落在「不会在正被使用期间被换掉」的一侧。
//!
//! # 为什么要短锁
//!
//! token 是**应用级**的（一份服务 N 个数据源共用），缓存未命中时若多个实例同时
//! 出站换取，会对飞书的签发接口产生重复请求，而飞书对签发同样有频控——重复刷新
//! 只会加剧限流。锁的 TTL 必须**严格大于**单次刷新的最坏耗时，否则锁提前过期、
//! 第三个实例又能抢到锁。这条不变式由 `outbound` 的单测
//! `token_refresh_lock_ttl_outlives_the_worst_case_critical_section` 钉住。

#![allow(dead_code)] // 出站能力先落地；消费者（拉取 worker）在后续批次接入。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context};
use serde::Deserialize;
use yang_db::RedisClient;

use super::outbound::{
    send_with_retry, OutboundMethod, OutboundRequest, OutboundTransport, Sleeper,
    TOKEN_REQUEST_TIMEOUT_SECS, TOKEN_RETRY,
};

/// 飞书开放平台域名。
pub(crate) const FEISHU_OPEN_BASE: &str = "https://open.feishu.cn";

/// 自建应用换取 `tenant_access_token` 的路径。
const TENANT_TOKEN_PATH: &str = "/open-apis/auth/v3/tenant_access_token/internal";

/// 刷新锁 TTL（秒）。临界区**只含换取 token 的那次 POST**，不含拉取记录。
///
/// 取 30 的依据是 [`super::outbound::RetryPolicy::worst_case_seconds`]：
/// `3 次尝试 × 5 秒超时 + 2 次退避 × 2 秒 = 19 秒 < 30 秒`。
/// 文档里原先写的 10 秒**不满足**这条不变式（10 < 19），锁会在刷新完成前过期。
pub(crate) const LOCK_TTL_SECONDS: i64 = 30;

/// 缓存 TTL 相对 `expire` 留出的余量（秒）。
const CACHE_TTL_MARGIN_SECONDS: i64 = 300;

/// 未持锁时的等待轮次与间隔。
const LOCK_WAIT_ATTEMPTS: u8 = 3;
const LOCK_WAIT_INTERVAL: Duration = Duration::from_millis(200);

// ---------------------------------------------------------------------------
// 凭证与响应契约
// ---------------------------------------------------------------------------

/// 自建应用凭证。
///
/// 手写 `Debug` 是为了**不会**把 secret 打进日志：`#[derive(Debug)]` 会让任何一次
/// `tracing::debug!(?credentials)` 或断言失败输出把租户级凭证落到日志里。
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct FeishuCredentials {
    pub(crate) app_id: String,
    pub(crate) app_secret: String,
}

impl std::fmt::Debug for FeishuCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FeishuCredentials")
            .field("app_id", &self.app_id)
            .field("app_secret", &"<已隐藏>")
            .finish()
    }
}

/// 换取 token 的响应体。
///
/// **刻意不复用 `{code,msg,data}` 信封**：这个接口的字段在顶层（见文件头）。
#[derive(Debug, Deserialize)]
struct TenantTokenResponse {
    code: i32,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    tenant_access_token: Option<String>,
    #[serde(default)]
    expire: Option<i64>,
}

/// 一次成功的换取结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TenantTokenGrant {
    pub(crate) tenant_access_token: String,
    pub(crate) expire: i64,
}

/// 把响应体折成 [`TenantTokenGrant`]。
///
/// 把「业务码非 0」与「业务码为 0 但缺字段」**分开报**：前者是飞书拒绝（凭证错、
/// 应用停用等），后者是我们对契约的理解错了。两者的排查方向完全不同，合并成
/// 一句「换取失败」会让线上问题无法定位。
pub(crate) fn parse_token_grant(body: &str) -> anyhow::Result<TenantTokenGrant> {
    let response: TenantTokenResponse =
        serde_json::from_str(body).context("解析 tenant_access_token 响应失败")?;
    ensure!(
        response.code == 0,
        "飞书拒绝签发 tenant_access_token: code={}, msg={}",
        response.code,
        response.msg
    );
    let tenant_access_token = response
        .tenant_access_token
        .filter(|token| !token.trim().is_empty())
        .context("飞书返回 code=0 但缺少 tenant_access_token")?;
    let expire = response.expire.context("飞书返回 code=0 但缺少 expire")?;
    ensure!(expire > 0, "飞书返回的 expire 必须为正数，收到 {expire}");
    Ok(TenantTokenGrant {
        tenant_access_token,
        expire,
    })
}

/// 缓存 TTL = `expire - 300`（秒）。
///
/// 下界**必须自己守**：`RedisClient::setex` 不对秒数做正数校验，`SETEX key -N value`
/// 会变成 `DbError::RedisCommandError`（Server 类、不可重试）。那种失败的症状很难查
/// ——「锁拿到了、token 也换到了，却永远写不进缓存」，且下一轮重复发生、不自愈。
pub(crate) fn cache_ttl_seconds(expire: i64) -> anyhow::Result<i64> {
    ensure!(
        expire > CACHE_TTL_MARGIN_SECONDS,
        "tenant_access_token 的 expire 必须大于 {CACHE_TTL_MARGIN_SECONDS} 秒，收到 {expire}"
    );
    Ok(expire - CACHE_TTL_MARGIN_SECONDS)
}

/// token 缓存键。命名空间与授权版本缓存保持一致（`yang-system:{deployment}:...`）。
pub(crate) fn token_key(deployment: &str) -> String {
    format!("yang-system:{deployment}:feishu:tenant_token")
}

/// 刷新锁键。
pub(crate) fn lock_key(deployment: &str) -> String {
    format!("{}:lock", token_key(deployment))
}

/// 「比较并删除」放锁。
///
/// 裸 `DEL` 是不安全的：锁 TTL 一过，原持锁者放锁会删掉**别人**的锁，第三个实例
/// 随即又能抢锁，刷新风暴由此而来。`yang-db` 没有 compare-and-delete 原语，
/// 但 `script()` + `eval_script()` 已经够用，不需要改框架。
const RELEASE_LOCK_SCRIPT: &str = r"
if redis.call('GET', KEYS[1]) == ARGV[1] then
    return redis.call('DEL', KEYS[1])
end
return 0
";

// ---------------------------------------------------------------------------
// 缓存抽象
// ---------------------------------------------------------------------------

/// token 缓存与锁。
///
/// 抽成 trait 的**唯一目的**是可测性：无 Redis 也能断言「缓存命中不抢锁」
/// 「抢到锁后写入的 TTL」「他人 owner 放锁不删锁」这些并发语义。
#[async_trait::async_trait]
pub(crate) trait TenantTokenCache: Send + Sync {
    async fn get(&self) -> anyhow::Result<Option<String>>;
    async fn put(&self, token: &str, ttl_seconds: i64) -> anyhow::Result<()>;
    async fn invalidate(&self) -> anyhow::Result<()>;
    /// 抢锁；`owner` 是本次获取的唯一标识，放锁时用它做比较。
    async fn acquire_lock(&self, owner: &str, ttl_seconds: i64) -> anyhow::Result<bool>;
    async fn release_lock(&self, owner: &str) -> anyhow::Result<()>;
}

/// 生产实现。
pub(crate) struct RedisTenantTokenCache {
    redis: RedisClient,
    key: String,
    lock_key: String,
}

impl RedisTenantTokenCache {
    pub(crate) fn new(redis: RedisClient, deployment: &str) -> Self {
        Self {
            redis,
            key: token_key(deployment),
            lock_key: lock_key(deployment),
        }
    }
}

#[async_trait::async_trait]
impl TenantTokenCache for RedisTenantTokenCache {
    async fn get(&self) -> anyhow::Result<Option<String>> {
        self.redis
            .get(self.key.as_str())
            .await
            .context("读取 tenant_access_token 缓存失败")
    }

    async fn put(&self, token: &str, ttl_seconds: i64) -> anyhow::Result<()> {
        ensure!(ttl_seconds > 0, "tenant_access_token 缓存 TTL 必须为正数");
        // 形参顺序是 setex(key, **seconds**, value)。
        self.redis
            .setex(self.key.as_str(), ttl_seconds, token)
            .await
            .context("写入 tenant_access_token 缓存失败")
    }

    async fn invalidate(&self) -> anyhow::Result<()> {
        // del 的形参是切片，单键也要写成 &[key]。
        self.redis
            .del(std::slice::from_ref(&self.key))
            .await
            .context("清除 tenant_access_token 缓存失败")?;
        Ok(())
    }

    async fn acquire_lock(&self, owner: &str, ttl_seconds: i64) -> anyhow::Result<bool> {
        ensure!(ttl_seconds > 0, "tenant_access_token 锁 TTL 必须为正数");
        // 形参顺序是 set_nx_ex(key, **value**, ttl)——与本文件里 setex 的后两位恰好
        // 相反。写反了编译器全盘接受（都是 String/i64 能匹配），要到运行期才由
        // Redis 报「非法整数」。这类错位是本模块最容易踩的坑。
        self.redis
            .set_nx_ex(self.lock_key.as_str(), owner, ttl_seconds)
            .await
            .context("抢 tenant_access_token 刷新锁失败")
    }

    async fn release_lock(&self, owner: &str) -> anyhow::Result<()> {
        let script = self.redis.script(RELEASE_LOCK_SCRIPT);
        let _: i64 = self
            .redis
            .eval_script(
                &script,
                std::slice::from_ref(&self.lock_key),
                &[owner.to_string()],
            )
            .await
            .context("释放 tenant_access_token 刷新锁失败")?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 换取流程
// ---------------------------------------------------------------------------

/// 换取并缓存 `tenant_access_token`。
pub(crate) struct TenantTokenProvider {
    cache: Arc<dyn TenantTokenCache>,
    transport: Arc<dyn OutboundTransport>,
    sleeper: Arc<dyn Sleeper>,
    credentials: FeishuCredentials,
    deployment: String,
}

impl TenantTokenProvider {
    pub(crate) fn new(
        cache: Arc<dyn TenantTokenCache>,
        transport: Arc<dyn OutboundTransport>,
        sleeper: Arc<dyn Sleeper>,
        credentials: FeishuCredentials,
        deployment: &str,
    ) -> anyhow::Result<Self> {
        // 复用授权版本缓存的命名空间校验：键形状必须是 `[a-z0-9-]`，
        // 否则一个含冒号的 deployment 会悄悄改写缓存键的层级结构。
        crate::authorization::validate_deployment_name(deployment)?;
        Ok(Self {
            cache,
            transport,
            sleeper,
            credentials,
            deployment: deployment.to_string(),
        })
    }

    /// 取一个可用的 token。
    ///
    /// 快路径是纯读缓存（绝大多数轮次走这条，不抢锁）。未命中才进入
    /// 「抢锁 → 二次读 → 换取 → 写缓存 → 放锁」。
    pub(crate) async fn tenant_access_token(&self) -> anyhow::Result<String> {
        if let Some(token) = self.read_cached().await? {
            return Ok(token);
        }

        // owner 每次获取都新生成：锁值是「本次获取」的标识，不是「本实例」的标识。
        // 复用同一个值会让一个实例能删掉自己稍后那次获取的锁（TTL 已过时尤其危险）。
        let owner = uuid::Uuid::new_v4().to_string();

        let locked = match self.cache.acquire_lock(&owner, LOCK_TTL_SECONDS).await {
            Ok(locked) => locked,
            Err(error) => {
                // Redis 故障降级到权威事实源（与 `request_validator` 的既有惯例一致），
                // 但要打点：否则「缓存未命中」与「Redis 坏了」在监控上无法区分。
                tracing::warn!(
                    error = %error,
                    deployment = %self.deployment,
                    "抢 token 刷新锁失败，降级为直连换取一次"
                );
                metrics::counter!("feishu_outbound_token_total", "result" => "lock_error")
                    .increment(1);
                return self.issue_and_cache(false).await;
            }
        };

        if !locked {
            // 锁被别的实例持有。锁 TTL(30s) 覆盖单次刷新最坏耗时(19s)，因此等到的一定
            // 是对方刚写完的值。
            for _ in 0..LOCK_WAIT_ATTEMPTS {
                self.sleeper.sleep(LOCK_WAIT_INTERVAL).await;
                if let Some(token) = self.read_cached().await? {
                    metrics::counter!("feishu_outbound_token_total", "result" => "waited")
                        .increment(1);
                    return Ok(token);
                }
            }
            // 仍未命中，说明缓存里**本来就没有值**（而不是「有旧值但别人在刷」）：
            // 再等下去没有意义。降级为自己换一次且**不写缓存**——写缓存会与持锁者
            // 竞争同一个键，而这件事的代价只是短暂并存两个有效 token（官方语义合法）。
            tracing::warn!(
                deployment = %self.deployment,
                "等待 token 刷新超时，降级为自行换取一次（不写缓存）"
            );
            metrics::counter!("feishu_outbound_token_total", "result" => "lock_wait_timeout")
                .increment(1);
            return self.issue_and_cache(false).await;
        }

        // 持锁后二次读：从「第一次读缓存」到「抢到锁」之间，可能已有别的持有者写入。
        let outcome = match self.read_cached().await? {
            Some(token) => Ok(token),
            None => self.issue_and_cache(true).await,
        };
        self.release_lock(&owner).await;
        outcome
    }

    /// 遇到 token 失效业务码时的补救：清缓存 + 强制刷新**一次**。
    ///
    /// 只做一次是刻意的：第二次仍失效说明问题不在缓存陈旧，而在凭证本身
    /// （应用停用、secret 轮换过、租户不匹配），继续换只会掩盖真因。
    pub(crate) async fn invalidate_and_refresh(&self) -> anyhow::Result<String> {
        if let Err(error) = self.cache.invalidate().await {
            // 删失败不阻断：紧接着的强制刷新会覆写同一个键，陈旧值不会被再读走。
            tracing::warn!(error = %error, "清除 token 缓存失败，继续强制刷新");
        }
        metrics::counter!("feishu_outbound_token_total", "result" => "invalidated").increment(1);
        self.issue_and_cache(true).await
    }

    async fn read_cached(&self) -> anyhow::Result<Option<String>> {
        match self.cache.get().await {
            // 空串按未命中处理：一个空值写进缓存后若被当命中，出站请求会带着
            // `Authorization: Bearer ` 出去，症状是「每个请求都 400 且看不出原因」。
            Ok(Some(token)) => Ok(Some(token).filter(|token| !token.trim().is_empty())),
            Ok(None) => Ok(None),
            Err(error) => {
                // 缓存故障不停机：降级为直连换取。
                tracing::warn!(
                    error = %error,
                    deployment = %self.deployment,
                    "读取 token 缓存失败，降级为直连换取"
                );
                metrics::counter!("feishu_outbound_redis_total", "reason" => "get_failed")
                    .increment(1);
                Ok(None)
            }
        }
    }

    fn token_request(&self) -> OutboundRequest {
        OutboundRequest {
            method: OutboundMethod::Post,
            url: format!("{FEISHU_OPEN_BASE}{TENANT_TOKEN_PATH}"),
            query: Vec::new(),
            // 该接口的凭证在请求体里，**不带** Authorization 头。
            bearer_token: None,
            json_body: Some(serde_json::json!({
                "app_id": self.credentials.app_id,
                "app_secret": self.credentials.app_secret,
            })),
            timeout_secs: Some(TOKEN_REQUEST_TIMEOUT_SECS),
            // 幂等：飞书换 token 时新旧并存，重发不会破坏任何状态。
            idempotent: true,
        }
    }

    async fn issue_and_cache(&self, cache: bool) -> anyhow::Result<String> {
        let response = send_with_retry(
            self.transport.as_ref(),
            self.sleeper.as_ref(),
            &self.token_request(),
            TOKEN_RETRY,
        )
        .await
        .map_err(|failure| anyhow::anyhow!("{failure}"))
        .context("请求 tenant_access_token 失败")?;

        let grant = parse_token_grant(&response.body)?;
        if cache {
            // 下界在 cache_ttl_seconds 里守，写缓存的所有路径都必经它。
            let ttl = cache_ttl_seconds(grant.expire)?;
            self.cache
                .put(&grant.tenant_access_token, ttl)
                .await
                .context("写入 tenant_access_token 缓存失败")?;
            metrics::counter!("feishu_outbound_token_total", "result" => "refreshed").increment(1);
        }
        Ok(grant.tenant_access_token)
    }

    async fn release_lock(&self, owner: &str) {
        // 放锁失败不覆盖业务结果：锁本身有 TTL 兜底，最坏是让别人多等一个 TTL。
        if let Err(error) = self.cache.release_lock(owner).await {
            tracing::warn!(error = %error, "释放 token 刷新锁失败（锁有 TTL 兜底）");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::feishu::domain::outbound::OutboundResponse;
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use yang_base::BaseError;

    // ---- 纯函数 ----

    #[test]
    fn token_response_is_flat_not_data_wrapped() {
        // 官方示例就是扁平信封；套 data 会让 tenant_access_token 恒为 None
        let grant = parse_token_grant(
            r#"{"code":0,"msg":"ok","tenant_access_token":"t-abc","expire":7200}"#,
        )
        .unwrap_or_else(|error| panic!("扁平信封必须可解析: {error}"));
        assert_eq!(grant.tenant_access_token, "t-abc");
        assert_eq!(grant.expire, 7200);

        let wrapped = parse_token_grant(
            r#"{"code":0,"msg":"ok","data":{"tenant_access_token":"t-abc","expire":7200}}"#,
        );
        assert!(wrapped.is_err(), "data 包裹的形态不该被接受");
    }

    #[test]
    fn non_zero_code_is_rejected() {
        let error = parse_token_grant(r#"{"code":10003,"msg":"invalid app_secret"}"#)
            .err()
            .unwrap_or_else(|| panic!("业务失败必须报错"));
        let text = error.to_string();
        assert!(text.contains("10003"), "错误里必须带业务码: {text}");
    }

    #[test]
    fn missing_fields_are_reported_separately_from_a_rejected_request() {
        // 「飞书拒绝」与「我们理解错了契约」是两种问题，排查方向不同
        let no_token = parse_token_grant(r#"{"code":0,"msg":"ok","expire":7200}"#)
            .err()
            .unwrap_or_else(|| panic!("缺 tenant_access_token 必须报错"));
        assert!(
            no_token.to_string().contains("tenant_access_token"),
            "实际: {no_token}"
        );

        let no_expire = parse_token_grant(r#"{"code":0,"msg":"ok","tenant_access_token":"t-x"}"#)
            .err()
            .unwrap_or_else(|| panic!("缺 expire 必须报错"));
        assert!(
            no_expire.to_string().contains("expire"),
            "实际: {no_expire}"
        );
    }

    #[test]
    fn blank_token_is_treated_as_missing() {
        assert!(parse_token_grant(
            r#"{"code":0,"msg":"ok","tenant_access_token":"   ","expire":7200}"#
        )
        .is_err());
    }

    #[test]
    fn non_positive_expire_is_rejected() {
        for expire in [0, -1] {
            let body =
                format!(r#"{{"code":0,"msg":"ok","tenant_access_token":"t-x","expire":{expire}}}"#);
            assert!(
                parse_token_grant(&body).is_err(),
                "expire={expire} 必须被拒绝"
            );
        }
    }

    #[test]
    fn cache_ttl_keeps_the_official_thirty_minute_margin() {
        assert_eq!(
            cache_ttl_seconds(7200).unwrap_or_else(|error| panic!("7200 应有效: {error}")),
            6900
        );
        // 边界：恰好 300 秒不满足「严格大于」，必须拒绝——否则 TTL 会变成 0
        assert!(cache_ttl_seconds(300).is_err());
        assert!(cache_ttl_seconds(299).is_err());
        assert_eq!(
            cache_ttl_seconds(301).unwrap_or_else(|error| panic!("301 应有效: {error}")),
            1
        );
    }

    #[test]
    fn cache_keys_are_namespaced_by_deployment() {
        assert_eq!(
            token_key("prod-cn"),
            "yang-system:prod-cn:feishu:tenant_token"
        );
        assert_eq!(
            lock_key("prod-cn"),
            "yang-system:prod-cn:feishu:tenant_token:lock"
        );
        assert_ne!(token_key("a"), token_key("b"));
    }

    #[test]
    fn credentials_debug_never_leaks_the_secret() {
        let credentials = FeishuCredentials {
            app_id: "cli_visible".to_string(),
            app_secret: "super-secret-value".to_string(),
        };
        let rendered = format!("{credentials:?}");
        assert!(
            rendered.contains("cli_visible"),
            "app_id 可以出现: {rendered}"
        );
        assert!(
            !rendered.contains("super-secret-value"),
            "app_secret 绝不能出现在 Debug 输出里: {rendered}"
        );
    }

    #[test]
    fn deployment_name_shape_is_validated() {
        assert!(crate::authorization::validate_deployment_name("prod-cn-1").is_ok());
        for deployment in ["", "UPPER", "contains_space", "slash/value", "colon:value"] {
            assert!(
                crate::authorization::validate_deployment_name(deployment).is_err(),
                "deployment={deployment:?} 必须被拒绝"
            );
        }
    }

    // ---- 假件 ----

    fn grant_body(token: &str, expire: i64) -> OutboundResponse {
        OutboundResponse {
            status: 200,
            body: format!(
                r#"{{"code":0,"msg":"ok","tenant_access_token":"{token}","expire":{expire}}}"#
            ),
            headers: BTreeMap::new(),
        }
    }

    struct ScriptedTransport {
        responses: Mutex<std::collections::VecDeque<Result<OutboundResponse, BaseError>>>,
        calls: Mutex<usize>,
    }

    impl ScriptedTransport {
        fn new(responses: Vec<Result<OutboundResponse, BaseError>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                calls: Mutex::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.lock().map(|count| *count).unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl OutboundTransport for ScriptedTransport {
        async fn send(&self, _request: OutboundRequest) -> Result<OutboundResponse, BaseError> {
            if let Ok(mut calls) = self.calls.lock() {
                *calls += 1;
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

    struct CountingSleeper {
        count: Mutex<u32>,
    }

    impl CountingSleeper {
        fn new() -> Self {
            Self {
                count: Mutex::new(0),
            }
        }

        fn count(&self) -> u32 {
            self.count.lock().map(|count| *count).unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl Sleeper for CountingSleeper {
        async fn sleep(&self, _duration: Duration) {
            if let Ok(mut count) = self.count.lock() {
                *count += 1;
            }
        }
    }

    /// 内存版缓存，记录写入的 TTL 与放锁用的 owner。
    #[derive(Default)]
    struct InMemoryCache {
        value: Mutex<Option<String>>,
        lock_owner: Mutex<Option<String>>,
        /// `None` = 本次抢锁失败（已有主）。
        lock_acquirable: Mutex<bool>,
        put_ttls: Mutex<Vec<i64>>,
        released_owners: Mutex<Vec<String>>,
        invalidations: Mutex<u32>,
    }

    impl InMemoryCache {
        fn with_value(token: &str) -> Self {
            let cache = Self::default();
            if let Ok(mut value) = cache.value.lock() {
                *value = Some(token.to_string());
            }
            cache
        }

        fn empty_acquirable() -> Self {
            let cache = Self::default();
            if let Ok(mut acquirable) = cache.lock_acquirable.lock() {
                *acquirable = true;
            }
            cache
        }

        fn pre_locked() -> Self {
            Self::default() // lock_acquirable 默认 false
        }

        fn put_ttls(&self) -> Vec<i64> {
            self.put_ttls
                .lock()
                .map(|ttls| ttls.clone())
                .unwrap_or_default()
        }

        fn released_owners(&self) -> Vec<String> {
            self.released_owners
                .lock()
                .map(|owners| owners.clone())
                .unwrap_or_default()
        }

        fn invalidations(&self) -> u32 {
            self.invalidations
                .lock()
                .map(|count| *count)
                .unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl TenantTokenCache for InMemoryCache {
        async fn get(&self) -> anyhow::Result<Option<String>> {
            Ok(self
                .value
                .lock()
                .map(|value| value.clone())
                .unwrap_or_default())
        }

        async fn put(&self, token: &str, ttl_seconds: i64) -> anyhow::Result<()> {
            if let Ok(mut value) = self.value.lock() {
                *value = Some(token.to_string());
            }
            if let Ok(mut ttls) = self.put_ttls.lock() {
                ttls.push(ttl_seconds);
            }
            Ok(())
        }

        async fn invalidate(&self) -> anyhow::Result<()> {
            if let Ok(mut value) = self.value.lock() {
                *value = None;
            }
            if let Ok(mut count) = self.invalidations.lock() {
                *count += 1;
            }
            Ok(())
        }

        async fn acquire_lock(&self, owner: &str, _ttl_seconds: i64) -> anyhow::Result<bool> {
            let acquirable = self
                .lock_acquirable
                .lock()
                .map(|value| *value)
                .unwrap_or_default();
            if !acquirable {
                return Ok(false);
            }
            if let Ok(mut holder) = self.lock_owner.lock() {
                *holder = Some(owner.to_string());
            }
            Ok(true)
        }

        async fn release_lock(&self, owner: &str) -> anyhow::Result<()> {
            if let Ok(mut owners) = self.released_owners.lock() {
                owners.push(owner.to_string());
            }
            Ok(())
        }
    }

    fn provider(
        cache: Arc<InMemoryCache>,
        transport: Arc<ScriptedTransport>,
        sleeper: Arc<CountingSleeper>,
    ) -> TenantTokenProvider {
        TenantTokenProvider::new(
            cache,
            transport,
            sleeper,
            FeishuCredentials {
                app_id: "cli_test".to_string(),
                app_secret: "secret".to_string(),
            },
            "test-dep",
        )
        .unwrap_or_else(|error| panic!("provider 应可构建: {error}"))
    }

    // ---- 流程 ----

    #[tokio::test]
    async fn cache_hit_never_touches_the_network_or_the_lock() {
        let cache = Arc::new(InMemoryCache::with_value("t-cached"));
        let transport = Arc::new(ScriptedTransport::new(Vec::new()));
        let sleeper = Arc::new(CountingSleeper::new());
        let provider = provider(
            Arc::clone(&cache),
            Arc::clone(&transport),
            Arc::clone(&sleeper),
        );

        let token = provider
            .tenant_access_token()
            .await
            .unwrap_or_else(|error| panic!("应命中缓存: {error}"));

        assert_eq!(token, "t-cached");
        assert_eq!(transport.call_count(), 0, "命中缓存不得出网");
        assert_eq!(sleeper.count(), 0, "命中缓存不得等待");
        assert!(cache.put_ttls().is_empty(), "命中缓存不得重写");
    }

    #[tokio::test]
    async fn fresh_fetch_caches_with_expire_minus_300_and_releases_the_lock() {
        let cache = Arc::new(InMemoryCache::empty_acquirable());
        let transport = Arc::new(ScriptedTransport::new(vec![Ok(grant_body("t-new", 7200))]));
        let sleeper = Arc::new(CountingSleeper::new());
        let provider = provider(
            Arc::clone(&cache),
            Arc::clone(&transport),
            Arc::clone(&sleeper),
        );

        let token = provider
            .tenant_access_token()
            .await
            .unwrap_or_else(|error| panic!("应换取成功: {error}"));

        assert_eq!(token, "t-new");
        assert_eq!(transport.call_count(), 1);
        assert_eq!(cache.put_ttls(), vec![6900], "TTL 必须是 expire - 300");
        let released = cache.released_owners();
        assert_eq!(released.len(), 1, "必须放锁");
        assert!(!released[0].is_empty(), "放锁必须带 owner");
    }

    #[tokio::test]
    async fn waiting_for_another_holder_then_adopting_its_value() {
        // 抢锁失败 → 等待后读到对方写的值。这里用「第一次读命中」模拟对方已写完。
        let cache = Arc::new(InMemoryCache::with_value("t-from-peer"));
        let transport = Arc::new(ScriptedTransport::new(Vec::new()));
        let sleeper = Arc::new(CountingSleeper::new());
        let provider = provider(
            Arc::clone(&cache),
            Arc::clone(&transport),
            Arc::clone(&sleeper),
        );

        let token = provider
            .tenant_access_token()
            .await
            .unwrap_or_else(|error| panic!("应读到值: {error}"));
        assert_eq!(token, "t-from-peer");
        assert_eq!(transport.call_count(), 0);
    }

    #[tokio::test]
    async fn lock_wait_timeout_degrades_without_writing_the_cache() {
        // 抢锁失败且缓存始终为空：等待耗尽后自行换取一次，但**不写缓存**
        // （写缓存会与持锁者竞争同一个键）
        let cache = Arc::new(InMemoryCache::pre_locked());
        let transport = Arc::new(ScriptedTransport::new(vec![Ok(grant_body(
            "t-fallback",
            7200,
        ))]));
        let sleeper = Arc::new(CountingSleeper::new());
        let provider = provider(
            Arc::clone(&cache),
            Arc::clone(&transport),
            Arc::clone(&sleeper),
        );

        let token = provider
            .tenant_access_token()
            .await
            .unwrap_or_else(|error| panic!("应降级换取: {error}"));

        assert_eq!(token, "t-fallback");
        assert_eq!(sleeper.count(), u32::from(LOCK_WAIT_ATTEMPTS), "等待轮次");
        assert_eq!(transport.call_count(), 1);
        assert!(cache.put_ttls().is_empty(), "降级路径不得写缓存");
        assert!(cache.released_owners().is_empty(), "没抢到锁就不该放锁");
    }

    #[tokio::test]
    async fn invalidate_and_refresh_clears_then_forces_a_new_token() {
        let cache = Arc::new(InMemoryCache::with_value("t-stale"));
        let transport = Arc::new(ScriptedTransport::new(vec![Ok(grant_body(
            "t-fresh", 7200,
        ))]));
        let sleeper = Arc::new(CountingSleeper::new());
        let provider = provider(
            Arc::clone(&cache),
            Arc::clone(&transport),
            Arc::clone(&sleeper),
        );

        let token = provider
            .invalidate_and_refresh()
            .await
            .unwrap_or_else(|error| panic!("应强制刷新: {error}"));

        assert_eq!(token, "t-fresh");
        assert_eq!(cache.invalidations(), 1, "必须先清缓存");
        assert_eq!(transport.call_count(), 1);
        // 强制刷新绕过锁：若它还去抢锁，持锁者异常退出时这里会一起卡住
        assert_eq!(cache.put_ttls(), vec![6900], "刷新结果要写回缓存");
    }

    #[tokio::test]
    async fn blank_cached_value_is_treated_as_a_miss() {
        // 空串若被当命中，出站请求会带 `Authorization: Bearer ` 出去
        let cache = Arc::new(InMemoryCache::with_value("   "));
        let transport = Arc::new(ScriptedTransport::new(vec![Ok(grant_body("t-real", 7200))]));
        let sleeper = Arc::new(CountingSleeper::new());
        let provider = provider(
            Arc::clone(&cache),
            Arc::clone(&transport),
            Arc::clone(&sleeper),
        );

        let token = provider
            .tenant_access_token()
            .await
            .unwrap_or_else(|error| panic!("应重新换取: {error}"));
        assert_eq!(token, "t-real");
        assert_eq!(transport.call_count(), 1);
    }

    #[tokio::test]
    async fn rejected_grant_surfaces_the_business_code() {
        let cache = Arc::new(InMemoryCache::empty_acquirable());
        let transport = Arc::new(ScriptedTransport::new(vec![Ok(OutboundResponse {
            status: 200,
            body: r#"{"code":10003,"msg":"invalid app_secret"}"#.to_string(),
            headers: BTreeMap::new(),
        })]));
        let sleeper = Arc::new(CountingSleeper::new());
        let provider = provider(
            Arc::clone(&cache),
            Arc::clone(&transport),
            Arc::clone(&sleeper),
        );

        let error = provider
            .tenant_access_token()
            .await
            .err()
            .unwrap_or_else(|| panic!("业务失败必须冒泡"));
        let text = format!("{error:#}");
        assert!(text.contains("10003"), "必须带上业务码: {text}");
        assert!(cache.put_ttls().is_empty(), "失败不得写缓存");
        // 失败也要放锁，否则锁要等满 TTL 才释放
        assert_eq!(cache.released_owners().len(), 1, "失败路径同样要放锁");
    }
}

//! 每次真实副作用前先记阶段；没有远端确认时禁止 transport 自动重发。

use super::super::{context::FeishuContext, outbound::*};
use super::{begin_effect, confirm_effect, facts, mark_unknown, TaskClaim};
use yang_base::BaseError;

pub(crate) struct ExecutionTransport<'a> {
    pub(crate) inner: &'a dyn OutboundTransport,
    pub(crate) context: &'a FeishuContext,
    pub(crate) claim: &'a TaskClaim,
}

#[async_trait::async_trait]
impl OutboundTransport for ExecutionTransport<'_> {
    async fn send(&self, request: OutboundRequest) -> Result<OutboundResponse, BaseError> {
        let create = request.method == OutboundMethod::Post
            && request.url.ends_with("/approval/v4/instances");
        let backfill = request.method == OutboundMethod::Post
            && request.url.ends_with("/records/batch_update");
        if !create && !backfill {
            return self.inner.send(request).await;
        }
        let storage = |error: anyhow::Error| BaseError::ConfigError(error.to_string());
        let (_, instance) = facts(self.context, self.claim).await.map_err(storage)?;
        begin_effect(
            self.context,
            self.claim,
            if create { "creating" } else { "backfilling" },
        )
        .await
        .map_err(storage)?;
        let result = self.inner.send(request).await;
        let safe_phase = if instance.is_some() {
            "created"
        } else {
            "ready"
        };
        match result {
            Ok(response) => {
                let json = serde_json::from_str::<serde_json::Value>(&response.body).ok();
                let code = json.as_ref().and_then(|v| v["code"].as_i64());
                if code == Some(0) && (200..300).contains(&response.status) {
                    let created = if create {
                        json.as_ref()
                            .and_then(|v| v["data"]["instance_code"].as_str())
                            .filter(|s| !s.trim().is_empty())
                    } else {
                        None
                    };
                    if !create || created.is_some() {
                        confirm_effect(
                            self.context,
                            self.claim,
                            if create { "created" } else { safe_phase },
                            created,
                        )
                        .await
                        .map_err(storage)?;
                        return Ok(response);
                    }
                } else if create && code == Some(i64::from(CODE_UUID_CONFLICT)) {
                    confirm_effect(self.context, self.claim, "lookup", None)
                        .await
                        .map_err(storage)?;
                    return Ok(response);
                } else if code.is_some_and(|code| {
                    CODES_TOKEN_EXPIRED
                        .iter()
                        .any(|known| i64::from(*known) == code)
                        || [CODE_RATE_LIMITED, CODE_TOO_MANY_REQUEST]
                            .iter()
                            .any(|known| i64::from(*known) == code)
                }) {
                    // 上游明确拒绝鉴权/频控，未执行写入，允许有界重试。
                    confirm_effect(self.context, self.claim, safe_phase, None)
                        .await
                        .map_err(storage)?;
                    return Ok(response);
                } else if code.is_some_and(|code| code != 0) {
                    // 非零业务码 = 远端**明确拒绝**：响应来自飞书本身、请求已被解析
                    // 并挡下，写入没有发生。**不能只认 4xx**——飞书的大量业务错误走
                    // HTTP 200 + 非零 code（见本模块文档第一条），只认 4xx 会把它们
                    // 静默推进 review：任务永久卡住、真实错误码丢成 900001
                    // （2026-10-26 真机是 4xx 形态；2026-10-28 真机以 200 形态复发：
                    // 同一配置 5 条记录转 review，飞书侧实际零实例）。恢复安全阶段并
                    // 原样上抛，让 `classify` 给出真实错误码。
                    confirm_effect(self.context, self.claim, safe_phase, None)
                        .await
                        .map_err(storage)?;
                    tracing::warn!(
                        http_status = response.status,
                        business_code = code,
                        "飞书写入被远端明确拒绝（未执行），交上层按业务错误处置"
                    );
                    return Ok(response);
                }
                // 走到这里 = 响应拿到了但形态无法确认写入结果（非 JSON、HTTP 非 2xx
                // 且无业务码、或 2xx + code=0 却缺 instance_code）。转 review 之后
                // 没有任何原始信息可复核——2026-10-28 真机 5 条 review 连响应原文都
                // 追不到，所以把状态码与响应体前缀留在这里。
                tracing::warn!(
                    response = %summarize(response.status, &response.body),
                    "飞书写入响应无法确认，转人工核查"
                );
            }
            Err(error @ BaseError::RateLimitExceeded { .. }) => {
                // 本地配额在连接前拒绝，不属于未知外部副作用。
                confirm_effect(self.context, self.claim, safe_phase, None)
                    .await
                    .map_err(storage)?;
                return Err(error);
            }
            Err(error) => {
                // 传输层错误才是真正的「结果未知」：请求可能已到飞书，也可能没有。
                // 不记录它会让 review 的原因无从追溯（这里原本是 `Err(_) => {}`）。
                tracing::warn!(error = %error, "飞书写入未获得传输层确认，结果未知");
            }
        }
        mark_unknown(self.context, self.claim)
            .await
            .map_err(storage)?;
        Err(BaseError::ConfigError(
            "飞书写入未获得有效确认，任务已转人工核查，禁止自动重发".into(),
        ))
    }
}

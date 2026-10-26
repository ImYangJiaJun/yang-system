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
                }
            }
            Err(error @ BaseError::RateLimitExceeded { .. }) => {
                // 本地配额在连接前拒绝，不属于未知外部副作用。
                confirm_effect(self.context, self.claim, safe_phase, None)
                    .await
                    .map_err(storage)?;
                return Err(error);
            }
            Err(_) => {}
        }
        mark_unknown(self.context, self.claim)
            .await
            .map_err(storage)?;
        Err(BaseError::ConfigError(
            "飞书写入未获得有效确认，任务已转人工核查，禁止自动重发".into(),
        ))
    }
}

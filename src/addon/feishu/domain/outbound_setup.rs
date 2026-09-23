//! 出站调用的共享构造：凭证 + token 提供者 + transport + sleeper。
//!
//! 控制台里凡是要**真调飞书**的 Action（列出数据表 / 列出视图 / 列出字段、体检）
//! 都需要同一套设施。此前只有 `pull_probe` 与 `feishu_pull` 两处各写一遍；
//! 本次新增的元数据端点会再多几处，所以收敛到这里。
//!
//! 并未回改 `pull_probe`：那是无测试覆盖的热路径，留给退役字段级入口那一步一起动。

use std::sync::Arc;

use yang_base::action::ActionContext;
use yang_base::BaseError;

use crate::config::FeishuSettings;

use super::outbound::{
    HttpClientTransport, OutboundFailure, OutboundTransport, Sleeper, TokioSleeper,
};
use super::tenant_token::{FeishuCredentials, RedisTenantTokenCache, TenantTokenProvider};

/// 一次出站调用所需的设施。
pub(crate) struct Outbound {
    transport: Arc<HttpClientTransport>,
    sleeper: Arc<TokioSleeper>,
    pub(crate) tokens: TenantTokenProvider,
}

impl Outbound {
    /// HTTP 传输；调用方的出站函数收 `&dyn OutboundTransport`。
    pub(crate) fn transport(&self) -> &dyn OutboundTransport {
        self.transport.as_ref()
    }

    /// 退避器；调用方的出站函数收 `&dyn Sleeper`。
    pub(crate) fn sleeper(&self) -> &dyn Sleeper {
        self.sleeper.as_ref()
    }
}

/// 组装出站设施。
///
/// # 调用前提
///
/// `settings.can_pull()` 必须为真：凭证是占位值时换不到 token，早失败比让飞书
/// 回一个含糊的错误码好。调用方负责这一步并给出可归因的失败。
pub(crate) fn build(ctx: &ActionContext, settings: &FeishuSettings) -> Result<Outbound, BaseError> {
    let app_id = settings.app_id.clone().unwrap_or_default();
    let app_secret = settings.app_secret.clone().unwrap_or_default();
    // 部署命名空间复用授权缓存那一份，而不是在 [feishu] 段再配一个：
    // 同一个部署在缓存层必须是同一个键空间，两份配置迟早会漂移。
    let deployment = ctx
        .tools()
        .extension::<crate::authorization::AuthorizationVersionCache>()?
        .deployment()
        .to_string();

    let cache = Arc::new(RedisTenantTokenCache::new(
        ctx.tools().cache()?.clone(),
        &deployment,
    ));
    let transport = Arc::new(HttpClientTransport::new(ctx.tools().http()?.clone()));
    let sleeper = Arc::new(TokioSleeper);
    let tokens = TenantTokenProvider::new(
        cache,
        transport.clone(),
        sleeper.clone(),
        FeishuCredentials { app_id, app_secret },
        &deployment,
    )
    .map_err(|error| BaseError::ConfigError(error.to_string()))?;

    Ok(Outbound {
        transport,
        sleeper,
        tokens,
    })
}

/// 出站失败 → 框架错误。
///
/// 用 `ParamInvalid` 而不是 HTTP 5xx：这一层的失败几乎都是**配置或权限**问题
/// （坐标写错、没给应用加文档权限、凭证不对），返回可读文案比返回 500 更能定位。
pub(crate) fn outbound_error(failure: OutboundFailure) -> BaseError {
    BaseError::ParamInvalid("feishu".to_string(), failure.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::feishu::domain::outbound::FailureKind;

    #[test]
    fn outbound_failures_become_readable_param_errors() {
        // 归因文案必须原样带出来——它是运维唯一的线索
        let error = outbound_error(OutboundFailure {
            kind: FailureKind::Fatal { code: 1254302 },
            message: "调用身份缺少多维表格的高级权限".to_string(),
        });
        match error {
            BaseError::ParamInvalid(field, message) => {
                assert_eq!(field, "feishu");
                assert!(
                    message.contains("1254302") || message.contains("高级权限"),
                    "实际: {message}"
                );
            }
            other => panic!("出站失败应映射为 ParamInvalid，实际: {other:?}"),
        }
    }
}

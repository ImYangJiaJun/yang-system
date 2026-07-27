//! Access Token 授权版本的新鲜度校验。

use async_trait::async_trait;
use std::cmp::Ordering;
use yang_base::action::{ActionContext, TokenClaimsValidator};
use yang_base::token::TokenClaims;
use yang_base::BaseError;

use super::{AuthorizationVersionCache, CachedAuthorizationVersion};
use crate::modules::account;

const ACTIVE_STATUS: &str = "active";

#[derive(Clone)]
pub struct AuthorizationVersionValidator {
    cache: Option<AuthorizationVersionCache>,
}

impl AuthorizationVersionValidator {
    pub fn new(cache: Option<AuthorizationVersionCache>) -> Self {
        Self { cache }
    }
}

#[async_trait]
impl TokenClaimsValidator for AuthorizationVersionValidator {
    async fn validate(&self, ctx: &ActionContext, claims: &TokenClaims) -> Result<(), BaseError> {
        let user_id = match positive_claim(claims.sub.parse::<i64>().ok()) {
            Some(user_id) => user_id,
            None => {
                record_check("invalid", "token");
                tracing::warn!(
                    request_id = %ctx.request_id,
                    error_code = BaseError::AuthorizationVersionInvalid.code_str(),
                    "Access Token subject 不是正整数"
                );
                return Err(BaseError::AuthorizationVersionInvalid);
            }
        };
        let token_version = match positive_claim(
            claims
                .custom
                .get("authz_version")
                .and_then(serde_json::Value::as_i64),
        ) {
            Some(version) => version,
            None => {
                record_check("invalid", "token");
                tracing::warn!(
                    request_id = %ctx.request_id,
                    user_id,
                    error_code = BaseError::AuthorizationVersionInvalid.code_str(),
                    "Access Token authz_version 缺失或无效"
                );
                return Err(BaseError::AuthorizationVersionInvalid);
            }
        };
        let cache = match self.cache.as_ref() {
            Some(cache) => cache,
            None => {
                record_check("unavailable", "token");
                tracing::error!(
                    request_id = %ctx.request_id,
                    user_id,
                    token_version,
                    error_code = BaseError::AuthorizationCheckUnavailable.code_str(),
                    "Schema-only 应用不能处理认证请求"
                );
                return Err(BaseError::AuthorizationCheckUnavailable);
            }
        };

        let fallback_reason = match cache.read(user_id).await {
            Ok(CachedAuthorizationVersion::Version(current_version)) => {
                match compare_versions(token_version, current_version) {
                    VersionComparison::Match => {
                        record_check("match", "redis");
                        return Ok(());
                    }
                    VersionComparison::Stale => {
                        record_check("stale", "redis");
                        tracing::info!(
                            request_id = %ctx.request_id,
                            user_id,
                            token_version,
                            current_version,
                            error_code = BaseError::AuthorizationStale.code_str(),
                            "Redis 判定 Access Token 授权版本已过期"
                        );
                        return Err(BaseError::AuthorizationStale);
                    }
                    VersionComparison::CacheBehind => "cache_behind",
                }
            }
            Ok(CachedAuthorizationVersion::Missing) => "miss",
            Ok(CachedAuthorizationVersion::Malformed) => {
                tracing::warn!(
                    request_id = %ctx.request_id,
                    user_id,
                    token_version,
                    "Redis 授权版本损坏，降级查询 MySQL"
                );
                "malformed"
            }
            Err(error) => {
                tracing::warn!(
                    request_id = %ctx.request_id,
                    user_id,
                    token_version,
                    error = %error,
                    "读取 Redis 授权版本失败，降级查询 MySQL"
                );
                "redis_error"
            }
        };
        record_fallback(fallback_reason);

        let state =
            match account::find_authorization_version(ctx.tools().mysql()?.pool(), user_id).await {
                Ok(state) => state,
                Err(error) => {
                    record_check("unavailable", "mysql");
                    tracing::error!(
                        request_id = %ctx.request_id,
                        user_id,
                        token_version,
                        error_code = BaseError::AuthorizationCheckUnavailable.code_str(),
                        error = %error,
                        "MySQL 授权版本回源失败"
                    );
                    return Err(BaseError::AuthorizationCheckUnavailable);
                }
            };
        let (status, current_version) = match state {
            Some(state) => state,
            None => {
                record_check("invalid", "mysql");
                tracing::warn!(
                    request_id = %ctx.request_id,
                    user_id,
                    token_version,
                    error_code = BaseError::AuthorizationVersionInvalid.code_str(),
                    "Access Token 对应用户不存在"
                );
                return Err(BaseError::AuthorizationVersionInvalid);
            }
        };
        if status != ACTIVE_STATUS || current_version < 1 {
            record_check("invalid", "mysql");
            tracing::warn!(
                request_id = %ctx.request_id,
                user_id,
                token_version,
                current_version,
                status = %status,
                error_code = BaseError::AuthorizationVersionInvalid.code_str(),
                "Access Token 对应用户状态或授权版本无效"
            );
            return Err(BaseError::AuthorizationVersionInvalid);
        }

        match compare_versions(token_version, current_version) {
            VersionComparison::Match => {
                record_check("match", "mysql");
                if let Err(error) = cache.publish(user_id, current_version).await {
                    tracing::warn!(
                        request_id = %ctx.request_id,
                        user_id,
                        token_version,
                        current_version,
                        error = %error,
                        "MySQL 授权版本校验成功，但回填 Redis 失败"
                    );
                }
                Ok(())
            }
            VersionComparison::Stale => {
                record_check("stale", "mysql");
                tracing::info!(
                    request_id = %ctx.request_id,
                    user_id,
                    token_version,
                    current_version,
                    error_code = BaseError::AuthorizationStale.code_str(),
                    "MySQL 判定 Access Token 授权版本已过期"
                );
                Err(BaseError::AuthorizationStale)
            }
            VersionComparison::CacheBehind => {
                record_check("invalid", "mysql");
                tracing::error!(
                    request_id = %ctx.request_id,
                    user_id,
                    token_version,
                    current_version,
                    error_code = BaseError::AuthorizationVersionInvalid.code_str(),
                    "Access Token 授权版本领先于 MySQL 事实版本"
                );
                Err(BaseError::AuthorizationVersionInvalid)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VersionComparison {
    Match,
    Stale,
    CacheBehind,
}

fn compare_versions(token_version: i64, current_version: i64) -> VersionComparison {
    match token_version.cmp(&current_version) {
        Ordering::Equal => VersionComparison::Match,
        Ordering::Less => VersionComparison::Stale,
        Ordering::Greater => VersionComparison::CacheBehind,
    }
}

fn positive_claim(value: Option<i64>) -> Option<i64> {
    value.filter(|value| *value > 0)
}

fn record_check(result: &'static str, source: &'static str) {
    metrics::counter!(
        "authz_version_check_total",
        "result" => result,
        "source" => source
    )
    .increment(1);
}

fn record_fallback(reason: &'static str) {
    metrics::counter!("authz_version_fallback_total", "reason" => reason).increment(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison_preserves_fact_direction_at_i64_boundary() {
        assert_eq!(compare_versions(7, 7), VersionComparison::Match);
        assert_eq!(compare_versions(6, 7), VersionComparison::Stale);
        assert_eq!(compare_versions(8, 7), VersionComparison::CacheBehind);
        assert_eq!(
            compare_versions(i64::MAX - 1, i64::MAX),
            VersionComparison::Stale
        );
        assert_eq!(positive_claim(Some(1)), Some(1));
        assert_eq!(positive_claim(Some(i64::MAX)), Some(i64::MAX));
        for invalid in [None, Some(0), Some(-1)] {
            assert_eq!(positive_claim(invalid), None);
        }
    }
}

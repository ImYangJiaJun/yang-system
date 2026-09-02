//! Access Token 授权版本的新鲜度校验。

use async_trait::async_trait;
use std::cmp::Ordering;
use std::sync::Arc;
use yang_base::action::{ActionContext, TokenClaimsValidator};
use yang_base::token::TokenClaims;
use yang_base::BaseError;

use super::{AuthorizationVersionCache, AuthorizationVersionSource, CachedAuthorizationVersion};

#[derive(Clone)]
pub struct AuthorizationVersionValidator {
    cache: Option<AuthorizationVersionCache>,
    source: Option<Arc<dyn AuthorizationVersionSource>>,
}

impl AuthorizationVersionValidator {
    /// 组合根注入授权版本缓存与回源端口；两者缺失都在校验时 fail-closed。
    pub fn new(
        cache: Option<AuthorizationVersionCache>,
        source: Option<Arc<dyn AuthorizationVersionSource>>,
    ) -> Self {
        Self { cache, source }
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
        let source = match self.source.as_ref() {
            Some(source) => source,
            None => {
                record_check("unavailable", "token");
                tracing::error!(
                    request_id = %ctx.request_id,
                    user_id,
                    token_version,
                    error_code = BaseError::AuthorizationCheckUnavailable.code_str(),
                    "授权版本回源端口未装配，拒绝认证请求"
                );
                return Err(BaseError::AuthorizationCheckUnavailable);
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
        validate_via_source(ctx, source, Some(cache), user_id, token_version).await
    }
}

/// 经授权版本回源端口判定 Token 新鲜度；MySQL 是最终事实源。
///
/// 缓存只用于 Match 时的尽力回填（失败不致命）；测试在无 Redis 环境传 `None`。
async fn validate_via_source(
    ctx: &ActionContext,
    source: &Arc<dyn AuthorizationVersionSource>,
    cache: Option<&AuthorizationVersionCache>,
    user_id: i64,
    token_version: i64,
) -> Result<(), BaseError> {
    let snapshot = match source
        .find_authorization_version(ctx.tools().mysql()?.pool(), user_id)
        .await
    {
        Ok(snapshot) => snapshot,
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
    let snapshot = match snapshot {
        Some(snapshot) => snapshot,
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
    let current_version = snapshot.version();
    if !snapshot.is_active() || current_version < 1 {
        record_check("invalid", "mysql");
        tracing::warn!(
            request_id = %ctx.request_id,
            user_id,
            token_version,
            current_version,
            status = %snapshot.status(),
            error_code = BaseError::AuthorizationVersionInvalid.code_str(),
            "Access Token 对应用户状态或授权版本无效"
        );
        return Err(BaseError::AuthorizationVersionInvalid);
    }

    match compare_versions(token_version, current_version) {
        VersionComparison::Match => {
            record_check("match", "mysql");
            if let Some(cache) = cache {
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
    use crate::authorization::AuthorizationVersionSnapshot;
    use sqlx::mysql::MySqlPoolOptions;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use yang_base::action::Request;
    use yang_base::token::TokenType;
    use yang_base::tools::{Tools, ToolsBuilder};
    use yang_db::{Database, DatabaseConfig};

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

    fn test_tools() -> Arc<Tools> {
        let pool = MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"));
        let mysql = Database::from_pool(pool, DatabaseConfig::default())
            .unwrap_or_else(|error| panic!("测试 Database 应构建成功: {error}"));
        Arc::new(
            ToolsBuilder::new()
                .mysql(mysql)
                .build()
                .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}")),
        )
    }

    fn test_ctx() -> ActionContext {
        ActionContext::new(Request::new(serde_json::json!({})), test_tools())
    }

    fn test_claims(user_id: i64, authz_version: i64) -> TokenClaims {
        TokenClaims::new(
            "test",
            user_id.to_string(),
            "test-api",
            0,
            0,
            0,
            "jti",
            TokenType::Access,
            serde_json::json!({ "authz_version": authz_version }),
        )
    }

    struct FakeSource {
        calls: AtomicUsize,
        snapshot: Option<AuthorizationVersionSnapshot>,
    }

    #[async_trait]
    impl AuthorizationVersionSource for FakeSource {
        async fn find_authorization_version(
            &self,
            _pool: &sqlx::MySqlPool,
            _user_id: i64,
        ) -> Result<Option<AuthorizationVersionSnapshot>, BaseError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.snapshot.clone())
        }
    }

    fn fake_source(
        snapshot: Option<AuthorizationVersionSnapshot>,
    ) -> Arc<dyn AuthorizationVersionSource> {
        Arc::new(FakeSource {
            calls: AtomicUsize::new(0),
            snapshot,
        })
    }

    /// 回源端口未装配必须 fail-closed，且不触碰任何存储。
    #[tokio::test]
    async fn missing_source_port_fails_closed_before_any_storage() {
        let validator = AuthorizationVersionValidator::new(None, None);
        let result = validator.validate(&test_ctx(), &test_claims(7, 3)).await;
        assert!(
            matches!(result, Err(BaseError::AuthorizationCheckUnavailable)),
            "缺少回源端口必须拒绝认证: {result:?}"
        );
    }

    /// Schema-only 运行态（无缓存）在端口已装配时保持原有 fail-closed 语义。
    #[tokio::test]
    async fn missing_cache_still_fails_closed_when_source_is_wired() {
        let validator = AuthorizationVersionValidator::new(
            None,
            Some(fake_source(Some(AuthorizationVersionSnapshot::new(
                "active", true, 3,
            )))),
        );
        let result = validator.validate(&test_ctx(), &test_claims(7, 3)).await;
        assert!(
            matches!(result, Err(BaseError::AuthorizationCheckUnavailable)),
            "缺少缓存必须拒绝认证: {result:?}"
        );
    }

    /// 回源判定全部经端口实现驱动：match/stale/失效用户/版本回退方向。
    #[tokio::test]
    async fn fallback_decisions_are_driven_by_the_source_port() {
        let ctx = test_ctx();
        let source = Arc::new(FakeSource {
            calls: AtomicUsize::new(0),
            snapshot: Some(AuthorizationVersionSnapshot::new("active", true, 7)),
        });
        let trait_source: Arc<dyn AuthorizationVersionSource> = source.clone();

        let matched = validate_via_source(&ctx, &trait_source, None, 42, 7).await;
        assert!(matched.is_ok(), "版本一致必须放行: {matched:?}");
        assert_eq!(
            source.calls.load(Ordering::SeqCst),
            1,
            "回源必须经过注入的端口实现"
        );

        let stale = validate_via_source(&ctx, &trait_source, None, 42, 6).await;
        assert!(
            matches!(stale, Err(BaseError::AuthorizationStale)),
            "Token 版本落后于事实必须判定过期: {stale:?}"
        );

        let ahead = validate_via_source(&ctx, &trait_source, None, 42, 8).await;
        assert!(
            matches!(ahead, Err(BaseError::AuthorizationVersionInvalid)),
            "Token 版本领先于事实必须拒绝（版本不回退）: {ahead:?}"
        );

        let missing = fake_source(None);
        let missing_result = validate_via_source(&ctx, &missing, None, 42, 7).await;
        assert!(
            matches!(missing_result, Err(BaseError::AuthorizationVersionInvalid)),
            "用户不存在必须拒绝: {missing_result:?}"
        );

        let disabled = fake_source(Some(AuthorizationVersionSnapshot::new(
            "disabled", false, 7,
        )));
        let disabled_result = validate_via_source(&ctx, &disabled, None, 42, 7).await;
        assert!(
            matches!(disabled_result, Err(BaseError::AuthorizationVersionInvalid)),
            "停用用户即使版本一致也必须拒绝: {disabled_result:?}"
        );

        let invalid_version =
            fake_source(Some(AuthorizationVersionSnapshot::new("active", true, 0)));
        let invalid_version_result = validate_via_source(&ctx, &invalid_version, None, 42, 0).await;
        assert!(
            matches!(
                invalid_version_result,
                Err(BaseError::AuthorizationVersionInvalid)
            ),
            "事实版本无效必须拒绝: {invalid_version_result:?}"
        );
    }
}

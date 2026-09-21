//! 多维表格写入 API 的静态 Token 校验中间件。
//!
//! # 为什么是一个中间件，而不是普通受保护 Action
//!
//! 静态 Token 的调用方（飞书多维表格自动化工作流）没有 JWT，过不了框架的
//! `TokenAuthMiddleware`；而 `Middleware` 虽然**能读**身份
//! （`ActionContext::actor()` / `user_roles_set()` 是公开的），却**不能注入**身份
//! （`with_user` / `ctx.user` 是 `pub(crate)`）。因此无法让静态 Token 走受保护 Action，
//! 只能让 Action 保持 `public` 并由中间件承担凭证校验。
//!
//! # 漏挂的风险与缓解
//!
//! Action 是 `public`，中间件一旦漏挂就会裸奔。缓解是 `target_action()` 精确限定 +
//! 集成测试断言「无 Token 调用被拒」（见 Task 11）。这里不用 `MiddlewareScope::AllActions`
//! 全量拦截，是为了避免误伤同 module 的公开取选项端点——那个端点有自己的来源校验。

use async_trait::async_trait;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::ActionRef;
use yang_base::router::{Middleware, Next};
use yang_base::BaseError;

use super::token::{hash_token, verify_token};

/// 校验 `Authorization: Bearer <token>` 是否匹配管理 Token。
pub(crate) struct ManagementTokenMiddleware {
    token_hash: String,
    target: ActionRef,
}

impl ManagementTokenMiddleware {
    /// 绑定管理 Token 明文（立即哈希，不保留明文）与目标 Action。
    pub(crate) fn new(management_api_token: &str, target: ActionRef) -> Self {
        Self {
            token_hash: hash_token(management_api_token),
            target,
        }
    }
}

/// 从 `Authorization` 头取出 Bearer token。
///
/// scheme 大小写不敏感（RFC 7235 规定 scheme 不区分大小写），前后空白容忍。
/// 非 Bearer 方案一律返回 `None`——不做任何降级猜测。
fn extract_token(header: &str) -> Option<String> {
    // 先整体 trim：否则前导空白会让 split_once 切出空 scheme
    let (scheme, value) = header.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.to_string())
}

#[async_trait]
impl Middleware for ManagementTokenMiddleware {
    fn target_action(&self) -> Option<&ActionRef> {
        Some(&self.target)
    }

    async fn handle(&self, ctx: ActionContext, next: Next<'_>) -> Result<ApiResponse, BaseError> {
        let presented = ctx
            .request
            .get_header("authorization")
            .and_then(extract_token);
        let authorized = presented
            .as_deref()
            .is_some_and(|token| verify_token(token, &self.token_hash));
        if !authorized {
            // 业务失败走 Ok(ApiResponse::fail)：Err 会记成 result="error" 并烧可用性预算
            return Ok(ApiResponse::fail(40102, "管理 Token 校验失败"));
        }
        next.run(ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bearer_token_case_insensitively() {
        assert_eq!(extract_token("Bearer abc123").as_deref(), Some("abc123"));
        assert_eq!(extract_token("bearer abc123").as_deref(), Some("abc123"));
        assert_eq!(extract_token("BEARER abc123").as_deref(), Some("abc123"));
        assert_eq!(
            extract_token("  Bearer   abc123  ").as_deref(),
            Some("abc123"),
            "首尾空白应被容忍"
        );
    }

    #[test]
    fn rejects_non_bearer_schemes() {
        // 不做降级猜测：Basic / 裸 token 一律拒绝
        assert_eq!(extract_token("Basic abc"), None);
        assert_eq!(extract_token("abc123"), None);
        assert_eq!(extract_token(""), None);
        assert_eq!(extract_token("Bearer"), None);
        assert_eq!(extract_token("Bearer   "), None);
    }

    #[test]
    fn token_with_inner_spaces_is_preserved() {
        // Token 本身可能含空格（自定义取值，格式不限），只 trim 首尾
        assert_eq!(extract_token("Bearer a b c").as_deref(), Some("a b c"));
    }

    #[test]
    fn middleware_hashes_the_token_immediately() {
        // 构造后不应保留明文：token_hash 必须是 64 位 hex
        let target = ActionRef::new(
            yang_base::module!("feishu.option"),
            yang_base::action_name!("upsert_options"),
        );
        let middleware =
            ManagementTokenMiddleware::new("a-real-management-token-value-1234", target);
        assert_eq!(middleware.token_hash.len(), 64);
        assert!(middleware
            .token_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit()));
        assert!(
            !middleware.token_hash.contains("a-real"),
            "摘要里不得残留明文片段"
        );
    }

    #[test]
    fn target_action_is_pinned_to_the_declared_ref() {
        // 中间件必须精确限定到一个 Action，避免误伤同 module 的公开取选项端点
        let target = ActionRef::new(
            yang_base::module!("feishu.option"),
            yang_base::action_name!("upsert_options"),
        );
        let middleware =
            ManagementTokenMiddleware::new("a-real-management-token-value-1234", target);
        let pinned = middleware
            .target_action()
            .unwrap_or_else(|| panic!("必须限定目标 Action"));
        assert_eq!(pinned.module().as_str(), "feishu.option");
        assert_eq!(pinned.action().as_str(), "upsert_options");
    }
}

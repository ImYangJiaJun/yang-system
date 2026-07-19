//! pre-tenant 企业用例服务。

use super::repository::TenantRepository;
use schemars::JsonSchema;
use serde::Serialize;
use yang_base::action::ActionContext;
use yang_base::BaseError;

pub(super) const DEFAULT_PAGE: usize = 1;
pub(super) const DEFAULT_LIMIT: usize = 20;
pub(super) const MAX_LIMIT: usize = 100;

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct TenantSummary {
    pub(super) id: i64,
    pub(super) name: String,
    pub(super) code: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct TenantPage {
    pub(super) items: Vec<TenantSummary>,
    pub(super) total: usize,
    pub(super) page: usize,
    pub(super) limit: usize,
}

pub(super) struct TenantService {
    repository: TenantRepository,
}

impl TenantService {
    pub(super) fn new(repository: TenantRepository) -> Self {
        Self { repository }
    }

    pub(super) async fn list(
        &self,
        ctx: &ActionContext,
        page: Option<i64>,
        limit: Option<i64>,
    ) -> Result<TenantPage, BaseError> {
        let user = ctx
            .authenticated_user()
            .ok_or_else(|| BaseError::Unauthorized("租户发现需要已认证用户".to_string()))?;
        let page = positive_usize("page", page, DEFAULT_PAGE, usize::MAX)?;
        let limit = positive_usize("limit", limit, DEFAULT_LIMIT, MAX_LIMIT)?;
        self.repository
            .list_for_user(ctx, user.id, page, limit)
            .await
    }
}

fn positive_usize(
    name: &str,
    value: Option<i64>,
    default: usize,
    maximum: usize,
) -> Result<usize, BaseError> {
    let value = match value {
        None => default,
        Some(value) if value > 0 => usize::try_from(value).map_err(|_| {
            BaseError::ParamInvalid(name.to_string(), "参数超出有效范围".to_string())
        })?,
        Some(_) => {
            return Err(BaseError::ParamInvalid(
                name.to_string(),
                "参数必须大于 0".to_string(),
            ))
        }
    };
    if value > maximum {
        return Err(BaseError::ParamInvalid(
            name.to_string(),
            format!("参数不能超过 {maximum}"),
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pagination_is_positive_and_bounded() {
        assert_eq!(
            positive_usize("limit", None, DEFAULT_LIMIT, MAX_LIMIT)
                .unwrap_or_else(|error| panic!("默认分页大小应有效: {error}")),
            DEFAULT_LIMIT
        );
        assert!(matches!(
            positive_usize("limit", Some(0), DEFAULT_LIMIT, MAX_LIMIT),
            Err(BaseError::ParamInvalid(name, _)) if name == "limit"
        ));
        assert!(matches!(
            positive_usize("limit", Some(101), DEFAULT_LIMIT, MAX_LIMIT),
            Err(BaseError::ParamInvalid(name, _)) if name == "limit"
        ));
    }
}

//! 平台账号读取模型与分页契约。

use schemars::JsonSchema;
use serde::Serialize;
use yang_base::BaseError;

const DEFAULT_PAGE: i64 = 1;
const DEFAULT_LIMIT: i64 = 20;
const MAX_LIMIT: i64 = 100;

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct AdminAccountView {
    pub(super) id: i64,
    pub(super) user_user: i64,
    pub(super) username: String,
    pub(super) name: String,
    pub(super) position: Option<String>,
    pub(super) status: String,
    pub(super) admin: bool,
    pub(super) created_at: i64,
    pub(super) updated_at: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct AdminAccountPage {
    items: Vec<AdminAccountView>,
    total: usize,
    page: i64,
    limit: i64,
}

impl AdminAccountPage {
    pub(super) fn new(items: Vec<AdminAccountView>, total: usize, request: PageRequest) -> Self {
        Self {
            items,
            total,
            page: request.page,
            limit: request.limit,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PageRequest {
    pub(super) page: i64,
    pub(super) limit: i64,
    pub(super) offset: u64,
}

impl PageRequest {
    pub(super) fn parse(page: Option<i64>, limit: Option<i64>) -> Result<Self, BaseError> {
        let page = page.unwrap_or(DEFAULT_PAGE);
        let limit = limit.unwrap_or(DEFAULT_LIMIT);
        if page < 1 || !(1..=MAX_LIMIT).contains(&limit) {
            return Err(BaseError::ParamInvalid(
                "page/limit".to_string(),
                format!("page 必须大于等于 1，limit 必须在 1..={MAX_LIMIT} 之间"),
            ));
        }
        let offset = page
            .checked_sub(1)
            .and_then(|value| value.checked_mul(limit))
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| {
                BaseError::ParamInvalid("page".to_string(), "分页偏移量超出范围".to_string())
            })?;
        Ok(Self {
            page,
            limit,
            offset,
        })
    }

    pub(super) fn sql_limit(self) -> Result<u64, BaseError> {
        u64::try_from(self.limit)
            .map_err(|_| BaseError::ParamInvalid("limit".to_string(), "参数超出范围".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pagination_is_bounded_and_overflow_safe() {
        let request = PageRequest::parse(None, None)
            .unwrap_or_else(|error| panic!("默认分页应有效: {error}"));
        assert_eq!((request.page, request.limit, request.offset), (1, 20, 0));
        assert!(PageRequest::parse(Some(0), Some(20)).is_err());
        assert!(PageRequest::parse(Some(1), Some(101)).is_err());
        assert!(PageRequest::parse(Some(i64::MAX), Some(100)).is_err());
    }
}

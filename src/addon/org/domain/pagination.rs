//! 企业领域共用的强类型分页值对象。

use schemars::JsonSchema;
use serde::Serialize;
use yang_base::BaseError;

const DEFAULT_PAGE: usize = 1;
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;

#[derive(Debug, Clone, Copy)]
pub(in crate::addon::org) struct PageRequest {
    pub(in crate::addon::org) page: usize,
    pub(in crate::addon::org) limit: usize,
}

impl PageRequest {
    pub(in crate::addon::org) fn parse(
        page: Option<i64>,
        limit: Option<i64>,
    ) -> Result<Self, BaseError> {
        Ok(Self {
            page: positive_usize("page", page, DEFAULT_PAGE, usize::MAX)?,
            limit: positive_usize("limit", limit, DEFAULT_LIMIT, MAX_LIMIT)?,
        })
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(in crate::addon::org) struct Page<T> {
    pub(super) items: Vec<T>,
    pub(super) total: usize,
    pub(super) page: usize,
    pub(super) limit: usize,
    pub(super) total_pages: usize,
}

impl<T> Page<T> {
    pub(in crate::addon::org) fn new(items: Vec<T>, total: usize, request: PageRequest) -> Self {
        Self {
            items,
            total,
            page: request.page,
            limit: request.limit,
            total_pages: total.div_ceil(request.limit),
        }
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
    fn pagination_defaults_bounds_and_calculates_total_pages() {
        let request = PageRequest::parse(None, None)
            .unwrap_or_else(|error| panic!("默认分页应有效: {error}"));
        assert_eq!(request.page, DEFAULT_PAGE);
        assert_eq!(request.limit, DEFAULT_LIMIT);
        assert!(PageRequest::parse(Some(0), None).is_err());
        assert!(PageRequest::parse(None, Some(101)).is_err());

        let page = Page::new(vec![1, 2], 21, request);
        assert_eq!(page.total_pages, 2);
    }
}

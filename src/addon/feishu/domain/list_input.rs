//! 前端控制台列表接口的通用分页输入。
//!
//! **必须与框架内置 `SelectAction` 的标准分页契约逐字段同构**
//! （`page / page_size / search / where / order_by / count_total`），否则通用
//! `TableView` 一调就失败——它会把这六个字段全发过来，而 `deny_unknown_fields`
//! 会把未声明的键判为 400。
//!
//! 这不是理论风险：最初本结构只声明了 `page / page_size / search`，结果前端点开页面
//! 就报错。参照实现见 `crate::addon::demo::notes::actions::list_notes`（它验证过与
//! 通用 TableView 的兼容性）。

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::builtin::OrderByItem;
use yang_base::definition::{ParamInput, Params};
use yang_base::table::WhereCondition;

fn default_page() -> u32 {
    1
}

fn default_page_size() -> u32 {
    10
}

/// 列表查询输入；与通用 TableView 发送的标准 select 输入完全同构。
///
/// 不能 derive `Debug`：`OrderByItem`（框架内置分页输入类型）没有实现它——
/// 这是 `ADDON_ONBOARDING.md` 记录的既有摩擦点 ④，去掉 derive 即可。
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListInput {
    /// 页码（1 起步），缺省 1。
    #[serde(default = "default_page")]
    pub(crate) page: u32,
    /// 每页条数，缺省 10，必须 1..=100。
    #[serde(default = "default_page_size")]
    pub(crate) page_size: u32,
    /// 在表定义声明的 `searchable` 字段中执行关键词搜索。
    #[serde(default)]
    pub(crate) search: Option<String>,
    /// where 布尔过滤树（JSON key 为 `"where"`），缺省无条件。
    #[serde(rename = "where", default)]
    pub(crate) where_clause: Option<WhereCondition>,
    /// 排序规则列表。
    #[serde(default)]
    pub(crate) order_by: Vec<OrderByItem>,
    /// 是否额外执行 COUNT 查询。
    #[serde(default)]
    pub(crate) count_total: bool,
    /// 按数据源过滤；本服务自己的扩展字段，前端不发即为 `None`。
    #[serde(default)]
    pub(crate) source_key: Option<String>,
}

impl ListInput {
    /// 校验分页参数；越界返回 `ParamInvalid`（与内置 `SelectAction` 一致）。
    pub(crate) fn validate(&self) -> Result<(), yang_base::BaseError> {
        if self.page == 0 || self.page_size == 0 || self.page_size > 100 {
            return Err(yang_base::BaseError::ParamInvalid(
                "page/page_size".into(),
                "page>=1, 1<=page_size<=100".into(),
            ));
        }
        Ok(())
    }
}

impl ParamInput for ListInput {
    fn params() -> Params {
        Params::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_matches_the_standard_table_view_select_contract() {
        // 这条测试是防线：它会拒绝任何把字段删掉/改名的改动。
        // 通用 TableView 发出的就是这六个键，少一个就 400。
        let input = serde_json::from_value::<ListInput>(serde_json::json!({
            "page": 2,
            "page_size": 20,
            "search": "北京",
            "where": {"type": "eq", "field": "source_key", "value": "demo"},
            "order_by": [{"field": "sort_order", "direction": "Desc"}],
            "count_total": true
        }))
        .unwrap_or_else(|error| panic!("标准 select 输入必须可解析: {error}"));

        assert_eq!(input.page, 2);
        assert_eq!(input.page_size, 20);
        assert_eq!(input.search.as_deref(), Some("北京"));
        assert!(input.where_clause.is_some());
        assert_eq!(input.order_by.len(), 1);
        assert_eq!(
            input.order_by[0].direction,
            yang_base::table::SortOrder::Desc,
            "方向是 PascalCase 的 Desc，不是 desc"
        );
        assert!(input.count_total);
        assert!(input.source_key.is_none(), "前端不发本服务的扩展字段");
    }

    #[test]
    fn accepts_empty_object_with_defaults() {
        let input = serde_json::from_value::<ListInput>(serde_json::json!({}))
            .unwrap_or_else(|error| panic!("空对象应可解析: {error}"));
        assert_eq!(input.page, 1);
        assert_eq!(input.page_size, 10);
        assert!(input.order_by.is_empty());
        assert!(!input.count_total);
    }

    #[test]
    fn accepts_the_table_view_first_load_request() {
        // 首次加载只发 count_total，其余全走缺省——这条与 demo 的同类断言对齐
        let input = serde_json::from_value::<ListInput>(serde_json::json!({"count_total": true}))
            .unwrap_or_else(|error| panic!("最小请求应可解析: {error}"));
        assert_eq!(input.page, 1);
        assert_eq!(input.page_size, 10);
        assert!(input.search.is_none());
        assert!(input.where_clause.is_none());
        assert!(input.order_by.is_empty());
    }

    #[test]
    fn rejects_out_of_range_paging() {
        for (page, page_size) in [(0u32, 10u32), (1, 0), (1, 101)] {
            let input = ListInput {
                page,
                page_size,
                search: None,
                where_clause: None,
                order_by: Vec::new(),
                count_total: false,
                source_key: None,
            };
            assert!(
                input.validate().is_err(),
                "page={page} page_size={page_size} 应被拒绝"
            );
        }
    }

    #[test]
    fn accepts_boundary_paging() {
        for (page, page_size) in [(1u32, 1u32), (1, 100), (u32::MAX, 100)] {
            let input = ListInput {
                page,
                page_size,
                search: None,
                where_clause: None,
                order_by: Vec::new(),
                count_total: false,
                source_key: None,
            };
            assert!(input.validate().is_ok(), "边界值应被接受");
        }
    }
}

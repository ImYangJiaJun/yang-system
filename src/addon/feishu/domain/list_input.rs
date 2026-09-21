//! 前端控制台列表接口的通用分页输入。
//!
//! 与飞书那条入口的游标分页不同：控制台是人在用，`page/page_size` 语义更直观，
//! 也是框架 `SelectAction` 的标准契约（`TableView` 直接消费 `items/page/page_size/total`）。

/// 前端控制台列表接口的通用分页输入。
///
/// 与飞书那条入口的游标分页不同：控制台是人在用，`page/page_size` 语义更直观，
/// 也是框架 `SelectAction` 的标准契约（`TableView` 直接消费 `items/page/page_size/total`）。
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListInput {
    /// 页码，从 1 开始。
    #[serde(default)]
    pub(crate) page: Option<i64>,
    /// 每页条数；上限由框架的 `MAX_QUERY_PAGE_SIZE`（100）决定。
    #[serde(default)]
    pub(crate) page_size: Option<i64>,
    /// 关键词；命中表上声明了 `searchable` 的文本字段。
    #[serde(default)]
    pub(crate) search: Option<String>,
    /// 按数据源过滤；`None` 表示不过滤（控制台可看全部）。
    #[serde(default)]
    pub(crate) source_key: Option<String>,
}

impl ListInput {
    /// 归一化分页参数：页码至少 1，每页条数夹在 1..=100，缺省 20。
    ///
    /// 这里显式夹取而不是让 `TableQuery::page` 报错：控制台的页码来自前端分页组件，
    /// 越界是操作而不是攻击，回退到合法值比返回 400 体验更好。
    pub(crate) fn normalized(&self) -> (usize, usize) {
        const DEFAULT_PAGE_SIZE: usize = 20;
        const MAX_PAGE_SIZE: usize = 100;
        let page = self.page.unwrap_or(1).max(1) as usize;
        let page_size = match self.page_size {
            Some(value) if value > 0 => (value as usize).min(MAX_PAGE_SIZE),
            _ => DEFAULT_PAGE_SIZE,
        };
        (page, page_size)
    }
}

impl yang_base::definition::ParamInput for ListInput {
    fn params() -> yang_base::definition::Params {
        yang_base::definition::Params::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    fn input(page: Option<i64>, page_size: Option<i64>) -> ListInput {
        ListInput {
            page,
            page_size,
            search: None,
            source_key: None,
        }
    }

    #[test]
    fn clamps_out_of_range_paging() {
        assert_eq!(
            input(Some(0), Some(0)).normalized(),
            (1, 20),
            "非法值应回退到默认"
        );
        assert_eq!(
            input(Some(3), Some(1000)).normalized(),
            (3, 100),
            "每页条数必须夹在框架上限内"
        );
        assert_eq!(input(None, None).normalized(), (1, 20));
    }

    #[test]
    fn rejects_unknown_fields() {
        // 控制台输入用 deny_unknown_fields：这里是内部契约，拼错的键应当报错而不是被忽略
        let mut request = yang_base::action::Request::new(serde_json::json!({"typo": 1}));
        assert!(ListInput::decode(&mut request).is_err());
    }

    #[test]
    fn accepts_empty_object() {
        // 全部字段可选：不传任何参数就是第一页
        let mut request = yang_base::action::Request::new(serde_json::json!({}));
        assert!(ListInput::decode(&mut request).is_ok());
    }
}

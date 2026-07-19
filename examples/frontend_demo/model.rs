//! 验收服务共享的内存模型。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use yang_base::definition::{ParamInput, Params};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct NoInput {}

impl ParamInput for NoInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(super) struct DemoItem {
    pub(super) id: i64,
    pub(super) name: String,
    pub(super) category_id: i64,
    pub(super) status: String,
    pub(super) parent_id: Option<i64>,
}

pub(super) type DemoItems = Arc<tokio::sync::RwLock<Vec<DemoItem>>>;

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct MutationOutput {
    pub(super) id: i64,
}

pub(super) fn fixture_items() -> DemoItems {
    Arc::new(tokio::sync::RwLock::new(vec![
        DemoItem {
            id: 1,
            name: "平台能力".to_string(),
            category_id: 1,
            status: "active".to_string(),
            parent_id: None,
        },
        DemoItem {
            id: 2,
            name: "通用渲染器".to_string(),
            category_id: 2,
            status: "draft".to_string(),
            parent_id: Some(1),
        },
    ]))
}

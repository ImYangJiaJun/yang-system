//! 外部选项 / 部门名的真实解析器。
//!
//! 选项台账是 `feishu_option`：`option_id` 全局唯一，`source_key` + `label` 可查询。
//! 外部选项回读可能是 ID（提交时的 `option_id`）或文案；两种都要能落到文案，
//! 因为多维表单选/多选写的是选项名。

use async_trait::async_trait;
use serde_json::json;

use super::super::approval::resolve_department_name;
use super::super::context::FeishuContext;
use super::super::outbound::{OutboundTransport, Sleeper};
use super::super::tenant_token::TenantTokenProvider;
use super::error::SyncError;
use super::plan::{DepartmentResolver, OptionResolver};

/// 基于 `feishu_option` 的外部选项解析器。
pub(crate) struct DbOptionResolver<'a> {
    pub(crate) context: &'a FeishuContext,
}

#[async_trait]
impl OptionResolver for DbOptionResolver<'_> {
    async fn label_for_id(
        &self,
        source_key: &str,
        option_id: &str,
    ) -> Result<Option<String>, SyncError> {
        let source_key = source_key.trim();
        let option_id = option_id.trim();
        if source_key.is_empty() || option_id.is_empty() {
            return Ok(None);
        }
        let row = self
            .context
            .options()
            .query()
            .where_eq("source_key", json!(source_key))
            .map_err(|_| SyncError::OptionUnresolved {
                widget_id: String::new(),
            })?
            .where_eq("option_id", json!(option_id))
            .map_err(|_| SyncError::OptionUnresolved {
                widget_id: String::new(),
            })?
            .optional()
            .await
            .map_err(|_| SyncError::OptionUnresolved {
                widget_id: String::new(),
            })?;
        Ok(row
            .and_then(|row| row.optional::<String>("label").ok().flatten())
            .map(|label| label.trim().to_string())
            .filter(|label| !label.is_empty()))
    }

    async fn id_for_label(
        &self,
        source_key: &str,
        label: &str,
    ) -> Result<Option<String>, SyncError> {
        let source_key = source_key.trim();
        let label = label.trim();
        if source_key.is_empty() || label.is_empty() {
            return Ok(None);
        }
        let rows = self
            .context
            .options()
            .query()
            .where_eq("source_key", json!(source_key))
            .map_err(|_| SyncError::OptionUnresolved {
                widget_id: String::new(),
            })?
            .where_eq("label", json!(label))
            .map_err(|_| SyncError::OptionUnresolved {
                widget_id: String::new(),
            })?
            .order_by("sort_order", yang_base::table::SortOrder::Asc)
            .map_err(|_| SyncError::OptionUnresolved {
                widget_id: String::new(),
            })?
            .all()
            .await
            .map_err(|_| SyncError::OptionUnresolved {
                widget_id: String::new(),
            })?;
        Ok(rows
            .into_iter()
            .find_map(|row| row.optional::<String>("option_id").ok().flatten())
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty()))
    }
}

/// 部门 open_department_id → 名称；失败返回 `None`（调用方写 ID 兜底）。
pub(crate) struct FeishuDepartmentResolver<'a> {
    pub(crate) transport: &'a dyn OutboundTransport,
    pub(crate) sleeper: &'a dyn Sleeper,
    pub(crate) tokens: &'a TenantTokenProvider,
}

#[async_trait]
impl DepartmentResolver for FeishuDepartmentResolver<'_> {
    async fn name_for_id(&self, department_id: &str) -> Result<Option<String>, SyncError> {
        let department_id = department_id.trim();
        if department_id.is_empty() {
            return Ok(None);
        }
        Ok(
            resolve_department_name(self.transport, self.sleeper, self.tokens, department_id)
                .await
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty()),
        )
    }
}

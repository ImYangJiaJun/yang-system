//! 更新查询门户配置。

use std::collections::HashMap;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::portal::actions::create_portal::{validate_custom_slug, ACCESS_MODES};
use crate::addon::feishu::portal::domain::bindings::{load_bound_fields, require_all_bound};
use crate::infrastructure::audit;

const TITLE_MAX: usize = 100;
const EMPTY_MESSAGE_MAX: usize = 500;
const RATE_LIMIT_MAX: u32 = 120;

/// 更新门户的输入契约；每个字段都是 `Option`，**省略即保持原值**。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdatePortalInput {
    /// 目标门户 `id`。
    pub(super) portal_id: i64,
    /// 门户名称。
    #[serde(default)]
    pub(super) title: Option<String>,
    /// 查询键 `field_id`；改它会同时刷新冗余的 `query_field_source_key`。
    #[serde(default)]
    pub(super) query_field_id: Option<String>,
    /// 开放列（`field_id` 列表）。
    #[serde(default)]
    pub(super) exposed_columns: Option<Vec<String>>,
    /// 访问模式。
    #[serde(default)]
    pub(super) access_mode: Option<String>,
    /// 自定义短链；显式给空串表示**清空**（回落到 UUID slug）。
    #[serde(default)]
    pub(super) custom_slug: Option<String>,
    /// 展示名映射。
    #[serde(default)]
    pub(super) result_labels: Option<HashMap<String, String>>,
    /// 查无结果时的提示文案。
    #[serde(default)]
    pub(super) empty_message: Option<String>,
    /// 每分钟最大查询次数。
    #[serde(default)]
    pub(super) rate_limit_per_min: Option<u32>,
    /// 是否启用模糊匹配。
    #[serde(default)]
    pub(super) fuzzy_enabled: Option<bool>,
    /// 触发模糊匹配所需的最少连续数字位数。
    #[serde(default)]
    pub(super) fuzzy_min_digits: Option<u32>,
}

impl ParamInput for UpdatePortalInput {
    fn params() -> Params {
        Params::new()
    }
}

impl UpdatePortalInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.portal_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "portal_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        if let Some(title) = self.title.as_deref() {
            if title.trim().is_empty()
                || title.chars().count() > TITLE_MAX
                || title.chars().any(char::is_control)
            {
                return Err(BaseError::ParamInvalid(
                    "title".to_string(),
                    format!("名称必须在 1..={TITLE_MAX} 字符"),
                ));
            }
        }
        if let Some(columns) = &self.exposed_columns {
            if columns.is_empty() {
                return Err(BaseError::ParamInvalid(
                    "exposed_columns".to_string(),
                    "至少要开放一列".to_string(),
                ));
            }
        }
        if let Some(mode) = self.access_mode.as_deref() {
            if !ACCESS_MODES.contains(&mode) {
                return Err(BaseError::ParamInvalid(
                    "access_mode".to_string(),
                    format!("访问模式只能是 {}", ACCESS_MODES.join("、")),
                ));
            }
        }
        if let Some(slug) = self.custom_slug.as_deref() {
            // 空串是「清空」的显式表达，不是非法形态。
            if !slug.is_empty() {
                validate_custom_slug(slug)?;
            }
        }
        if let Some(message) = self.empty_message.as_deref() {
            if message.chars().count() > EMPTY_MESSAGE_MAX {
                return Err(BaseError::ParamInvalid(
                    "empty_message".to_string(),
                    format!("提示文案不得超过 {EMPTY_MESSAGE_MAX} 字符"),
                ));
            }
        }
        if let Some(rate) = self.rate_limit_per_min {
            if rate == 0 || rate > RATE_LIMIT_MAX {
                return Err(BaseError::ParamInvalid(
                    "rate_limit_per_min".to_string(),
                    format!("限速必须在 1..={RATE_LIMIT_MAX} 次/分钟"),
                ));
            }
        }
        if let Some(digits) = self.fuzzy_min_digits {
            if !(2..=16).contains(&digits) {
                return Err(BaseError::ParamInvalid(
                    "fuzzy_min_digits".to_string(),
                    "模糊匹配最低位数必须在 2..=16".to_string(),
                ));
            }
        }
        Ok(())
    }

    /// 本次请求点名要校验的 `field_id`（查询键 + 开放列）。
    fn referenced_field_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        if let Some(field_id) = &self.query_field_id {
            ids.push(field_id.clone());
        }
        if let Some(columns) = &self.exposed_columns {
            ids.extend(columns.iter().cloned());
        }
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// 是否一个可改字段都没给。
    fn changes_nothing(&self) -> bool {
        self.title.is_none()
            && self.query_field_id.is_none()
            && self.exposed_columns.is_none()
            && self.access_mode.is_none()
            && self.custom_slug.is_none()
            && self.result_labels.is_none()
            && self.empty_message.is_none()
            && self.rate_limit_per_min.is_none()
            && self.fuzzy_enabled.is_none()
            && self.fuzzy_min_digits.is_none()
    }
}

/// `custom_slug` 的唯一键冲突折算成参数错误（与创建路径同一口径）。
fn fold_duplicate_custom_slug(error: BaseError) -> BaseError {
    if let BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(message)) = &error {
        if message.contains("Duplicate entry")
            && message.contains("uk_feishu_datasource_portal_custom_slug")
        {
            return BaseError::ParamInvalid(
                "custom_slug".to_string(),
                "该短链已被占用，请换一个".to_string(),
            );
        }
    }
    error
}

/// 注册更新门户端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("update_portal"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Put, "/api/v1/feishu/portals")
        .display_name("更新查询门户")
        .description("更新门户的开放列、访问模式、限速等配置；省略的字段保持原值")
        .permissions(["feishu.portal.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: UpdatePortalInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;
    if input.changes_nothing() {
        return Err(BaseError::ParamInvalid(
            "portal_id".to_string(),
            "没有要更新的字段".to_string(),
        ));
    }

    let current = context
        .portals()
        .query()
        .select_fields(&[
            "id",
            "datasource_id",
            "query_field_id",
            "exposed_columns",
            "passphrase_hash",
        ])?
        .where_eq("id", serde_json::json!(input.portal_id))?
        .optional()
        .await?
        .ok_or_else(|| BaseError::RecordNotFound("门户不存在".to_string()))?;
    let datasource_id: i64 = current.require("datasource_id")?;
    let current_query_field: String = current.require("query_field_id")?;
    let current_exposed: Vec<String> =
        serde_json::from_str(&current.require::<String>("exposed_columns")?).unwrap_or_default();
    let has_passphrase: bool = current.optional::<String>("passphrase_hash")?.is_some();

    // 生效后的形状：查询键与开放列可能只有一个被改，但「查询键不得出现在开放列中」
    // 这条约束要看**合并后**的值，只看本次给的那个会漏。
    let effective_query_field = input
        .query_field_id
        .clone()
        .unwrap_or_else(|| current_query_field.clone());
    let effective_exposed = input
        .exposed_columns
        .clone()
        .unwrap_or_else(|| current_exposed.clone());
    if effective_exposed
        .iter()
        .any(|c| c == &effective_query_field)
    {
        return Err(BaseError::ParamInvalid(
            "exposed_columns".to_string(),
            "查询键不能出现在开放列中".to_string(),
        ));
    }

    let bindings =
        load_bound_fields(&context, datasource_id, &input.referenced_field_ids()).await?;
    if let Some(field_id) = &input.query_field_id {
        require_all_bound(&bindings, std::slice::from_ref(field_id), "query_field_id")?;
    }
    if let Some(columns) = &input.exposed_columns {
        require_all_bound(&bindings, columns, "exposed_columns")?;
    }

    // 切到口令模式却没有存量口令 = 切出一个谁也进不去的门户。口令本身走 set_passphrase。
    if input.access_mode.as_deref() == Some("passphrase") && !has_passphrase {
        return Err(BaseError::ParamInvalid(
            "access_mode".to_string(),
            "尚未设置口令，请先调用设置口令接口".to_string(),
        ));
    }

    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<(), BaseError> = async {
        let mut record = Record::new();
        if let Some(title) = input.title.as_deref() {
            record.insert("title", serde_json::json!(title.trim()));
        }
        if let Some(field_id) = input.query_field_id.as_deref() {
            let source_key = bindings.get(field_id).cloned().ok_or_else(|| {
                BaseError::ParamInvalid(
                    "query_field_id".to_string(),
                    "查询键不是该数据源的已绑定字段".to_string(),
                )
            })?;
            record.insert("query_field_id", serde_json::json!(field_id.trim()));
            record.insert("query_field_source_key", serde_json::json!(source_key));
        }
        if let Some(columns) = &input.exposed_columns {
            record.insert(
                "exposed_columns",
                serde_json::json!(serde_json::to_string(columns)
                    .map_err(|error| BaseError::JsonSerializeFailed(error.to_string()))?),
            );
        }
        if let Some(mode) = input.access_mode.as_deref() {
            record.insert("access_mode", serde_json::json!(mode));
        }
        if let Some(slug) = input.custom_slug.as_deref() {
            // 空串表示清空：写成 NULL 而不是空串，否则唯一索引会把「多个门户都没短链」
            // 判成重复（MySQL 的唯一索引不约束 NULL，却约束空串）。
            if slug.is_empty() {
                record.insert("custom_slug", serde_json::Value::Null);
            } else {
                record.insert("custom_slug", serde_json::json!(slug));
            }
        }
        if let Some(labels) = &input.result_labels {
            record.insert(
                "result_labels",
                serde_json::json!(serde_json::to_string(labels)
                    .map_err(|error| BaseError::JsonSerializeFailed(error.to_string()))?),
            );
        }
        if let Some(message) = input.empty_message.as_deref() {
            record.insert("empty_message", serde_json::json!(message));
        }
        if let Some(rate) = input.rate_limit_per_min {
            record.insert("rate_limit_per_min", serde_json::json!(i64::from(rate)));
        }
        if let Some(enabled) = input.fuzzy_enabled {
            record.insert("fuzzy_enabled", serde_json::json!(enabled));
        }
        if let Some(digits) = input.fuzzy_min_digits {
            record.insert("fuzzy_min_digits", serde_json::json!(i64::from(digits)));
        }

        let affected = context
            .portals()
            .query()
            .where_eq("id", serde_json::json!(input.portal_id))?
            .update_in_tx(&mut transaction, record)
            .await
            .map_err(fold_duplicate_custom_slug)?;
        if affected == 0 {
            return Err(BaseError::RecordNotFound("门户不存在".to_string()));
        }

        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_datasource_portal", input.portal_id)?,
            None,
            Some(audit::summary([(
                "outcome_code",
                serde_json::json!("updated_portal"),
            )])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;

    FeishuContext::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        serde_json::json!({ "portal_id": input.portal_id }),
        "更新成功",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> UpdatePortalInput {
        UpdatePortalInput {
            portal_id: 3,
            title: Some("卡号查询".to_string()),
            query_field_id: None,
            exposed_columns: None,
            access_mode: None,
            custom_slug: None,
            result_labels: None,
            empty_message: None,
            rate_limit_per_min: None,
            fuzzy_enabled: None,
            fuzzy_min_digits: None,
        }
    }

    #[test]
    fn accepts_a_partial_update() {
        assert!(input().validate().is_ok());
    }

    #[test]
    fn rejects_a_non_positive_portal_id() {
        let mut input = input();
        input.portal_id = 0;
        assert!(input.validate().is_err());
    }

    #[test]
    fn rejects_a_request_that_changes_nothing() {
        // 全 Option 的输入天然允许空对象；空更新会在 `update_in_tx` 里因为
        // 「赋值列为空」失败，报出的却是内部错误。在这里挡掉。
        let mut input = input();
        input.title = None;
        assert!(input.changes_nothing());
        assert!(
            input.validate().is_ok(),
            "空更新是合法形状，但 handler 会拒"
        );
    }

    #[test]
    fn rejects_an_empty_exposed_column_set() {
        let mut input = input();
        input.exposed_columns = Some(Vec::new());
        assert!(input.validate().is_err());
    }

    #[test]
    fn accepts_an_empty_custom_slug_as_an_explicit_clear() {
        let mut input = input();
        input.custom_slug = Some(String::new());
        assert!(input.validate().is_ok(), "空串表示清空短链");

        input.custom_slug = Some("Bad Slug".to_string());
        assert!(input.validate().is_err());
    }

    #[test]
    fn rejects_an_out_of_range_rate_limit() {
        for rate in [0u32, 121] {
            let mut input = input();
            input.rate_limit_per_min = Some(rate);
            assert!(input.validate().is_err());
        }
    }

    #[test]
    fn rejects_an_out_of_range_fuzzy_min_digits() {
        for digits in [0u32, 1, 17] {
            let mut input = input();
            input.fuzzy_min_digits = Some(digits);
            assert!(input.validate().is_err(), "位数 {digits} 应被拒");
        }
        let mut input = input();
        input.fuzzy_min_digits = Some(6);
        assert!(input.validate().is_ok());
    }

    #[test]
    fn referenced_field_ids_cover_every_field_the_request_names() {
        let mut input = input();
        input.query_field_id = Some("fldCard".to_string());
        input.exposed_columns = Some(vec!["fldShop".to_string()]);
        assert_eq!(
            input.referenced_field_ids(),
            vec!["fldCard".to_string(), "fldShop".to_string()]
        );
    }
}

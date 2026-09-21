//! 多维表格写入 API：按 id 批量停用选项。
//!
//! **语义是停用（`enabled = false`）而不是物理删除。** 选项 id 会被历史审批单引用，
//! 删掉行会让那些单据的字段值彻底失联、且无法追溯；停用则既让新发起的审批看不到它，
//! 又保留了可审计的事实。真正的物理清理属于运维动作，不开放给自动化工作流。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::infrastructure::audit;

/// 单次请求允许停用的最大条数。
const MAX_BATCH: usize = 500;

/// 停用接口的输入契约。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteOptionsInput {
    /// 目标数据源。
    pub(super) source_key: String,
    /// 要停用的选项 id。
    pub(super) ids: Vec<String>,
}

impl ParamInput for DeleteOptionsInput {
    fn params() -> Params {
        Params::new()
    }
}

impl DeleteOptionsInput {
    /// 进入事务前的入参校验。
    fn validate(&self) -> Result<(), BaseError> {
        let invalid =
            |message: &str| BaseError::ParamInvalid("ids".to_string(), message.to_string());
        if self.source_key.trim().is_empty() {
            return Err(BaseError::ParamInvalid(
                "source_key".to_string(),
                "数据源标识不能为空".to_string(),
            ));
        }
        if self.ids.is_empty() {
            return Err(invalid("选项 id 列表不能为空"));
        }
        if self.ids.len() > MAX_BATCH {
            return Err(invalid(&format!(
                "单次最多停用 {MAX_BATCH} 条，收到 {}",
                self.ids.len()
            )));
        }
        if self.ids.iter().any(|id| id.trim().is_empty()) {
            return Err(invalid("选项 id 不能为空"));
        }
        Ok(())
    }
}

/// 停用结果。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct DeleteOptionsResult {
    /// 实际被停用的条数。
    disabled: u64,
    /// 数据源标识。
    source_key: String,
}

/// 注册批量停用端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("delete_options"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/inbound/options/delete")
        .display_name("停用飞书选项")
        .description("多维表格工作流批量停用选项")
        .public()
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: DeleteOptionsInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let mut transaction = ctx.begin_transaction().await?;
    let options = context.options();

    let result: Result<u64, BaseError> = async {
        let mut disabled = 0u64;
        {
            let mut record = yang_base::table::Record::new();
            record.insert("enabled", serde_json::json!(false));
            // 条件带 source_key：防止用一个数据源的凭证停用另一个数据源的选项
            let affected = options
                .query()
                .where_eq("source_key", serde_json::json!(input.source_key))?
                .where_in(
                    "option_id",
                    input
                        .ids
                        .iter()
                        .map(|id| serde_json::json!(id))
                        .collect::<Vec<_>>(),
                )?
                .update_in_tx(&mut transaction, record)
                .await?;
            disabled += affected;
        }

        let event = audit::succeeded_system_event(
            &ctx,
            "feishu-inbound",
            None,
            Some(audit::entity("feishu_datasource", &input.source_key)?),
            audit::entity("feishu_option", &input.source_key)?,
            Some(audit::summary([(
                "outcome_code",
                serde_json::json!("enabled_false"),
            )])?),
            Some(audit::summary([(
                "option_count",
                serde_json::json!(disabled as i64),
            )])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(disabled)
    }
    .await;

    let disabled = FeishuContext::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        DeleteOptionsResult {
            disabled,
            source_key: input.source_key,
        },
        "停用成功",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(count: usize) -> DeleteOptionsInput {
        DeleteOptionsInput {
            source_key: "demo".to_string(),
            ids: (0..count).map(|index| format!("opt_{index}")).collect(),
        }
    }

    #[test]
    fn rejects_empty_id_list() {
        assert!(input(0).validate().is_err(), "空列表应被拒绝");
    }

    #[test]
    fn rejects_over_limit_batch() {
        assert!(input(MAX_BATCH).validate().is_ok(), "上限内应通过");
        assert!(input(MAX_BATCH + 1).validate().is_err(), "超限应被拒绝");
    }

    #[test]
    fn rejects_blank_id_inside_list() {
        let mut payload = input(2);
        payload.ids[1] = "  ".to_string();
        assert!(payload.validate().is_err(), "列表内的空白 id 应被拒绝");
    }

    #[test]
    fn rejects_blank_source_key() {
        let mut payload = input(1);
        payload.source_key = String::new();
        assert!(payload.validate().is_err(), "空白数据源标识应被拒绝");
    }

    #[test]
    fn duplicate_ids_are_allowed_here() {
        // 与 upsert 不同：重复 id 在这里只会重复命中同一行，语义无害。
        // 写这条测试是为了固定「两边规则不同是有意的」这个决定。
        let mut payload = input(2);
        payload.ids[1] = payload.ids[0].clone();
        assert!(payload.validate().is_ok(), "停用允许重复 id");
    }
}

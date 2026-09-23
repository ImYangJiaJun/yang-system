//! 删除表级数据源：连同它的字段绑定与选项一起收掉。
//!
//! # 与「取消勾选」的区别
//!
//! 取消某一列的勾选走的是**更新**，把那条绑定**停用**（见 `update_datasource_table`）。
//! 本端点删的是**整个数据源**：表级行、它的全部绑定，以及每个 `source_key` 名下的选项。
//!
//! 选项只**停用**不删除，沿用 `delete_datasource.rs` 既有的语义——历史审批单里
//! 引用的 `option_id` 要留在库里可查，删了就彻底失联。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::infrastructure::audit;

/// 删除表级数据源的输入契约。
///
/// `datasource_id` 走 **body**：与 `delete_datasource` / `update_datasource` 一致。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteTableInput {
    /// 目标数据源的 `id`。
    pub(super) datasource_id: i64,
}

impl ParamInput for DeleteTableInput {
    fn params() -> Params {
        Params::new()
    }
}

impl DeleteTableInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.datasource_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "datasource_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        Ok(())
    }
}

/// 注册删除表级数据源端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("delete_datasource_table"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Delete, "/api/v1/feishu/datasources/table")
        .display_name("删除表级数据源")
        .description("删除数据源、它的全部字段绑定；其选项行只停用不删除")
        .permissions(["feishu.datasource.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: DeleteTableInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let mut transaction = ctx.begin_transaction().await?;

    let result: Result<(u64, u64), BaseError> = async {
        // 1) 先取出这个数据源的全部 source_key——选项是按 source_key 索引的。
        let bindings = context
            .datasource_fields()
            .query()
            .select_fields(&["source_key"])?
            .where_eq("datasource_id", serde_json::json!(input.datasource_id))?
            .all()
            .await?;
        let source_keys: Vec<String> = bindings
            .iter()
            .map(|record| record.require("source_key"))
            .collect::<Result<_, BaseError>>()?;

        // 2) 停用选项（不删）。先停用再删源：中途失败时日志更贴近意图。
        let mut disabled_options = 0u64;
        for source_key in &source_keys {
            let mut record = Record::new();
            record.insert("enabled", serde_json::json!(false));
            disabled_options += context
                .options()
                .query()
                .where_eq("source_key", serde_json::json!(source_key))?
                .update_in_tx(&mut transaction, record)
                .await?;
        }

        // 3) 删绑定，再删表级行。
        context
            .datasource_fields()
            .query()
            .where_eq("datasource_id", serde_json::json!(input.datasource_id))?
            .delete_in_tx(&mut transaction)
            .await?;

        let deleted = context
            .datasources()
            .query()
            .where_eq("id", serde_json::json!(input.datasource_id))?
            .delete_in_tx(&mut transaction)
            .await?;
        if deleted == 0 {
            return Err(BaseError::RecordNotFound("数据源不存在".to_string()));
        }

        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_datasource", input.datasource_id.to_string())?,
            None,
            Some(audit::summary([
                ("outcome_code", serde_json::json!("deleted_table")),
                ("fields", serde_json::json!(source_keys.len())),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;

        Ok((source_keys.len() as u64, disabled_options))
    }
    .await;

    let (fields, disabled_options) = FeishuContext::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        serde_json::json!({
            "deleted_fields": fields,
            "disabled_options": disabled_options,
        }),
        "删除成功",
    )
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_zero_datasource_id() {
        let input = DeleteTableInput { datasource_id: 0 };
        assert!(input.validate().is_err());
    }

    #[test]
    fn accepts_a_positive_datasource_id() {
        let input = DeleteTableInput { datasource_id: 42 };
        assert!(input.validate().is_ok());
    }
}

//! 删除数据源（前端控制台）。
//!
//! **同事务停用其下全部选项。** 数据源没了而选项仍启用着，会留下一种自相矛盾的状态：
//! 外部选项接口按 `source_key` 查不到数据源、返回 40401，而选项行却还写着
//! `enabled = true`——事后无法判断是「忘了停用」还是「被人改回来了」。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::infrastructure::audit;

/// 删除数据源的输入契约。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteDatasourceInput {
    /// 目标数据源。
    pub(super) source_key: String,
}

impl ParamInput for DeleteDatasourceInput {
    fn params() -> Params {
        Params::new()
    }
}

impl DeleteDatasourceInput {
    /// 进入事务前的入参校验。
    fn validate(&self) -> Result<(), BaseError> {
        if self.source_key.trim().is_empty() {
            return Err(BaseError::ParamInvalid(
                "source_key".to_string(),
                "数据源标识不能为空".to_string(),
            ));
        }
        Ok(())
    }
}

/// 删除结果。
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub(super) struct DeleteDatasourceResult {
    /// 被删除的数据源数（0 或 1）。
    deleted: u64,
    /// 被连带停用的选项数。
    disabled_options: u64,
}

/// 注册删除数据源端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("delete_datasource"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Delete, "/api/v1/feishu/datasources")
        .display_name("删除数据源")
        .description("删除飞书数据源并停用其下全部选项")
        .permissions(["feishu.datasource.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: DeleteDatasourceInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let mut transaction = ctx.begin_transaction().await?;

    let result: Result<(u64, u64), BaseError> = async {
        // 先停用选项，再删数据源。顺序反了也不影响正确性（同一事务），但先停用能让
        // 中途失败时的日志更贴近「意图」。
        let mut record = Record::new();
        record.insert("enabled", serde_json::json!(false));
        let disabled_options = context
            .options()
            .query()
            .where_eq("source_key", serde_json::json!(input.source_key))?
            .update_in_tx(&mut transaction, record)
            .await?;

        let deleted = context
            .datasources()
            .query()
            .where_eq("source_key", serde_json::json!(input.source_key))?
            .delete_in_tx(&mut transaction)
            .await?;
        if deleted == 0 {
            return Err(BaseError::RecordNotFound("数据源不存在".to_string()));
        }

        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_datasource", &input.source_key)?,
            Some(audit::summary([(
                "outcome_code",
                serde_json::json!("deleted_with_options_disabled"),
            )])?),
            None,
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok((deleted, disabled_options))
    }
    .await;

    let (deleted, disabled_options) =
        FeishuContext::finish_transaction(transaction, result).await?;
    ApiResponse::success(
        DeleteDatasourceResult {
            deleted,
            disabled_options,
        },
        "删除成功",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_blank_source_key() {
        let payload = DeleteDatasourceInput {
            source_key: "  ".to_string(),
        };
        assert!(payload.validate().is_err());
    }

    #[test]
    fn accepts_non_blank_source_key() {
        let payload = DeleteDatasourceInput {
            source_key: "demo".to_string(),
        };
        assert!(payload.validate().is_ok());
    }
}

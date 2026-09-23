//! 列出多维表格视图——配置向导的第二步。
//!
//! **视图的职责必须讲对**：它决定**拉取哪些行**，不决定能勾哪些字段。
//! 实测「列出字段」的 `view_id` 参数不生效（带与不带返回完全相同的字段集合与顺序），
//! 所以界面上不能把它和「选字段」讲成一件事。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::bitable::{list_all_views, validate_path_segment};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound_setup;

/// 输入契约。两个坐标都进 URL 路径段。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ListBitableViewsInput {
    /// 多维表格 app_token。
    pub(super) app_token: String,
    /// 数据表 ID。
    pub(super) table_id: String,
}

impl ParamInput for ListBitableViewsInput {
    fn params() -> Params {
        Params::new()
    }
}

impl ListBitableViewsInput {
    fn validate(&self) -> Result<(), BaseError> {
        for (name, value) in [
            ("app_token", self.app_token.as_str()),
            ("table_id", self.table_id.as_str()),
        ] {
            let value = value.trim();
            if value.is_empty() {
                return Err(BaseError::ParamInvalid(
                    name.to_string(),
                    "不能为空".to_string(),
                ));
            }
            validate_path_segment("bitable_base_token", value)
                .map_err(|error| BaseError::ParamInvalid(name.to_string(), error.to_string()))?;
        }
        Ok(())
    }
}

/// 注册列出多维表格视图端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("list_bitable_views"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/datasources/bitable-views")
        .display_name("列出多维表格视图")
        .description("列出一张数据表下的视图，供配置向导选择（视图决定拉取哪些行）")
        // 与其余元数据端点同权限：只读语义，但**会出站**消耗本应用的频控配额，
        // 与 pull_probe 同类（见其 register 的注释）。
        .permissions(["feishu.datasource.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: ListBitableViewsInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let settings = match outbound_setup::require_settings(&context) {
        Ok(settings) => settings,
        Err((code, message)) => return Ok(ApiResponse::fail(code, message)),
    };
    let outbound = outbound_setup::build(&ctx, settings)?;

    let items = list_all_views(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        input.app_token.trim(),
        input.table_id.trim(),
    )
    .await
    .map_err(outbound_setup::outbound_error)?;

    let payload: Vec<serde_json::Value> = items
        .into_iter()
        .map(|item| {
            serde_json::json!({
                "view_id": item.view_id,
                "view_name": item.view_name,
                "view_type": item.view_type,
            })
        })
        .collect();

    ApiResponse::success(serde_json::json!({ "views": payload }), "查询成功")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(app_token: &str, table_id: &str) -> ListBitableViewsInput {
        ListBitableViewsInput {
            app_token: app_token.to_string(),
            table_id: table_id.to_string(),
        }
    }

    #[test]
    fn rejects_a_blank_app_token() {
        assert!(input("  ", "tblauuOafa4acvT3").validate().is_err());
    }

    #[test]
    fn rejects_a_blank_table_id() {
        assert!(input("ZoCWb82JQaCCiAspCqbcUvlsnwg", "   ")
            .validate()
            .is_err());
    }

    #[test]
    fn rejects_path_traversal_in_either_coordinate() {
        // 两个坐标都进 URL 路径段
        assert!(input("../evil", "tblauuOafa4acvT3").validate().is_err());
        assert!(input("ZoCWb82JQaCCiAspCqbcUvlsnwg", "../evil")
            .validate()
            .is_err());
    }

    #[test]
    fn accepts_the_real_ledger_coordinates() {
        // 目标台账的真实坐标（2026-09-23 实测）
        assert!(input("ZoCWb82JQaCCiAspCqbcUvlsnwg", "tblauuOafa4acvT3")
            .validate()
            .is_ok());
    }
}

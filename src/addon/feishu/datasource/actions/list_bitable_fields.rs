//! 列出多维表格字段——配置向导的第三步（勾选要作为外部选项的列）。
//!
//! 返回**全表字段**，不受视图影响（实测 `view_id` 对「列出字段」不生效）。
//! 类型码原样带出，由前端展示、由运维自己判断哪些列适合当选项源。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::bitable::{
    list_all_fields, validate_path_segment, BitableCoordinates,
};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound_setup;

/// 输入契约。两个坐标都进 URL 路径段。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ListBitableFieldsInput {
    /// 多维表格 app_token。
    pub(super) app_token: String,
    /// 数据表 ID。
    pub(super) table_id: String,
}

impl ParamInput for ListBitableFieldsInput {
    fn params() -> Params {
        Params::new()
    }
}

impl ListBitableFieldsInput {
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

/// 注册列出多维表格字段端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("list_bitable_fields"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/datasources/bitable-fields",
        )
        .display_name("列出多维表格字段")
        .description("列出一张数据表的全部字段（含类型码），供配置向导勾选")
        // 与其余元数据端点同权限：只读语义，但**会出站**消耗本应用的频控配额，
        // 与 pull_probe 同类（见其 register 的注释）。
        .permissions(["feishu.datasource.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: ListBitableFieldsInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let settings = match outbound_setup::require_settings(&context) {
        Ok(settings) => settings,
        Err((code, message)) => return Ok(ApiResponse::fail(code, message)),
    };
    let outbound = outbound_setup::build(&ctx, settings)?;

    let items = list_all_fields(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        &BitableCoordinates {
            app_token: input.app_token.trim().to_string(),
            table_id: input.table_id.trim().to_string(),
            view_id: None,
        },
    )
    .await
    .map_err(outbound_setup::outbound_error)?;

    let payload: Vec<serde_json::Value> = items
        .into_iter()
        .map(|item| {
            serde_json::json!({
                "field_id": item.field_id,
                "field_name": item.field_name,
                "type": item.field_type,
                "ui_type": item.ui_type,
            })
        })
        .collect();

    ApiResponse::success(serde_json::json!({ "fields": payload }), "查询成功")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(app_token: &str, table_id: &str) -> ListBitableFieldsInput {
        ListBitableFieldsInput {
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

//! 列出多维表格数据表——配置向导的第一步。
//!
//! 运维只填 `app_token`，表列表由这里取回让他在界面上选。这是把「手填 table_id」
//! 换成「从飞书拉回来选」的第一环。
//!
//! # 为什么不受 `can_pull()` 门禁影响
//!
//! 与 `pull_probe` 那一组不同：那三个端点在没 worker 时注册出来只会答「Worker 未在
//! 运行」。本端点没这个依赖，凭证缺失时返回**可归因的失败**比让路由直接 404 更好查。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::bitable::{list_all_tables, validate_path_segment};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound_setup;

/// 列出数据表的输入契约。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ListBitableTablesInput {
    /// 多维表格 app_token（`feishu.cn/base/<这一段>`）。
    pub(super) app_token: String,
}

impl ParamInput for ListBitableTablesInput {
    fn params() -> Params {
        Params::new()
    }
}

impl ListBitableTablesInput {
    /// 入口校验：形状与路径段安全性。
    ///
    /// 在这里挡而不是等到拼 URL——坐标会进**路径段**，`../` 之类的输入能把请求
    /// 打到别的路径上。
    fn validate(&self) -> Result<(), BaseError> {
        let app_token = self.app_token.trim();
        if app_token.is_empty() {
            return Err(BaseError::ParamInvalid(
                "app_token".to_string(),
                "不能为空".to_string(),
            ));
        }
        validate_path_segment("bitable_base_token", app_token)
            .map_err(|error| BaseError::ParamInvalid("app_token".to_string(), error.to_string()))
    }
}

/// 注册列出数据表端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("list_bitable_tables"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/datasources/bitable-tables",
        )
        .display_name("列出多维表格数据表")
        .description("用自建应用凭证列出某个多维表格 App 下的数据表，供配置向导选择")
        // 只读：不发 GET 之外的东西、不落库、不改数据源状态。但它**会出站调飞书**
        // 并消耗本应用的频控配额，所以与 pull_probe 同理归到 write 一侧。
        .permissions(["feishu.datasource.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: ListBitableTablesInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let settings = match outbound_setup::require_settings(&context) {
        Ok(settings) => settings,
        Err((code, message)) => return Ok(ApiResponse::fail(code, message)),
    };

    let outbound = outbound_setup::build(&ctx, settings)?;
    let tables = list_all_tables(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        input.app_token.trim(),
    )
    .await
    .map_err(outbound_setup::outbound_error)?;

    let items: Vec<serde_json::Value> = tables
        .into_iter()
        .map(|table| {
            serde_json::json!({
                "table_id": table.table_id,
                "name": table.name,
            })
        })
        .collect();

    ApiResponse::success(serde_json::json!({ "tables": items }), "查询成功")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(app_token: &str) -> ListBitableTablesInput {
        ListBitableTablesInput {
            app_token: app_token.to_string(),
        }
    }

    #[test]
    fn rejects_blank_app_token() {
        assert!(input("   ").validate().is_err(), "空 app_token 必须被拒");
    }

    #[test]
    fn rejects_a_path_traversal_app_token() {
        // 它要进 URL 路径段：直接采信会让 `../` 把请求打到别的路径上
        assert!(input("../evil").validate().is_err());
    }

    #[test]
    fn accepts_a_real_app_token_shape() {
        // 目标台账的真实 app_token（2026-09-23 实测）
        assert!(input("ZoCWb82JQaCCiAspCqbcUvlsnwg").validate().is_ok());
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        // 从 URL 里复制粘贴时常带首尾空白，不该让运维自己先 trim
        assert!(input("  ZoCWb82JQaCCiAspCqbcUvlsnwg  ").validate().is_ok());
    }
}

//! 飞书批量审批 module：待办列表/详情/用户搜索与异步 job。
//!
//! 所有 Action 只要求登录（authenticated-only）：审批数据由用户自己的
//! `user_access_token` 限定，服务端不做跨用户查询。

mod actions;
pub(crate) mod domain;
pub(crate) use actions::cancel_job::cancel_user_jobs;
pub(crate) use domain::{api, worker};

use std::sync::Arc;

use yang_base::action::TokenAuthMiddleware;
use yang_base::definition::{ModuleName, ModulePresentationSpec, ModuleSpec};
use yang_base::BaseError;

use crate::addon::account::user_from_claims;
use crate::authorization::AuthorizationVersionValidator;

use super::domain::batch_computed_field_table;
use super::domain::batch_job_table;
use super::domain::context::FeishuContext;

/// 只声明计算字段配置表，CRUD 由批量审批 module 的 Action 经上下文跨表完成。
pub(crate) fn build_computed_field_module() -> Result<ModuleSpec, BaseError> {
    Ok(ModuleSpec::new(
        ModuleName::new("feishu.batch_computed_field")
            .map_err(|error| BaseError::ConfigError(error.to_string()))?,
    )
    .table(batch_computed_field_table::table_spec()?))
}

pub(crate) fn build_module(
    context: Arc<FeishuContext>,
    authorization_validator: AuthorizationVersionValidator,
) -> Result<ModuleSpec, BaseError> {
    let module = ModuleSpec::new(
        ModuleName::new("feishu.batch_approval")
            .map_err(|error| BaseError::ConfigError(error.to_string()))?,
    )
    .table(batch_job_table::table_spec()?)
    .middleware(
        TokenAuthMiddleware::new(user_from_claims)
            .with_claims_validator(authorization_validator)
            .authenticate_public_actions(),
    );
    Ok(actions::register_all(module, context).presentation(
        ModulePresentationSpec::new(crate::addon::user_identity(), "批量审批", "batch_approval")
            .app_route("/batch-approval")
            .description("批量处理当前账号自己的飞书待办")
            .order(20)
            .primary_action(yang_base::action!(
                "feishu.batch_approval.feishu_batch_list_tasks"
            )),
    ))
}

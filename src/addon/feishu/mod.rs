//! 飞书外部数据源集成。
//!
//! 本 addon 对飞书审批暴露「关联外部选项」接口，并为飞书多维表格自动化工作流提供
//! 选项写入 API。所有飞书契约细节（请求/响应形状、加解密、来源校验）收敛在
//! `domain/` 内，module 层只做装配。

pub(crate) mod datasource;
pub(crate) mod domain;
pub(crate) mod option;

use std::sync::Arc;

use sqlx::MySqlPool;
use yang_base::definition::{AddonName, AddonSpec};
use yang_base::BaseError;

use crate::authorization::AuthorizationVersionValidator;
use crate::config::FeishuSettings;

use self::domain::context::FeishuContext;
use self::domain::repository::Repository;

/// 装配飞书 Addon。
///
/// `pool` 由组合根从 `Tools` 取得——`Registry::dispatch` 只向 Action 注入所在 module
/// 的主表，所以两张表的 Repository 必须在这里一次性绑好、经 [`FeishuContext`] 共享。
pub(crate) fn build_addon(
    pool: Arc<MySqlPool>,
    settings: Option<Arc<FeishuSettings>>,
    authorization_validator: AuthorizationVersionValidator,
) -> Result<AddonSpec, BaseError> {
    let context = Arc::new(FeishuContext::new(
        Repository::new(
            datasource::table::table_spec()?.table_definition()?,
            Arc::clone(&pool),
        ),
        Repository::new(
            option::table::table_spec()?.table_definition()?,
            Arc::clone(&pool),
        ),
        settings.clone(),
    ));

    Ok(AddonSpec::new(
        AddonName::new("feishu").map_err(|error| BaseError::ConfigError(error.to_string()))?,
    )
    .module(datasource::build_module(
        Arc::clone(&context),
        authorization_validator.clone(),
    )?)
    .module(option::build_module(
        context,
        settings.as_deref(),
        authorization_validator,
    )?))
}

//! `feishu.option` Module：选项数据。
//!
//! 本 module 同时承载三类入口，用「谁在用」区分：
//! - 控制台**只读**查询（`list_options`，受保护 Action）
//! - 飞书审批取选项（`approval_options`，public + 按数据源 Token 自校验）
//! - 多维表格写入（`upsert_options` / `delete_options`，public + 管理 Token 中间件）
//!
//! # 控制台对选项只读，是架构决策不是缺失
//!
//! 选项的增改由多维表格写入 API 承担。两个并存的可写入口会让审计语义与数据来源
//! 分叉——「这条选项是谁写的」将无法回答。所以本 module 不挂任何选项写操作。
//!
//! # 为什么刻意不声明 `presentation()` 与 `view()`
//!
//! 与 `feishu.datasource` 同一取舍：控制台的选项视图是自建页面的一部分
//! （详情页），不是引擎投影的通用 TableView。两者必须成对省略，理由与半吊子改法的
//! 两个后果见 `datasource/mod.rs` 的文件头说明。

pub(crate) mod actions;
pub(crate) mod table;

use std::sync::Arc;

use yang_base::definition::{ModuleName, ModuleSpec};
use yang_base::BaseError;

use super::domain::context::FeishuContext;
use crate::addon::feishu::datasource::with_authentication;
use crate::authorization::AuthorizationVersionValidator;
use crate::config::FeishuSettings;

/// 本 module 的名字。
const MODULE: &str = "feishu.option";

/// 装配 `feishu.option` Module。
pub(crate) fn build_module(
    context: Arc<FeishuContext>,
    settings: Option<&FeishuSettings>,
    authorization_validator: AuthorizationVersionValidator,
) -> Result<ModuleSpec, BaseError> {
    let spec = ModuleSpec::new(module_name()?).table(table::table_spec()?);
    // 与 datastore 模块同一套认证中间件：本 module 的受保护 Action（list_options）
    // 要靠它才有身份。public 的机器入口**不经过**它——`Next::run` 对默认
    // `ProtectedActions` scope 的判据是 `!policy.is_public`，它们本来就被跳过，
    // 因此匿名调用照样放行。别为了让它们「放行匿名」而加
    // `authenticate_public_actions()`：那会把 scope 抬到 `AllActions`，让本中间件抢走
    // 管理 Token 中间件要用的 `Authorization` 头，把两条写入入口打成不可用
    // （见 `datasource::with_authentication` 的说明）。
    let spec = with_authentication(spec, authorization_validator);
    actions::register_all(spec, context, settings)
}

fn module_name() -> Result<ModuleName, BaseError> {
    ModuleName::new(MODULE).map_err(config_error)
}

fn config_error(error: impl std::fmt::Display) -> BaseError {
    BaseError::ConfigError(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_name_is_the_stable_qualified_identifier() {
        let name = module_name().unwrap_or_else(|error| panic!("模块名应有效: {error}"));
        assert_eq!(name.as_str(), MODULE);
    }

    #[test]
    fn table_spec_is_still_declared_after_dropping_the_view_projection() {
        let spec = table::table_spec().unwrap_or_else(|error| panic!("表声明应有效: {error}"));
        let definition = spec
            .table_definition()
            .unwrap_or_else(|error| panic!("应可编译为表定义: {error}"));
        assert_eq!(definition.name(), "feishu_option");
    }
}

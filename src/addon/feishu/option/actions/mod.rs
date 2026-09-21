//! `feishu.option` module 的 Action 注册表。
//!
//! 架构门禁要求：`actions/` 下每个文件恰好一个 `pub(super) async fn handle` +
//! 一个 `pub(super) fn register`，并在这里登记。

pub(super) mod approval_options;
pub(super) mod delete_options;
pub(super) mod upsert_options;

use std::sync::Arc;

use yang_base::definition::{ActionName, ActionRef, ModuleName, ModuleSpec};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::middleware::ManagementTokenMiddleware;
use crate::config::FeishuSettings;

/// 注册本 module 的全部 Action。
///
/// **`[feishu]` 段未启用时不注册任何飞书路由。** 两条写入入口的凭证来自该段，
/// 取选项端点虽然按数据源各自存凭证，但加密密钥同样来自该段——而「注册但拒绝所有
/// 请求」会让端点出现在 Catalog 里，给人它可用的错觉。这与配置文档的措辞一致：
/// 「关闭时不注册任何飞书路由」。
///
/// 中间件用 `target_action()` 精确限定到单个 Action：取选项端点是 public 且按数据源
/// 自校验，不能被管理 Token 中间件误伤。
pub(super) fn register_all(
    module: ModuleSpec,
    context: Arc<FeishuContext>,
    settings: Option<&FeishuSettings>,
) -> Result<ModuleSpec, BaseError> {
    let Some(settings) = settings.filter(|value| value.is_usable()) else {
        return Ok(module);
    };

    let mut module = approval_options::register(module, Arc::clone(&context));
    module = upsert_options::register(module, Arc::clone(&context));
    module = delete_options::register(module, context);

    for action in ["upsert_options", "delete_options"] {
        module = module.middleware(ManagementTokenMiddleware::new(
            &settings.management_api_token,
            action_ref(action)?,
        ));
    }
    Ok(module)
}

/// 构造本 module 内某个 Action 的引用，供 `target_action()` 限定。
fn action_ref(name: &str) -> Result<ActionRef, BaseError> {
    let module = ModuleName::new("feishu.option")
        .map_err(|error| BaseError::ConfigError(error.to_string()))?;
    let action =
        ActionName::new(name).map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(ActionRef::new(module, action))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_ref_points_at_this_module() {
        let reference =
            action_ref("upsert_options").unwrap_or_else(|error| panic!("应有效: {error}"));
        assert_eq!(reference.module().as_str(), "feishu.option");
        assert_eq!(reference.action().as_str(), "upsert_options");
    }

    #[test]
    fn invalid_action_name_is_rejected() {
        // 大写或含点的名字不是合法 ActionName，必须报错而不是静默通过
        assert!(action_ref("UpsertOptions").is_err());
        assert!(action_ref("a.b").is_err());
    }
}

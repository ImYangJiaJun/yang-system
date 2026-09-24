//! `feishu.option` module 的 Action 注册表。
//!
//! 架构门禁要求：`actions/` 下每个文件恰好一个 `pub(super) async fn handle` +
//! 一个 `pub(super) fn register`，并在这里登记。

pub(super) mod approval_options;
pub(super) mod delete_options;
pub(super) mod list_options;
pub(super) mod upsert_options;

use std::sync::Arc;

use yang_base::definition::{ActionName, ActionRef, ModuleName, ModuleSpec};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::middleware::ManagementTokenMiddleware;
use crate::addon::feishu::domain::request_log::MachineRequestLogMiddleware;
use crate::config::FeishuSettings;

/// 请求参数日志覆盖的机器入口。
///
/// 刻意只有这三个 public 端点。控制台侧的 Action 不在其中：`rotate_token` /
/// `update_datasource_table` 一类的请求体里带着**轮换中的新凭据**，而它们要回答的是
/// 「谁改了什么」——那是审计的职责，不是请求日志的。
const LOGGED_ACTIONS: &[&str] = &["approval_options", "upsert_options", "delete_options"];

/// 本次装配要挂请求日志中间件的 Action。
///
/// 抽成只吃开关的纯函数，是为了让「关闭 ⇒ 一个中间件都不挂」这条能被单测钉住。它坏掉
/// 时**没有任何症状**：要么在没打算记录的部署上开始记录凭据明文，要么反过来——以为
/// 开了却什么都没记，而那时你正等着报文来定位问题。
fn logged_actions(log_inbound_requests: bool) -> &'static [&'static str] {
    if log_inbound_requests {
        LOGGED_ACTIONS
    } else {
        &[]
    }
}

/// 注册本 module 的全部 Action。
///
/// 按「谁在用」分两组：
///
/// - **控制台查询**（`list_options`）始终注册：它声明 `feishu.option.read` 权限、走框架
///   JWT 鉴权，与 `[feishu]` 段无关——运维在接通飞书之前就该能查看选项数据。
/// - **机器入口**（取选项端点 + 两条写入）只在 `[feishu]` 段启用且管理 Token 非空时注册。
///   取选项端点虽然按数据源各自存凭证，但加密密钥同样来自该段；而「注册但拒绝所有请求」
///   会让端点出现在 Catalog 里，给人它可用的错觉。这与配置文档的措辞一致。
///
/// 中间件用 `target_action()` 精确限定到单个写入 Action：取选项端点是 public 且按
/// 数据源自校验，不能被管理 Token 中间件误伤。
pub(super) fn register_all(
    module: ModuleSpec,
    context: Arc<FeishuContext>,
    settings: Option<&FeishuSettings>,
) -> Result<ModuleSpec, BaseError> {
    let module = list_options::register(module, Arc::clone(&context));

    let Some(settings) = settings.filter(|value| value.is_usable()) else {
        return Ok(module);
    };

    let mut module = approval_options::register(module, Arc::clone(&context));
    module = upsert_options::register(module, Arc::clone(&context));
    module = delete_options::register(module, context);

    // 请求参数日志：默认关闭，开启时覆盖三个机器入口（见 `[feishu].log_inbound_requests`）。
    // **必须注册在管理 Token 中间件之前**：中间件链按注册顺序执行，`Next::run` 命中
    // 第一个适用者即把它当链的下一环——排在后面的中间件看不到被前面短路掉的请求，
    // 而「管理 Token 配错了」正是最需要从日志里认出来的那一类。
    for action in logged_actions(settings.log_inbound_requests) {
        module = module.middleware(MachineRequestLogMiddleware::new(action_ref(action)?));
    }

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

    /// 开关关闭时必须**一个中间件都不挂**：这是「关掉就没有运行期开销、也不会把凭据
    /// 写进日志」的实现点，而它坏掉时没有任何症状。
    #[test]
    fn request_log_middleware_is_attached_only_when_the_switch_is_on() {
        assert!(
            logged_actions(false).is_empty(),
            "log_inbound_requests = false 时不得挂任何请求日志中间件"
        );
        assert_eq!(
            logged_actions(true),
            &["approval_options", "upsert_options", "delete_options"][..],
            "开启时恰好覆盖三个机器入口"
        );
    }

    /// 三个入口必须是合法的 `ActionName`。
    ///
    /// 这条只守**语法**。「名字是否真的解析成已注册的 Action」只有 App 构建期的
    /// `validate_references` 能判定，由
    /// `app::tests::feishu_request_log_switch_still_builds_a_valid_app`（把开关打开装配一次）
    /// 覆盖。语法层仍值得单列：它是唯一在改动现场就报错的闸门，构建期那道要在
    /// 装配整棵 Addon 树时才亮。
    #[test]
    fn logged_action_names_are_syntactically_valid() {
        for action in LOGGED_ACTIONS {
            assert!(action_ref(action).is_ok(), "{action} 不是合法 ActionName");
        }
    }

    #[test]
    fn invalid_action_name_is_rejected() {
        // 大写或含点的名字不是合法 ActionName，必须报错而不是静默通过
        assert!(action_ref("UpsertOptions").is_err());
        assert!(action_ref("a.b").is_err());
    }
}

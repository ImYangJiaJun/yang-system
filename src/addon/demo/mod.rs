//! 演示 Addon（addon 层）：便签业务，验证新业务全流程接入成本（P7）。
//!
//! 两层结构：`notes/` 是 module 层（表、Action 注册表与展示投影）；
//! `domain/` 是 addon 层共享机制（模块上下文与便签持久化边界）。

pub(crate) mod domain;
mod notes;

use crate::authorization::AuthorizationVersionValidator;
use yang_base::definition::AddonSpec;
use yang_base::BaseError;

/// 构建演示 Addon。
///
/// Addon 边界负责声明产品能力及其 Module；应用层不应直接拼装 `demo.notes`。
pub(crate) fn build_addon(
    authorization_validator: AuthorizationVersionValidator,
) -> Result<AddonSpec, BaseError> {
    Ok(AddonSpec::new(yang_base::addon!("demo"))
        .module(notes::build_module(authorization_validator)?))
}

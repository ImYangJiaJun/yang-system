//! 飞书 SSO Action 注册表（每个 Action 一个自包含文件）。

mod authorize;
mod bind_authorize;
mod callback;
mod choose;
mod choose_bind;
mod choose_create;
mod identities;
mod mfa;
mod mfa_email_code;
mod status;
mod unbind_identity;

use std::sync::Arc;

use yang_base::definition::ModuleSpec;

use crate::addon::account::ExternalLoginPort;
use crate::addon::feishu::domain::context::FeishuContext;

pub(super) fn register_all(
    module: ModuleSpec,
    context: Arc<FeishuContext>,
    external_login: Arc<dyn ExternalLoginPort>,
) -> ModuleSpec {
    let module = status::register(module, Arc::clone(&context));
    let module = authorize::register(module, Arc::clone(&context));
    let module = callback::register(module, Arc::clone(&context), Arc::clone(&external_login));
    let module = choose::register(module, Arc::clone(&context));
    let module = choose_bind::register(module, Arc::clone(&context), Arc::clone(&external_login));
    let module = choose_create::register(module, Arc::clone(&context), Arc::clone(&external_login));
    let module = mfa::register(module, Arc::clone(&context), Arc::clone(&external_login));
    let module = mfa_email_code::register(module, Arc::clone(&external_login));
    let module = identities::register(module, Arc::clone(&external_login));
    let module = bind_authorize::register(module, Arc::clone(&context));
    unbind_identity::register(module, context, external_login)
}

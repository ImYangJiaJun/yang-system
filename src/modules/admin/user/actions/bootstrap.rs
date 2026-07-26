//! 一次性初始化首个平台超级管理员。

use super::super::service::{AdminService, BootstrapResult};
use crate::bootstrap_secret::BootstrapSecretVerifier;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{ModuleSpec, Password, Str};
use yang_base::{Action, BaseError};

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) BootstrapInput {
        secret: Password::new()
            .title("运维初始化凭证")
            .require(true)
            .min_length(32)
            .max_length(1024),
        name: Str::new().title("姓名").require(true).min_length(1).max_length(50),
        position: Str::new().title("职务").max_length(50),
    }
}

#[derive(Action)]
#[action(
    name = "bootstrap",
    display_name = "初始化平台管理员",
    description = "由持有运维初始化凭证的已登录用户一次性创建首个平台超级管理员，成功后需要刷新 Token",
    method = "POST",
    path = "/api/v1/admin/bootstrap",
    success_status = 201
)]
struct BootstrapAction {
    service: Arc<AdminService>,
}

#[async_trait]
impl ActionHandler for BootstrapAction {
    type Input = BootstrapInput;
    type Output = BootstrapResult;

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let actor_id = ctx.authenticated_user().map(|user| user.id);
        let request_id = ctx.request_id();
        if let Err(error) = authorize_bootstrap_secret(&ctx, &input.secret).await {
            tracing::warn!(
                ?actor_id,
                %request_id,
                outcome = "credential_rejected",
                error_code = error.code_str(),
                "平台管理员初始化"
            );
            return Err(error);
        }

        let result = self
            .service
            .bootstrap(&ctx, &input.name, input.position.as_deref())
            .await;
        match &result {
            Ok(_) => tracing::info!(
                ?actor_id,
                %request_id,
                outcome = "succeeded",
                "平台管理员初始化"
            ),
            Err(error) => tracing::warn!(
                ?actor_id,
                %request_id,
                outcome = "failed",
                error_code = error.code_str(),
                "平台管理员初始化"
            ),
        }
        result
    }
}

async fn authorize_bootstrap_secret(
    context: &ActionContext,
    candidate: &str,
) -> Result<(), BaseError> {
    let verifier = context.tools().config::<BootstrapSecretVerifier>()?;
    if verifier.verify(candidate).await? {
        return Ok(());
    }
    Err(BaseError::Unauthorized(
        "bootstrap 初始化凭证无效".to_string(),
    ))
}

pub(super) fn register(module: ModuleSpec, service: Arc<AdminService>) -> ModuleSpec {
    module.native_action(BootstrapAction { service })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap_secret::{generate_bootstrap_secret, BootstrapSecretVerifier};
    use yang_base::action::Request;
    use yang_base::definition::{ParamInput, WidgetHint};
    use yang_base::tools::ToolsBuilder;

    #[test]
    fn bootstrap_input_requires_a_password_shaped_secret() {
        let missing = serde_json::from_value::<BootstrapInput>(serde_json::json!({
            "name": "Root Admin"
        }));
        assert!(missing.is_err(), "缺少 bootstrap secret 必须在输入边界失败");

        let input = serde_json::from_value::<BootstrapInput>(serde_json::json!({
            "name": "Root Admin",
            "secret": "operator-bootstrap-secret-with-sufficient-length"
        }))
        .unwrap_or_else(|error| panic!("完整 bootstrap 输入应可解析: {error}"));
        assert_eq!(
            input.secret,
            "operator-bootstrap-secret-with-sufficient-length"
        );

        let params = <BootstrapInput as ParamInput>::params();
        let secret = params
            .as_slice()
            .iter()
            .find(|param| param.name.as_str() == "secret")
            .unwrap_or_else(|| panic!("bootstrap 契约应声明 secret 参数"));
        assert!(secret.required);
        assert_eq!(secret.presentation.widget, Some(WidgetHint::Password));
    }

    #[tokio::test]
    async fn bootstrap_gate_fails_closed_and_only_accepts_the_configured_secret() {
        let generated = generate_bootstrap_secret()
            .unwrap_or_else(|error| panic!("bootstrap secret 应生成成功: {error:#}"));
        let verifier = BootstrapSecretVerifier::new(generated.digest().clone(), 1)
            .unwrap_or_else(|error| panic!("verifier 应构建成功: {error}"));
        let tools = Arc::new(
            ToolsBuilder::new()
                .config(verifier)
                .build()
                .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}")),
        );
        let context = ActionContext::new(Request::new(serde_json::json!({})), tools);

        let wrong =
            authorize_bootstrap_secret(&context, "wrong-bootstrap-secret-with-sufficient-length")
                .await;
        assert!(matches!(wrong, Err(BaseError::Unauthorized(_))));
        authorize_bootstrap_secret(&context, generated.secret())
            .await
            .unwrap_or_else(|error| panic!("正确 secret 应通过数据库前置门禁: {error}"));

        let missing_tools = Arc::new(
            ToolsBuilder::new()
                .build()
                .unwrap_or_else(|error| panic!("空测试 Tools 应构建成功: {error}")),
        );
        let missing_context =
            ActionContext::new(Request::new(serde_json::json!({})), missing_tools);
        assert!(matches!(
            authorize_bootstrap_secret(&missing_context, generated.secret()).await,
            Err(BaseError::ConfigError(_))
        ));
    }
}

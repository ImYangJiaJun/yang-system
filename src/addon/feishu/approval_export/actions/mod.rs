//! `feishu.approval_export` 的 Action 注册表。
//!
//! 回写是独立于「审批派发」的功能：规则列表、运行台账、保存/体检/试算都走
//! 这里。只读本库的 5 个 Action 无条件注册；需要打飞书的 4 个 Action 与派发
//! 端点同一条 `can_pull()` 门禁——缺凭证时 404，比「注册出来永远失败」语义更准。

pub(super) mod export_check_comment;
pub(super) mod export_check_rules;
pub(super) mod export_create_rule;
pub(super) mod export_delete_rule;
pub(super) mod export_force_recreate;
pub(super) mod export_list_comment_runs;
pub(super) mod export_list_rules;
pub(super) mod export_list_runs;
pub(super) mod export_preview;
pub(super) mod export_retry_comment_run;
pub(super) mod export_retry_run;
pub(super) mod export_test_comment;
pub(super) mod export_update_rule;

use std::sync::Arc;

use yang_base::definition::ModuleSpec;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::config::FeishuSettings;

/// 注册本 module 的全部 Action。
pub(super) fn register_all(
    module: ModuleSpec,
    context: Arc<FeishuContext>,
    settings: Option<&FeishuSettings>,
) -> Result<ModuleSpec, BaseError> {
    // 只读写本库：缺出站凭证时也应当能查看规则、运行记录并做人工重试 / 重建。
    let module = export_list_rules::register(module, Arc::clone(&context));
    let module = export_list_runs::register(module, Arc::clone(&context));
    let module = export_delete_rule::register(module, Arc::clone(&context));
    let module = export_retry_run::register(module, Arc::clone(&context));
    let module = export_force_recreate::register(module, Arc::clone(&context));
    // 评论台账的查询与人工重试同样只读写本库。
    let module = export_list_comment_runs::register(module, Arc::clone(&context));
    let module = export_retry_comment_run::register(module, Arc::clone(&context));

    let Some(settings) = settings.filter(|value| value.can_pull()) else {
        return Ok(module);
    };
    // 门禁只判断 `can_pull()`，具体值本身不再被后续使用。
    let _ = settings;

    // 保存 / 更新 / 体检 / 试算都要打飞书（审批定义、目标列、实例详情）。
    let module = export_create_rule::register(module, Arc::clone(&context));
    let module = export_update_rule::register(module, Arc::clone(&context));
    let module = export_check_rules::register(module, Arc::clone(&context));
    let module = export_preview::register(module, Arc::clone(&context));
    // 评论体检与试发都要打飞书（目标列 / 订阅状态 / 评论接口）。
    let module = export_check_comment::register(module, Arc::clone(&context));
    let module = export_test_comment::register(module, context);
    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::*;

    use yang_base::definition::{HttpMethod, ModuleName, TableSpec};

    use crate::addon::feishu::domain::repository::Repository;

    const READ_ROUTES: &[&str] = &[
        "/api/v1/feishu/approval/export/rules/query",
        "/api/v1/feishu/approval/export/runs/query",
        "/api/v1/feishu/approval/export/rules/delete",
        "/api/v1/feishu/approval/export/runs/retry",
        "/api/v1/feishu/approval/export/runs/force-recreate",
        "/api/v1/feishu/approval/export/comment/runs/query",
        "/api/v1/feishu/approval/export/comment/runs/retry",
    ];

    const GATED_ROUTES: &[&str] = &[
        "/api/v1/feishu/approval/export/rules/create",
        "/api/v1/feishu/approval/export/rules/update",
        "/api/v1/feishu/approval/export/rules/check",
        "/api/v1/feishu/approval/export/preview",
        "/api/v1/feishu/approval/export/comment/check",
        "/api/v1/feishu/approval/export/comment/test",
    ];

    fn registered_module(with_credentials: bool) -> ModuleSpec {
        let pool = Arc::new(
            sqlx::MySqlPool::connect_lazy("mysql://user:pass@localhost:3306/yang")
                .unwrap_or_else(|error| panic!("惰性连接池应可构造: {error}")),
        );
        let definition = |spec: TableSpec| {
            spec.table_definition()
                .unwrap_or_else(|error| panic!("应可编译为表定义: {error}"))
        };
        let repository = |spec: Result<TableSpec, _>| {
            Repository::new(
                definition(spec.unwrap_or_else(|error| panic!("{error}"))),
                Arc::clone(&pool),
            )
        };
        let context = Arc::new(FeishuContext::new(
            repository(crate::addon::feishu::datasource::table::table_spec()),
            repository(crate::addon::feishu::datasource::domain::field_table::table_spec()),
            repository(crate::addon::feishu::option::table::table_spec()),
            repository(crate::addon::feishu::approval::table::table_spec()),
            repository(crate::addon::feishu::approval::domain::field_map_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::task_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::group_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::request_log_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::task_applicant_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::user_token_table::table_spec()),
            repository(crate::addon::feishu::portal::table::table_spec()),
            repository(crate::addon::feishu::portal::domain::log_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::event_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::export_rule_table::table_spec()),
            repository(
                crate::addon::feishu::approval::domain::export_field_map_table::table_spec(),
            ),
            repository(crate::addon::feishu::approval::domain::export_run_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::comment_run_table::table_spec()),
            repository(crate::addon::feishu::approval::domain::bitable_event_table::table_spec()),
            Arc::new(
                yang_base::action::auth::PasswordEngine::new(1)
                    .unwrap_or_else(|e| panic!("PasswordEngine 应可构造: {e}")),
            ),
            None,
        ));
        let settings = with_credentials.then(|| FeishuSettings {
            enabled: true,
            management_api_token: "a-real-management-token-value-1234".to_string(),
            encryption_key: None,
            app_id: Some("cli_real_app_id".to_string()),
            app_secret: Some("real-app-secret-value".to_string()),
            oauth_redirect_uri: None,
            pull_interval_seconds: 900,
            alert_recipients: Vec::new(),
            alert_failure_threshold: 3,
            log_inbound_requests: false,
            approval_create_rate_per_minute: 90,
            approval_scan_interval_seconds: 30,
            approval_base_timezone: "Asia/Shanghai".to_string(),
            event_verification_token: None,
            event_encrypt_key: None,
        });
        let spec = ModuleSpec::new(
            ModuleName::new("feishu.approval_export").unwrap_or_else(|error| panic!("{error}")),
        );
        register_all(spec, context, settings.as_ref())
            .unwrap_or_else(|error| panic!("应可注册: {error}"))
    }

    fn routes(module: &ModuleSpec) -> Vec<(HttpMethod, String)> {
        module
            .actions()
            .iter()
            .map(|spec| (spec.route.method, spec.route.path.clone()))
            .collect()
    }

    #[tokio::test]
    async fn read_only_routes_are_registered_without_credentials() {
        let module = registered_module(false);
        for path in READ_ROUTES {
            assert!(
                routes(&module).contains(&(HttpMethod::Post, path.to_string())),
                "无凭证时只读路由 {path} 也必须注册：{:?}",
                routes(&module)
            );
        }
    }

    #[tokio::test]
    async fn outbound_routes_follow_the_credential_gate() {
        let without = registered_module(false);
        for path in GATED_ROUTES {
            assert!(
                !routes(&without).contains(&(HttpMethod::Post, path.to_string())),
                "无凭证时出站路由 {path} 不得注册"
            );
        }
        let with = registered_module(true);
        for path in READ_ROUTES.iter().chain(GATED_ROUTES.iter()) {
            assert!(
                routes(&with).contains(&(HttpMethod::Post, path.to_string())),
                "有凭证时路由 {path} 必须注册：{:?}",
                routes(&with)
            );
        }
    }
}

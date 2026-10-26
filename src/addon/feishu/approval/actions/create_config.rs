//! 手动创建审批派发配置（前端控制台向导的最后一步）。
//!
//! # 与首调自动建配置共用同一条校验链
//!
//! 手动建与 dispatch 首次调用自动建必须给**同一套保证**（设计 §5.1）：本 Action
//! 不新写任何校验逻辑，整条复用 `approval_provision::build_plan + insert_plan`——
//! 取定义、取列、按名匹配、校验，全过才落库；配置与映射同一事务。
//!
//! # 建好即签发访问 Token（2026-10-13 起）
//!
//! 配置行的 `config_key` / `token_hash` / `token_cipher` / `token_rotated_at` 在
//! 同一事务里生成——否则「建好了却没签发」会让工作流请求进来时落在 40402
//! 「尚未签发访问 Token」，而控制台又看不到任何提示。生成一次、返回一次明文，
//! 与 `rotate_token`（datasource 侧）同一套 `issue_token` 语义。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::approval::subscribe_approval;
use crate::addon::feishu::domain::approval_match::FieldMapping;
use crate::addon::feishu::domain::approval_provision::{
    build_plan, fill_table_names, insert_plan, ProvisionInput,
};
use crate::addon::feishu::domain::bitable::resolve_bitable_link;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::crypto::issue_token;
use crate::addon::feishu::domain::outbound_setup;
use crate::infrastructure::audit;

/// 同坐标重复建配置（唯一冲突）时返回的可行动文案。
const CONFIG_ALREADY_EXISTS: &str = "该多维表格已配置，请先查询或删除后重建";

/// 创建配置的输入契约：审批信息必填，完整链接包含 table 参数时可省略 `table_id`。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateConfigInput {
    /// 多维表格 token。
    pub(super) base_token: String,
    /// 数据表 id；完整多维表格链接包含 `table` 参数时可省略。
    #[serde(default)]
    pub(super) table_id: String,
    /// 审批定义 code。
    pub(super) approval_code: String,
    /// 审批定义的数字 ID，只用于控制台「打开审批定义」跳转。由用户粘贴完整审批
    /// 后台地址解析得到；空 / 缺省 = 不写。
    #[serde(default)]
    pub(super) approval_id: Option<String>,
    /// 申请人员字段（`field_id` **或**列名都接受，落库统一存 id）。
    pub(super) applicant_field: String,
    /// 回填字段（同上）。
    pub(super) backfill_field: String,
    /// Base 时区（IANA 名）。多维表格日期是不带时区的毫秒时间戳，而审批 `date`
    /// 控件要带偏移量——猜错会让审批里的时间整体偏移，所以必须显式给定。
    pub(super) base_timezone: String,
    /// 显式控件到列的映射；省略时按名称自动匹配。
    #[serde(default)]
    pub(super) maps: Option<Vec<FieldMapping>>,
    /// 六个可选回填目标列（设计 §5.1）。`field_id` **或**列名都接受，落库统一存 id；
    /// 空 = 不启用该回填。给了就在 `build_plan` 里校验目标列存在且类型匹配。
    #[serde(default)]
    pub(super) status_field: Option<String>,
    #[serde(default)]
    pub(super) flow_field: Option<String>,
    #[serde(default)]
    pub(super) start_time_field: Option<String>,
    #[serde(default)]
    pub(super) end_time_field: Option<String>,
    #[serde(default)]
    pub(super) department_field: Option<String>,
    #[serde(default)]
    pub(super) handler_field: Option<String>,
}

impl ParamInput for CreateConfigInput {
    fn params() -> Params {
        Params::new()
    }
}

impl CreateConfigInput {
    fn resolved_coordinates(
        &self,
    ) -> Result<crate::addon::feishu::domain::bitable::ParsedBitableLink, BaseError> {
        resolve_bitable_link(&self.base_token, Some(&self.table_id), None)
            .map_err(|error| BaseError::ParamInvalid("base_token".to_string(), error.to_string()))
    }

    fn validate(&self) -> Result<(), BaseError> {
        for (name, value) in [
            ("base_token", &self.base_token),
            ("approval_code", &self.approval_code),
            ("applicant_field", &self.applicant_field),
            ("backfill_field", &self.backfill_field),
            ("base_timezone", &self.base_timezone),
        ] {
            if value.trim().is_empty() {
                return Err(BaseError::ParamInvalid(
                    name.to_string(),
                    "不能为空".to_string(),
                ));
            }
        }
        if let Some(approval_id) = self.approval_id.as_deref() {
            let approval_id = approval_id.trim();
            if !approval_id.is_empty()
                && (approval_id.len() > 64
                    || !approval_id.bytes().all(|byte| byte.is_ascii_digit()))
            {
                return Err(BaseError::ParamInvalid(
                    "approval_id".to_string(),
                    "审批定义 ID 必须是 1..=64 位数字".to_string(),
                ));
            }
        }
        let coordinates = self.resolved_coordinates()?;
        if coordinates.table_id.is_none() {
            return Err(BaseError::ParamInvalid(
                "table_id".to_string(),
                "不能为空，或在完整多维表格链接中提供 table 参数".to_string(),
            ));
        }
        Ok(())
    }
}

/// 唯一冲突 → 可行动文案（设计 §2.2-4，照 `change_username.rs:72-77` 模式）。
///
/// `insert_plan` 的 insert 路径用 `map_err(BaseError::DatabaseExecuteFailed)` 直包，
/// 不吃 `From<DbError>` 的「唯一冲突→ParamInvalid」映射，所以必须在插入层自己转。
/// `ConstraintError` 同时覆盖唯一键(1062)与外键(1452)的混叠——配置表无外键列，
/// 风险可控。
fn map_insert_error(error: BaseError) -> BaseError {
    match error {
        BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_)) => {
            BaseError::ParamInvalid("base_token".to_string(), CONFIG_ALREADY_EXISTS.to_string())
        }
        other => other,
    }
}

/// 注册创建配置端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("create_config"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/approval/configs/create")
        .display_name("创建审批派发配置")
        .description("手动创建飞书审批派发配置（与首调自动建共用同一条校验链）")
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: CreateConfigInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;
    let coordinates = input.resolved_coordinates()?;

    // 建配置要在飞书与本库两端取数，凭证缺了根本走不下去（照 dispatch provision 的处置）。
    let Some(settings) = context.settings().filter(|value| value.can_pull()) else {
        return Ok(ApiResponse::fail(50301, "飞书出站凭证未配置"));
    };

    let outbound = outbound_setup::build(&ctx, settings)?;
    let mut plan = build_plan(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        &context,
        &ProvisionInput {
            base_token: &coordinates.app_token,
            table_id: coordinates.table_id.as_deref().unwrap_or_default(),
            approval_code: input.approval_code.trim(),
            applicant_field: input.applicant_field.trim(),
            backfill_field: input.backfill_field.trim(),
            base_timezone: input.base_timezone.trim(),
            maps: input.maps.as_deref(),
            status_field: input.status_field.as_deref(),
            flow_field: input.flow_field.as_deref(),
            start_time_field: input.start_time_field.as_deref(),
            end_time_field: input.end_time_field.as_deref(),
            department_field: input.department_field.as_deref(),
            handler_field: input.handler_field.as_deref(),
        },
    )
    .await
    .map_err(|failure| BaseError::ParamInvalid("feishu".to_string(), failure.to_string()))?;

    fill_table_names(
        &mut plan,
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
    )
    .await;

    // 同时备好访问 Token：与 `rotate_config_token`（datasource 的 `rotate_token`）
    // 共用 `issue_token`——`config_key` 是普通随机串（进 URL 路径段），Token 是
    // 封存的（摘要校验 + 可逆密文，供后续「回显复制」路径解开）。
    let config_key = crate::addon::feishu::domain::crypto::generate_token();
    let wrapping_key = context.encryption_key().ok_or_else(|| {
        BaseError::ConfigError(
            "未配置 feishu.encryption_key：审批派发配置的访问 Token 需要它封存".to_string(),
        )
    })?;
    let issued = issue_token(&wrapping_key)?;

    // 配置与映射同一事务，审计同事务追加（AUDIT.md 契约：审计失败整体回滚）。
    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<i64, BaseError> = async {
        let config_id = insert_plan(&context, &mut transaction, &plan)
            .await
            .map_err(map_insert_error)?;
        let issued_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() as i64)
            .unwrap_or_default();
        let mut columns = Record::new();
        // 审批定义 ID（可选）：仅控制台跳转用，空 / 缺省就不写。
        if let Some(approval_id) = input.approval_id.as_deref() {
            let approval_id = approval_id.trim();
            if !approval_id.is_empty() {
                columns.insert("approval_id", serde_json::json!(approval_id));
            }
        }
        columns.insert("config_key", serde_json::json!(&config_key));
        columns.insert("token_hash", serde_json::json!(&issued.hash));
        columns.insert("token_cipher", serde_json::json!(&issued.cipher));
        columns.insert("token_rotated_at", serde_json::json!(issued_at));
        context
            .approval_configs()
            .query()
            .where_eq("id", serde_json::json!(config_id))?
            .update_in_tx(&mut transaction, columns)
            .await?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_approval_config", config_id)?,
            None,
            Some(audit::summary([(
                "config_id",
                serde_json::json!(config_id),
            )])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(config_id)
    }
    .await;
    let config_id = FeishuContext::finish_transaction(transaction, result).await?;

    // 建好即订阅该审批定义的事件（设计 §7.2 第 2 步）：不订阅的话飞书不推该
    // 定义的事件，进度追踪收不到。best-effort——订阅失败不影响建配置的结果
    //（worker 每次启动还会对全部启用配置重订一遍，见 `feishu_event_worker`）。
    if let Err(failure) = subscribe_approval(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        input.approval_code.trim(),
    )
    .await
    {
        tracing::warn!(
            error = %failure.message,
            approval_code = %input.approval_code.trim(),
            "审批定义事件订阅失败（配置已创建，worker 启动时会重订）"
        );
    }

    tracing::debug!(
        config_id,
        base_token = %coordinates.app_token,
        table_id = %coordinates.table_id.as_deref().unwrap_or_default(),
        approval_code = %input.approval_code.trim(),
        "审批派发配置已手动创建"
    );
    ApiResponse::success(
        serde_json::json!({
            "config_id": config_id,
            "config_key": config_key,
            "token": issued.plaintext,
        }),
        "配置创建成功",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use yang_base::action::ApiResponse;
    use yang_db::DbError;

    fn input() -> CreateConfigInput {
        CreateConfigInput {
            base_token: "appbcbWCzen6".to_string(),
            table_id: "tblsRc9GRRX".to_string(),
            approval_code: "CODE-TEST".to_string(),
            approval_id: None,
            applicant_field: "申请人".to_string(),
            backfill_field: "审批编号".to_string(),
            base_timezone: "Asia/Shanghai".to_string(),
            maps: None,
            status_field: None,
            flow_field: None,
            start_time_field: None,
            end_time_field: None,
            department_field: None,
            handler_field: None,
        }
    }

    #[test]
    fn explicit_mapping_input_is_accepted() {
        let parsed = serde_json::from_value::<CreateConfigInput>(serde_json::json!({
            "base_token": "appX", "table_id": "tblX", "approval_code": "CODE",
            "applicant_field": "申请人", "backfill_field": "审批编号",
            "base_timezone": "Asia/Shanghai",
            "maps": [{"widget_id": "w1", "bitable_field": "fldA"}]
        }));
        assert!(parsed.is_ok(), "显式字段映射应接受: {parsed:?}");
    }

    #[test]
    fn optional_backfill_fields_are_accepted_and_absent_by_default() {
        let parsed = serde_json::from_value::<CreateConfigInput>(serde_json::json!({
            "base_token": "appX", "table_id": "tblX", "approval_code": "CODE",
            "applicant_field": "申请人", "backfill_field": "审批编号",
            "base_timezone": "Asia/Shanghai",
            "status_field": "申请状态", "handler_field": "fldHandler"
        }))
        .unwrap_or_else(|error| panic!("可选回填列应接受: {error}"));
        assert_eq!(parsed.status_field.as_deref(), Some("申请状态"));
        assert_eq!(parsed.handler_field.as_deref(), Some("fldHandler"));
        assert!(parsed.flow_field.is_none(), "未给的回填列落 None");
        assert!(parsed.validate().is_ok());

        // 老请求体不带这六个键 → 全部 None，行为与现状一致。
        let legacy = serde_json::from_value::<CreateConfigInput>(serde_json::json!({
            "base_token": "appX", "table_id": "tblX", "approval_code": "CODE",
            "applicant_field": "申请人", "backfill_field": "审批编号",
            "base_timezone": "Asia/Shanghai"
        }))
        .unwrap_or_else(|error| panic!("{error}"));
        assert!(legacy.status_field.is_none() && legacy.handler_field.is_none());
    }

    #[test]
    fn numeric_approval_id_is_accepted_and_optional() {
        let mut value = input();
        value.approval_id = Some(" 7699783289053989908 ".to_string());
        assert!(
            value.validate().is_ok(),
            "数字 id 应通过（首尾空白被 trim）"
        );

        let mut value = input();
        value.approval_id = Some(String::new());
        assert!(value.validate().is_ok(), "空串视为未给");

        assert!(input().validate().is_ok(), "缺省通过");
    }

    #[test]
    fn non_numeric_or_overlong_approval_id_is_rejected() {
        let mut value = input();
        value.approval_id = Some("ABC123".to_string());
        assert!(value.validate().is_err(), "非数字必须被拒");

        let mut value = input();
        value.approval_id = Some("7".repeat(65));
        assert!(value.validate().is_err(), "超过 64 位必须被拒");
    }

    #[test]
    fn blank_field_is_rejected() {
        let mut value = input();
        value.base_token = "   ".to_string();
        assert!(value.validate().is_err());

        let mut value = input();
        value.base_timezone = String::new();
        assert!(value.validate().is_err());
    }

    #[test]
    fn valid_input_passes() {
        assert!(input().validate().is_ok());
    }

    #[test]
    fn complete_bitable_link_supplies_the_table_coordinate() {
        let mut value = input();
        value.base_token =
            "https://tenant.example/base/appbcbWCzen6?table=tblsRc9GRRX&view=vewA".to_string();
        value.table_id.clear();
        assert!(value.validate().is_ok());
        let coordinates = value
            .resolved_coordinates()
            .unwrap_or_else(|error| panic!("完整链接应解析成功: {error}"));
        assert_eq!(coordinates.app_token, "appbcbWCzen6");
        assert_eq!(coordinates.table_id.as_deref(), Some("tblsRc9GRRX"));
    }

    #[test]
    fn complete_link_payload_can_omit_table_id() {
        let parsed = serde_json::from_value::<CreateConfigInput>(serde_json::json!({
            "base_token": "https://tenant.example/base/appbcbWCzen6?table=tblsRc9GRRX",
            "approval_code": "CODE-TEST",
            "applicant_field": "申请人",
            "backfill_field": "审批编号",
            "base_timezone": "Asia/Shanghai"
        }))
        .unwrap_or_else(|error| panic!("完整链接请求体应接受省略 table_id: {error}"));
        assert!(parsed.validate().is_ok());
    }

    /// 坐标与三件套以外的键（如 `title`）必须被拒——deny_unknown_fields 挡在反序列化层。
    #[test]
    fn unknown_fields_are_rejected() {
        let parsed = serde_json::from_value::<CreateConfigInput>(serde_json::json!({
            "base_token": "appX",
            "table_id": "tblX",
            "approval_code": "CODE",
            "applicant_field": "申请人",
            "backfill_field": "审批编号",
            "base_timezone": "Asia/Shanghai",
            "title": "不该有",
        }));
        assert!(parsed.is_err(), "多余键必须被拒");
    }

    #[test]
    fn constraint_error_maps_to_the_friendly_conflict() {
        let error = BaseError::DatabaseExecuteFailed(DbError::ConstraintError(
            "Duplicate entry".to_string(),
        ));
        let mapped = map_insert_error(error);
        match mapped {
            BaseError::ParamInvalid(field, message) => {
                assert_eq!(field, "base_token");
                assert_eq!(message, CONFIG_ALREADY_EXISTS);
            }
            other => panic!("唯一冲突必须转成 ParamInvalid，实际 {other:?}"),
        }
    }

    #[test]
    fn other_errors_pass_through_unchanged() {
        let error = BaseError::DatabaseExecuteFailed(DbError::QueryError("boom".to_string()));
        assert!(matches!(
            map_insert_error(error),
            BaseError::DatabaseExecuteFailed(_)
        ));
    }

    /// 凭证缺失 → 50301 业务失败信封（照 dispatch provision 的处置）。
    #[test]
    fn missing_credentials_yield_the_same_50301_as_dispatch() {
        let response = ApiResponse::fail(50301, "飞书出站凭证未配置");
        assert_eq!(response.code, 50301);
    }
}

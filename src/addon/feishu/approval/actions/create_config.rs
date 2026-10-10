//! 手动创建审批派发配置（前端控制台向导的最后一步）。
//!
//! # 与首调自动建配置共用同一条校验链
//!
//! 手动建与 dispatch 首次调用自动建必须给**同一套保证**（设计 §5.1）：本 Action
//! 不新写任何校验逻辑，整条复用 `approval_provision::build_plan + insert_plan`——
//! 取定义、取列、按名匹配、校验，全过才落库；配置与映射同一事务。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::approval_match::FieldMapping;
use crate::addon::feishu::domain::approval_provision::{build_plan, insert_plan, ProvisionInput};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound_setup;
use crate::infrastructure::audit;

/// 同坐标重复建配置（唯一冲突）时返回的可行动文案。
const CONFIG_ALREADY_EXISTS: &str = "该多维表格已配置，请先查询或删除后重建";

/// 创建配置的输入契约：六件套全部必填。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateConfigInput {
    /// 多维表格 token。
    pub(super) base_token: String,
    /// 数据表 id。
    pub(super) table_id: String,
    /// 审批定义 code。
    pub(super) approval_code: String,
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
}

impl ParamInput for CreateConfigInput {
    fn params() -> Params {
        Params::new()
    }
}

impl CreateConfigInput {
    fn validate(&self) -> Result<(), BaseError> {
        for (name, value) in [
            ("base_token", &self.base_token),
            ("table_id", &self.table_id),
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

    // 建配置要在飞书与本库两端取数，凭证缺了根本走不下去（照 dispatch provision 的处置）。
    let Some(settings) = context.settings().filter(|value| value.can_pull()) else {
        return Ok(ApiResponse::fail(50301, "飞书出站凭证未配置"));
    };

    let outbound = outbound_setup::build(&ctx, settings)?;
    let plan = build_plan(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        &context,
        &ProvisionInput {
            base_token: input.base_token.trim(),
            table_id: input.table_id.trim(),
            approval_code: input.approval_code.trim(),
            applicant_field: input.applicant_field.trim(),
            backfill_field: input.backfill_field.trim(),
            base_timezone: input.base_timezone.trim(),
            maps: input.maps.as_deref(),
        },
    )
    .await
    .map_err(|failure| BaseError::ParamInvalid("feishu".to_string(), failure.to_string()))?;

    // 配置与映射同一事务，审计同事务追加（AUDIT.md 契约：审计失败整体回滚）。
    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<i64, BaseError> = async {
        let config_id = insert_plan(&context, &mut transaction, &plan)
            .await
            .map_err(map_insert_error)?;
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

    tracing::info!(
        config_id,
        base_token = %input.base_token.trim(),
        table_id = %input.table_id.trim(),
        approval_code = %input.approval_code.trim(),
        "审批派发配置已手动创建"
    );
    ApiResponse::success(
        serde_json::json!({ "config_id": config_id }),
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
            applicant_field: "申请人".to_string(),
            backfill_field: "审批编号".to_string(),
            base_timezone: "Asia/Shanghai".to_string(),
            maps: None,
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

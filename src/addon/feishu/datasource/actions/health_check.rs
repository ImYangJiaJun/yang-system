//! 数据源体检：把**不可自愈**的问题列出来。
//!
//! # 什么进这份报告，什么不进
//!
//! 判据只有一条：**运维这一次能不能靠改配置修好，且系统下一轮会不会自己跟上**。
//!
//! | 情形 | 能否自愈 | 处理 |
//! |---|---|---|
//! | 字段被**改名** | **能** | 每轮按 `field_id` 解析当前名字，自动跟上。**不进报告** |
//! | 字段被**删除** | 不能 | 报进 `missing_fields`（同时给 `field_id` 与 `source_key`） |
//! | 数据表 / 视图被删除 | 不能 | 报进 `table_missing` / `view_missing` |
//! | 权限被撤 | 不能 | 字段列表根本读不出来，按「查不了」连同原因报出 |
//!
//! **改名刻意不进报告。** 把它报成问题会稀释真正的问题，还会让运维为一件下轮
//! 就会自己消失的事白跑一趟——这正是决策 D2（身份存 `field_id`）要买到的东西。
//!
//! # 「查不了」必须能与「没问题」区分开
//!
//! `unchecked` 非空时 `ok` 必为 `false`：一次因为网络抖动而根本没查成的体检，
//! 报成「没问题」比报错更危险——运维会据此跳过一张其实已经坏掉的表。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::bitable::{
    list_all_fields, list_all_views, BitableCoordinates, FieldItem,
};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound::{
    FailureKind, OutboundFailure, CODES_PERMISSION_DENIED,
};
use crate::addon::feishu::domain::outbound_setup;

/// 失败码。与 `pull_now` 共用数值域（409 段 = 数据源类）。
mod codes {
    /// 数据源不存在。
    pub(super) const SOURCE_NOT_FOUND: i32 = 40401;
    /// 这条源没有可体检的坐标（不是 pull 模式 / 缺 Base Token / 缺数据表 ID）。
    pub(super) const NOT_CHECKABLE: i32 = 40905;
}

/// 体检输入。
///
/// `datasource_id` 走 **body**：与 `update_datasource_table` / `delete_datasource_table`
/// 一致（`datasource_id` 是本模块标识实体的既有口径）。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct HealthCheckInput {
    /// 目标数据源的 `id`。
    pub(super) datasource_id: i64,
}

impl ParamInput for HealthCheckInput {
    fn params() -> Params {
        Params::new()
    }
}

impl HealthCheckInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.datasource_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "datasource_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        Ok(())
    }
}

/// 一条启用中的绑定的定位信息——体检要比对的最小单位。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BindingRef {
    /// 身份。比对按它，**不按名字**。
    pub(super) field_id: String,
    /// 给人看的定位信息：只报 `field_id` 运维看不懂，只报 `source_key` 又定位不到列。
    pub(super) source_key: String,
}

/// 一个「勾了但表里已经没有了」的字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct MissingField {
    /// 多维表格字段 ID。
    pub(super) field_id: String,
    /// 进 URL 的数据源标识——运维在界面上看到的是它。
    pub(super) source_key: String,
}

/// 体检报告。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(super) struct HealthReport {
    /// 没有已知问题**且**每一项都真的查过。
    pub(super) ok: bool,
    /// 勾了但表里已被删除的字段。改名**不在**这里。
    pub(super) missing_fields: Vec<MissingField>,
    /// 配置的视图在表里已不存在。
    pub(super) view_missing: bool,
    /// 数据表已不存在（或应用已无权限读到它）。
    pub(super) table_missing: bool,
    /// 本轮**没能查成**的项及其原因。非空时 `ok` 必为 `false`。
    pub(super) unchecked: Vec<String>,
}

impl HealthReport {
    /// 记一项「本轮没查成」。
    ///
    /// 与「没问题」的区别必须显式——查不了不算问题（不制造假警报），但也不能让
    /// 「报告里没写问题」被读成「一切正常」，所以它同时压住 `ok`。
    pub(super) fn with_unchecked(mut self, line: impl Into<String>) -> Self {
        self.unchecked.push(line.into());
        self.ok = false;
        self
    }
}

/// 纯函数：拿「远端字段」与「勾选的绑定」比对出报告。
///
/// 三种失败模式**一起报**，不在第一件上短路：运维点一次体检就是为了看全要修的东西，
/// 报一件改一件是让人跑 N 趟。
pub(super) fn classify(
    remote: &[FieldItem],
    bindings: &[BindingRef],
    view_ok: bool,
    table_ok: bool,
) -> HealthReport {
    // 按 `field_id` 比对，**不看名字**：名字每轮都会被解析刷新，拿它比会把改名
    // 报成「字段没了」（假警报）。
    let missing_fields: Vec<MissingField> = bindings
        .iter()
        .filter(|binding| {
            !remote
                .iter()
                .any(|field| field.field_id == binding.field_id)
        })
        .map(|binding| MissingField {
            field_id: binding.field_id.clone(),
            source_key: binding.source_key.clone(),
        })
        .collect();

    HealthReport {
        // 三种失败模式是并列的，任一为真都不是「通过」。
        ok: table_ok && view_ok && missing_fields.is_empty(),
        missing_fields,
        view_missing: !view_ok,
        table_missing: !table_ok,
        unchecked: Vec::new(),
    }
}

/// 这个出站失败是否**证明了**表不可达（坐标错 / 表被删 / 权限被撤）？
///
/// 只有带业务码的 `Fatal` 才算证据：`Retry`（网络、频控、5xx）与 `Fatal { code: 0 }`
/// （网关回了非 JSON 的错误页）什么都证明不了。把一次网络抖动报成「表被删了」
/// 就是让运维白跑一趟——与「改名不进报告」是同一条取舍。
fn proves_table_unreachable(failure: &OutboundFailure) -> bool {
    match failure.kind {
        FailureKind::Fatal { code } => {
            // 1254003/1254040 = base token 不存在；1254004/1254009/1254044 = table_id
            // 不存在（分类口径见 `outbound::fatal_hint`）。
            matches!(code, 1254003 | 1254040 | 1254004 | 1254009 | 1254044)
                || CODES_PERMISSION_DENIED.contains(&code)
        }
        _ => false,
    }
}

/// trim 后非空才算有值——`NULL` 与空白串对「能不能体检」是同一件事。
fn trimmed(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// 从表级行拼出坐标；拼不出来时给出**可归因**的理由。
fn coordinates(
    ingest_mode: &str,
    base_token: Option<String>,
    table_id: Option<String>,
    view_id: Option<String>,
) -> Result<BitableCoordinates, String> {
    match (trimmed(base_token), trimmed(table_id)) {
        (Some(app_token), Some(table_id)) => Ok(BitableCoordinates {
            app_token,
            table_id,
            view_id: trimmed(view_id),
        }),
        _ if ingest_mode == "pull" => {
            Err("这条数据源缺少 Base Token 或数据表 ID，无法定位到多维表格".to_string())
        }
        _ => Err("取数方式不是「定时拉取」，这条数据源没有多维表格坐标可体检".to_string()),
    }
}

/// 注册数据源体检端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("health_check"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/datasources/table/health")
        .display_name("数据源体检")
        .description("把勾选的字段与表实际字段比对，列出不可自愈的问题（改名不算问题）")
        // 与其余元数据端点同权限：只读语义，但它**会出站**调飞书、消耗本应用的
        // 频控配额，属运维动作，不该和纯读的控制台查询共用同一权限位。
        .permissions(["feishu.datasource.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: HealthCheckInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    input.validate()?;

    let settings = match outbound_setup::require_settings(&context) {
        Ok(settings) => settings,
        Err((code, message)) => return Ok(ApiResponse::fail(code, message)),
    };

    // 1) 这条数据源与它的坐标。体检要出站，坐标缺了就无从谈起——那是**体检不成立**，
    //    不是「这条源没问题」，所以不能报成一张全绿的报告。
    let row = context
        .datasources()
        .query()
        .select_fields(&[
            "ingest_mode",
            "bitable_base_token",
            "bitable_table_id",
            "bitable_view_id",
        ])?
        .where_eq("id", serde_json::json!(input.datasource_id))?
        .optional()
        .await?;
    let Some(row) = row else {
        return Ok(ApiResponse::fail(codes::SOURCE_NOT_FOUND, "数据源不存在"));
    };
    let coordinates = match coordinates(
        row.optional::<String>("ingest_mode")?
            .as_deref()
            .unwrap_or(""),
        row.optional::<String>("bitable_base_token")?,
        row.optional::<String>("bitable_table_id")?,
        row.optional::<String>("bitable_view_id")?,
    ) {
        Ok(coordinates) => coordinates,
        Err(reason) => return Ok(ApiResponse::fail(codes::NOT_CHECKABLE, reason)),
    };

    let outbound = outbound_setup::build(&ctx, settings)?;

    // 2) 只查**启用中**的绑定：取消勾选 = 停用不删行，停用的列本就不参与拉取，
    //    它的列被删了也不是问题。
    let binding_rows = context
        .datasource_fields()
        .query()
        .select_fields(&["field_id", "source_key"])?
        .where_eq("datasource_id", serde_json::json!(input.datasource_id))?
        .where_eq("enabled", serde_json::json!(true))?
        .all()
        .await?;
    let bindings: Vec<BindingRef> = binding_rows
        .iter()
        .map(|row| {
            Ok(BindingRef {
                field_id: row.require("field_id")?,
                source_key: row.require("source_key")?,
            })
        })
        .collect::<Result<_, BaseError>>()?;

    // 3) 两个出站查询**分别**记结果。视图接口挂了不该让字段缺失报不出来，
    //    反过来也一样。
    let fields = list_all_fields(
        outbound.transport(),
        outbound.sleeper(),
        &outbound.tokens,
        &coordinates,
    )
    .await;

    // 没配视图（= 取全表）时压根没有要校验的对象，也就不必白跑一趟列视图。
    let (view_ok, view_unchecked) = match coordinates.view_id.as_deref() {
        None => (true, None),
        Some(wanted) => match list_all_views(
            outbound.transport(),
            outbound.sleeper(),
            &outbound.tokens,
            &coordinates.app_token,
            &coordinates.table_id,
        )
        .await
        {
            Ok(views) => (views.iter().any(|view| view.view_id.trim() == wanted), None),
            // 查不了 ≠ 没有了：不报成问题（假警报），但也不假装查过。
            Err(failure) => (true, Some(format!("视图列表查询失败：{failure}"))),
        },
    };

    let mut report = match &fields {
        Ok(remote) => classify(remote, &bindings, view_ok, true),
        // 有证据表不可达（坐标错 / 表被删 / 权限被撤）：`table_missing` 与逐列清单
        // **一起报**。每一条绑定此刻确实解析不到，运维要改的就是这份清单。
        Err(failure) if proves_table_unreachable(failure) => {
            classify(&[], &bindings, view_ok, false)
        }
        // 其余失败（网络、频控、5xx）什么都证明不了。**不能**拿空远端去比对：
        // 那会把每一条绑定都报成「字段已被删除」，运维会为一堆根本没发生的事白跑。
        Err(failure) => {
            let line = format!("字段列表查询失败：{failure}");
            classify(&[], &[], view_ok, true).with_unchecked(line)
        }
    };
    if let Some(line) = view_unchecked {
        report = report.with_unchecked(line);
    }

    ApiResponse::success(report, "体检完成")
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::addon::feishu::domain::projection_contract;

    /// 体检报告的键集与契约对账。
    ///
    /// 嵌套项单独对一次：`missing_fields[]` 是**另一个形状**（`field_id` / `source_key`），
    /// 前端在那两个键上拼给运维看的标识，漏一个就画不出「哪一列被删了」。
    #[test]
    fn the_committed_contract_matches_the_report_structs() {
        let report = HealthReport {
            ok: true,
            missing_fields: vec![MissingField {
                field_id: "fldA".to_string(),
                source_key: "k".to_string(),
            }],
            view_missing: false,
            table_missing: false,
            unchecked: vec!["x".to_string()],
        };
        projection_contract::assert_keys(
            &report,
            &["health_check", "report", "emitted"],
            "体检报告",
        );

        let missing = MissingField {
            field_id: "fldA".to_string(),
            source_key: "k".to_string(),
        };
        projection_contract::assert_keys(
            &missing,
            &["health_check", "missing_field", "emitted"],
            "体检的缺失字段项",
        );
    }

    fn remote_field(field_id: &str, field_name: &str) -> FieldItem {
        FieldItem {
            field_id: field_id.to_string(),
            field_name: field_name.to_string(),
            field_type: Some(3),
            ui_type: Some("SingleSelect".to_string()),
        }
    }

    fn binding(field_id: &str, source_key: &str) -> BindingRef {
        BindingRef {
            field_id: field_id.to_string(),
            source_key: source_key.to_string(),
        }
    }

    #[test]
    fn rename_is_not_reported_as_a_problem() {
        // 改名能自愈（每轮按 field_id 解析名字）→ 不得进体检列表。
        // 把它报成问题会让运维白跑一趟，也会稀释真正的问题。
        let remote = vec![remote_field("fldA", "币种（改名后）")];
        let bindings = vec![binding("fldA", "currency")];
        let report = classify(
            &remote, &bindings, /* view_ok */ true, /* table_ok */ true,
        );
        assert!(report.ok);
        assert!(report.missing_fields.is_empty());
    }

    #[test]
    fn a_deleted_field_is_reported_by_id_and_key() {
        // 只报 field_id 运维看不懂；只报 source_key 定位不到列。两个都要给。
        let remote = vec![remote_field("fldA", "币种")];
        let bindings = vec![binding("fldA", "currency"), binding("fldGONE", "old_rate")];
        let report = classify(&remote, &bindings, true, true);
        assert!(!report.ok);
        assert_eq!(report.missing_fields.len(), 1);
        assert_eq!(report.missing_fields[0].field_id, "fldGONE");
        assert_eq!(report.missing_fields[0].source_key, "old_rate");
    }

    #[test]
    fn every_failure_mode_is_listed_together_not_short_circuited() {
        // 表被删了、视图也没了、还缺字段 → 三件事一起报，别只报第一件
        let report = classify(&[], &[binding("fldGONE", "k")], false, false);
        assert!(report.table_missing, "表没了必须报出来");
        assert!(report.view_missing, "视图没了必须一起报");
        assert!(!report.ok);
    }

    #[test]
    fn a_configuration_with_nothing_wrong_is_ok() {
        let remote = vec![remote_field("fldA", "币种"), remote_field("fldB", "汇率")];
        let bindings = vec![binding("fldA", "currency"), binding("fldB", "fx")];
        let report = classify(&remote, &bindings, true, true);
        assert!(report.ok);
        assert!(report.missing_fields.is_empty());
        assert!(!report.view_missing);
        assert!(!report.table_missing);
        assert!(report.unchecked.is_empty());
    }

    #[test]
    fn a_deleted_view_alone_makes_the_report_not_ok() {
        let remote = vec![remote_field("fldA", "币种")];
        let bindings = vec![binding("fldA", "currency")];
        let report = classify(&remote, &bindings, /* view_ok */ false, true);
        assert!(report.view_missing);
        assert!(!report.ok);
        assert!(report.missing_fields.is_empty(), "缺视图不该牵连字段");
    }

    #[test]
    fn an_unchecked_item_suppresses_ok() {
        // 一次根本没查成的体检**不能**报成「没问题」：运维会据此跳过一张坏表。
        let report = classify(&[], &[], true, true).with_unchecked("字段列表查询失败：超时");
        assert!(!report.ok, "查不了就不能说通过");
        assert!(report.missing_fields.is_empty(), "查不了不等于字段被删");
        assert_eq!(report.unchecked.len(), 1);
    }

    fn failure(kind: FailureKind) -> OutboundFailure {
        OutboundFailure {
            kind,
            message: "HTTP 400: {\"code\":…}".to_string(),
        }
    }

    #[test]
    fn a_transient_failure_does_not_claim_the_table_is_gone() {
        // 网络抖动 / 频控 / 5xx / 网关错误页：什么都证明不了。报成「表被删了」
        // 与把改名报成问题同罪——让运维为一件没发生的事白跑一趟。
        for kind in [
            FailureKind::Retry {
                retry_after_seconds: None,
            },
            FailureKind::TokenExpired,
            FailureKind::Fatal { code: 0 },
        ] {
            assert!(
                !proves_table_unreachable(&failure(kind)),
                "{kind:?} 不该被当成表没了"
            );
        }
    }

    #[test]
    fn a_missing_table_or_a_revoked_permission_is_proof() {
        // 1254040 = base token 不存在，1254004 = table_id 不存在，1254302 = 权限被撤
        for code in [
            1254003, 1254040, 1254004, 1254009, 1254044, 1254302, 1254303,
        ] {
            assert!(
                proves_table_unreachable(&failure(FailureKind::Fatal { code })),
                "{code} 应被判为表不可达"
            );
        }
        // 1254024 是**字段名**层面的失败（`field_names` 不匹配），不是表没了
        assert!(!proves_table_unreachable(&failure(FailureKind::Fatal {
            code: 1254024
        })));
    }

    #[test]
    fn coordinates_come_from_the_table_row_and_say_what_is_missing() {
        let ok = coordinates(
            "pull",
            Some("ZoCWb82JQaCCiAspCqbcUvlsnwg".to_string()),
            Some("tblauuOafa4acvT3".to_string()),
            Some("vewAEKSbvO".to_string()),
        )
        .unwrap_or_else(|reason| panic!("坐标齐备应可体检: {reason}"));
        assert_eq!(ok.app_token, "ZoCWb82JQaCCiAspCqbcUvlsnwg");
        assert_eq!(ok.view_id.as_deref(), Some("vewAEKSbvO"));

        // 空白串与 NULL 对「能不能体检」是同一件事
        let blank = coordinates(
            "pull",
            Some("   ".to_string()),
            Some("tbl".to_string()),
            None,
        );
        assert!(blank.is_err(), "空白 base token 不算有值");

        let missing = coordinates("pull", None, Some("tbl".to_string()), None)
            .err()
            .unwrap_or_default();
        assert!(
            missing.contains("Base Token"),
            "理由要点名缺什么: {missing}"
        );

        let push = coordinates("push", None, None, None)
            .err()
            .unwrap_or_default();
        assert!(
            push.contains("定时拉取"),
            "push 源的真正原因是模式不对，不是缺列: {push}"
        );
    }

    #[test]
    fn rejects_a_non_positive_datasource_id() {
        assert!(HealthCheckInput { datasource_id: 0 }.validate().is_err());
        assert!(HealthCheckInput { datasource_id: -1 }.validate().is_err());
        assert!(HealthCheckInput { datasource_id: 7 }.validate().is_ok());
    }
}

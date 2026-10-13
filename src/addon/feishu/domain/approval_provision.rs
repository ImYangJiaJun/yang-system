//! 审批派发配置：坐标 + 审批定义 + 多维表格列 → config 行 + field_map 行。
//!
//! # 为什么要有这一层
//!
//! 默认按名称匹配控件、派发表列与外部选项来源；控制台也可分别手选列与来源，
//! 无需修改飞书侧名称。派发端点在**配置不存在**时取定义、取列、自动匹配、
//! 校验，全过才落库。新建与编辑复用同一条校验链。
//!
//! # 两条铁律
//!
//! 1. **配置期就报错**：一次返回**全部**不成立的控件，而不是遇到第一个就停。
//!    改一条看一条会来回多轮才配好。
//! 2. **存 id、用名匹配**是两条轴：存储一律用 `field_id`（改列名不会让配置失效），
//!    而 API 的投影与回写用**字段名**（`field_names` 与 `batch_update` 的 `fields` key
//!    都要名字）。[`ProvisionPlan`] 里两个关键坐标因此各带一份 id 与 name。
//!
//! # 本层只「建计划」，不落库
//!
//! 落库要事务，而事务的生命周期属于 Action/worker。所以 [`build_plan`] 只产出
//! 待插入的 `Record`，由调用方 [`insert_plan`] 在自己的事务里提交——配置与映射
//! 必须**同一事务**，否则「配置进去了、映射没进去」会让下一次派发直接报
//! 「该配置没有字段映射」，而配置行看起来是好的。

#![allow(dead_code)] // 建配置先落地并自带 8 个测试；消费者（dispatch 端点的自动创建）在下一步接入。

use std::collections::BTreeMap;

use serde_json::Value;
use yang_base::table::Record;
use yang_base::BaseError;
use yang_db::Transaction;

use super::approval::get_approval_definition;
use super::approval_convert::{Converter, WidgetMap};
use super::approval_match::{
    columns_from_fields, match_fields, resolve_field_coord, Column, FieldMapping, FormWidget,
    MatchOutcome,
};
use super::bitable::{list_all_fields, BitableCoordinates, FieldItem};
use super::context::FeishuContext;
use super::outbound::{OutboundTransport, Sleeper};
use super::repository::all_pages;
use super::tenant_token::TenantTokenProvider;

/// 自动建配置的入参。三个坐标由工作流在 `raw_body` 里写死——它们是**只有调用方
/// 知道**的信息（要提哪个定义、谁当发起人、编号写回哪列）。
pub(crate) struct ProvisionInput<'a> {
    pub(crate) base_token: &'a str,
    pub(crate) table_id: &'a str,
    pub(crate) approval_code: &'a str,
    /// 申请人列：`field_id` **或**列名都接受（见 `resolve_field_coord`）。
    pub(crate) applicant_field: &'a str,
    /// 回填列：同上。
    pub(crate) backfill_field: &'a str,
    pub(crate) base_timezone: &'a str,
    pub(crate) maps: Option<&'a [FieldMapping]>,
    /// 六个可选回填目标列（设计 §5.1）。`field_id` **或**列名都接受；
    /// `None`/空串 = 不回填该列。给了就在建配置时校验目标列存在且类型匹配。
    pub(crate) status_field: Option<&'a str>,
    pub(crate) flow_field: Option<&'a str>,
    pub(crate) start_time_field: Option<&'a str>,
    pub(crate) end_time_field: Option<&'a str>,
    pub(crate) department_field: Option<&'a str>,
    pub(crate) handler_field: Option<&'a str>,
}

/// 建配置失败。
#[derive(Debug)]
pub(crate) enum ProvisionError {
    /// 校验不过——**全部**原因，逐条可行动。
    Invalid(Vec<String>),
    /// 飞书侧取数失败（网络 / 权限 / 定义不存在）。
    Fetch(String),
    /// 本库查询失败。
    Store(String),
}

impl From<BaseError> for ProvisionError {
    fn from(error: BaseError) -> Self {
        Self::Store(error.to_string())
    }
}

impl std::fmt::Display for ProvisionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reasons) => {
                write!(formatter, "配置校验不通过（{} 条）：", reasons.len())?;
                for (index, reason) in reasons.iter().enumerate() {
                    if index > 0 {
                        write!(formatter, "；")?;
                    }
                    write!(formatter, "{reason}")?;
                }
                Ok(())
            }
            Self::Fetch(message) => write!(formatter, "取飞书数据失败：{message}"),
            Self::Store(message) => write!(formatter, "读本库配置失败：{message}"),
        }
    }
}

impl std::error::Error for ProvisionError {}

/// 校验通过的配置材料——**还不是行**。
///
/// # 为什么行不在这里
///
/// 两张 `Record` 要在各自表的接收者（`approval_configs()` / `approval_field_maps()`）
/// **旁边**构造：离线对账门禁 `schema_anchor` 按「语句里最近的那个表接收者」判定
/// 归属，把行拿到别处去填会把归属解析成别的表（实测解析成 `feishu_option`，
/// 于是拿 `feishu_option` 的声明去验 `base_token` 这类本就只在配置表上的列，
/// 报出一串假的 `FieldNotFound`）。所以组装挪进 [`insert_plan`]——它那两个
/// `*_in_tx` 接收者就在同一函数里。
#[derive(Debug)]
pub(crate) struct ProvisionPlan {
    /// 审批定义名，用来做配置标题（比 `table_id` 好读，控制台按它筛）。
    pub(crate) approval_name: String,
    pub(crate) base_name: Option<String>,
    pub(crate) table_name: Option<String>,
    pub(crate) applicant_field_name: String,
    pub(crate) backfill_field_name: String,
    pub(crate) base_token: String,
    pub(crate) table_id: String,
    pub(crate) approval_code: String,
    pub(crate) applicant_field_id: String,
    pub(crate) backfill_field_id: String,
    /// 六个可选回填目标列的 `field_id`（设计 §5.1）。`None` = 该列不回填。
    pub(crate) status_field_id: Option<String>,
    pub(crate) flow_field_id: Option<String>,
    pub(crate) start_time_field_id: Option<String>,
    pub(crate) end_time_field_id: Option<String>,
    pub(crate) department_field_id: Option<String>,
    pub(crate) handler_field_id: Option<String>,
    pub(crate) base_timezone: String,
    /// 审批定义的 `form` 原文快照。
    ///
    /// 存**原文**而不是 `FormWidget` 序列化回去的结果：后者只保留了本服务要读的
    /// 字段，会把 `printable` / `display_condition` 等丢掉，而快照的价值正是
    /// 「下次审批定义改了，和当初配的对不上能看得出来」。
    pub(crate) form_snapshot: String,
    /// 校验后确定的列（含当前列名）与控件映射，组装待插行用。
    pub(crate) columns: Vec<Column>,
    pub(crate) widgets: Vec<WidgetMap>,
}

/// 解析审批定义的 `form`。
///
/// 实测里它是 **JSON 字符串**（`"[{\"id\":\"w1\"…}]"`）而不是数组——
/// 所以两种形态都要认。字符串形态是官方文档里的样子，数组形态是理想值；
/// 只认一种会在另一半响应上整个解析失败，而失败会表现为「审批定义没有表单」。
pub(crate) fn parse_form(form: &Value) -> Result<Vec<FormWidget>, ProvisionError> {
    let parsed: Vec<FormWidget> = match form {
        Value::Null => {
            return Err(ProvisionError::Invalid(vec![
                "审批定义没有返回表单（`form` 为空）".to_string(),
            ]))
        }
        Value::String(raw) => serde_json::from_str(raw).map_err(|error| {
            ProvisionError::Invalid(vec![format!("审批定义的表单解析失败：{error}")])
        })?,
        Value::Array(_) => serde_json::from_value(form.clone()).map_err(|error| {
            ProvisionError::Invalid(vec![format!("审批定义的表单解析失败：{error}")])
        })?,
        other => {
            return Err(ProvisionError::Invalid(vec![format!(
                "审批定义的表单形态无法识别：{}",
                type_name(other)
            )]))
        }
    };
    if parsed.is_empty() {
        return Err(ProvisionError::Invalid(vec![
            "审批定义的表单里没有任何控件".to_string(),
        ]));
    }
    Ok(parsed)
}

/// 审批定义 `form` 的**原文**快照文本。
///
/// 实测里它有时是 JSON 字符串、有时是数组（`parse_form` 两种都认）。快照统一存
/// **数组形态**：不统一的话，下次比对得先猜这次是哪种，而「猜」在这里意味着
/// 「审批定义改了却看不出来」。
fn form_snapshot_text(form: Option<&Value>) -> String {
    match form {
        Some(Value::String(raw)) => raw.clone(),
        Some(value @ Value::Array(_)) => serde_json::to_string(value).unwrap_or_default(),
        _ => String::new(),
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// 取定义、取列、按名匹配、校验——**产出待插入的行，不落库**。
pub(crate) async fn build_plan(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    context: &FeishuContext,
    input: &ProvisionInput<'_>,
) -> Result<ProvisionPlan, ProvisionError> {
    let coordinates = BitableCoordinates {
        app_token: input.base_token.to_string(),
        table_id: input.table_id.to_string(),
        view_id: None,
    };

    let fields = list_all_fields(transport, sleeper, tokens, &coordinates)
        .await
        .map_err(|failure| ProvisionError::Fetch(failure.message))?;
    if fields.is_empty() {
        return Err(ProvisionError::Invalid(vec![
            "多维表格这张表里没有任何字段（权限不足或表为空）".to_string(),
        ]));
    }
    let columns = columns_from_fields(&fields);

    // 六个可选回填目标列：给了就校验存在且类型匹配（设计 §5.1），全部原因一次性返回。
    // 放在取定义之前——列不对时省掉一次飞书往返。
    let reasons = check_backfill_targets(
        &columns,
        &fields,
        input.status_field,
        input.flow_field,
        input.start_time_field,
        input.end_time_field,
        input.department_field,
        input.handler_field,
    );
    if !reasons.is_empty() {
        return Err(ProvisionError::Invalid(reasons));
    }

    let definition = get_approval_definition(transport, sleeper, tokens, input.approval_code)
        .await
        .map_err(|failure| ProvisionError::Fetch(failure.message))?;
    // `approvals get` 只对**原生**定义返回 `form`；三方定义走的是
    // `external_approvals` 另一个资源，取不到表单就是最好的判据。
    if definition.is_external {
        return Err(ProvisionError::Invalid(vec![
            "该 approval_code 是三方审批定义，不能用 `instances create` 提单".to_string(),
        ]));
    }
    let form = parse_form(
        definition
            .form
            .as_ref()
            .ok_or_else(|| ProvisionError::Invalid(vec!["审批定义没有返回表单".to_string()]))?,
    )?;

    if let MatchOutcome::Invalid(errors) = match_fields(&form, &columns, input.maps) {
        return Err(ProvisionError::Invalid(
            errors.into_iter().map(|error| error.to_string()).collect(),
        ));
    }
    let mapped_ids = input.maps.map(|maps| {
        maps.iter()
            .map(|map| map.widget_id.as_str())
            .collect::<Vec<_>>()
    });
    let explicit_sources = input
        .maps
        .into_iter()
        .flatten()
        .filter_map(|map| {
            map.option_source_key
                .as_ref()
                .map(|source| (map.widget_id.clone(), source.clone()))
        })
        .collect();
    let external_options =
        load_external_options(context, &form, mapped_ids.as_deref(), &explicit_sources).await?;

    let plan = plan(
        input,
        &definition.approval_name,
        &form,
        &columns,
        &external_options,
        &form_snapshot_text(definition.form.as_ref()),
    )?;
    Ok(plan)
}

/// 展示名称补充失败不改变校验结果，也不清空已有快照。
pub(crate) async fn fill_table_names(
    plan: &mut ProvisionPlan,
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
) {
    match super::bitable::get_base_name(transport, sleeper, tokens, &plan.base_token).await {
        Ok(name) => plan.base_name = name,
        Err(error) => tracing::warn!(%error, "审批配置多维表名称暂未取得"),
    }
    match super::bitable::list_all_tables(transport, sleeper, tokens, &plan.base_token).await {
        Ok(tables) => {
            plan.table_name = tables
                .into_iter()
                .find(|table| table.table_id == plan.table_id)
                .map(|table| table.name)
        }
        Err(error) => tracing::warn!(%error, "审批配置数据表名称暂未取得"),
    }
}

/// 纯校验 + 组装。全部失败原因一次性返回。
fn plan(
    input: &ProvisionInput<'_>,
    approval_name: &str,
    form: &[FormWidget],
    columns: &[Column],
    external_options: &BTreeMap<String, String>,
    form_snapshot: &str,
) -> Result<ProvisionPlan, ProvisionError> {
    let mut reasons: Vec<String> = Vec::new();

    // ---- 两个关键坐标 ----
    //
    // 两条分开判而不是用 `resolve_key_coords`：合并版返回裸 `Option`，
    // 报不出「**哪个**坐标没找到」，而那正是调用方唯一能据此修好的线索。
    let Some(applicant) = resolve_field_coord(columns, input.applicant_field) else {
        reasons.push(format!(
            "申请人在多维表格里没有对应列（给定「{}」）",
            input.applicant_field.trim()
        ));
        // 回填列找不到也要报：一条只报一个会让调用方修一轮再跑一轮。
        if resolve_field_coord(columns, input.backfill_field).is_none() {
            reasons.push(format!(
                "回填列在多维表格里没有对应列（给定「{}」）",
                input.backfill_field.trim()
            ));
        }
        return Err(ProvisionError::Invalid(reasons));
    };
    let Some(backfill) = resolve_field_coord(columns, input.backfill_field) else {
        return Err(ProvisionError::Invalid(vec![format!(
            "回填列在多维表格里没有对应列（给定「{}」）",
            input.backfill_field.trim()
        )]));
    };
    if applicant.field_id == backfill.field_id {
        return Err(ProvisionError::Invalid(vec![format!(
            "申请与回填不能是同一列「{}」",
            applicant.field_name
        )]));
    }

    // ---- 默认按名匹配，控制台可显式指定映射 ----
    let mut widgets = match match_fields(form, columns, input.maps) {
        MatchOutcome::Matched(widgets) => widgets,
        MatchOutcome::Invalid(errors) => {
            return Err(ProvisionError::Invalid(
                errors.into_iter().map(|error| error.to_string()).collect(),
            ))
        }
    };
    if widgets.is_empty() {
        return Err(ProvisionError::Invalid(vec![if input.maps.is_some() {
            "审批定义至少需要配置一条有效字段映射"
        } else {
            "审批控件和多维表格列没有任何一条同名，无法自动装配字段映射"
        }
        .to_string()]));
    }

    // ---- 链接型控件补选项 ----
    //
    // `match_by_name` 对链接态**不报**「派生不出」（它只判固定选项），把这一段
    // 留到这里——因为选项在本系统的库里而不在飞书响应里。
    let by_widget_id: BTreeMap<&str, &FormWidget> = form
        .iter()
        .flat_map(|widget| widget.walk())
        .map(|w| (w.id.as_str(), w))
        .collect();
    for widget in &mut widgets {
        let Some(source) = by_widget_id.get(widget.widget_id.as_str()) else {
            continue;
        };
        if !source.links_to_our_options() {
            continue;
        }
        match external_options.get(&source.id) {
            Some(key) => {
                widget.option_source_key = Some(key.clone());
                widget.option_map.clear();
            }
            None => reasons.push(format!(
                "控件「{}」链接了外部选项数据源，但本系统没有该列的绑定",
                source.name
            )),
        }
    }

    if !reasons.is_empty() {
        return Err(ProvisionError::Invalid(reasons));
    }

    Ok(ProvisionPlan {
        base_name: None,
        table_name: None,
        applicant_field_name: applicant.field_name.clone(),
        backfill_field_name: backfill.field_name.clone(),
        base_token: input.base_token.trim().to_string(),
        table_id: input.table_id.trim().to_string(),
        approval_code: input.approval_code.trim().to_string(),
        // **存 id**：改列名不会让配置失效。
        applicant_field_id: applicant.field_id.clone(),
        backfill_field_id: backfill.field_id.clone(),
        // 六个可选回填列：坐标解析成 id（`None`/空/未命中都落 `None`）。类型与存在性
        // 已在 `build_plan` 的 `check_backfill_targets` 里查过，这里只管存 id。
        status_field_id: resolve_optional_id(columns, input.status_field),
        flow_field_id: resolve_optional_id(columns, input.flow_field),
        start_time_field_id: resolve_optional_id(columns, input.start_time_field),
        end_time_field_id: resolve_optional_id(columns, input.end_time_field),
        department_field_id: resolve_optional_id(columns, input.department_field),
        handler_field_id: resolve_optional_id(columns, input.handler_field),
        base_timezone: input.base_timezone.trim().to_string(),
        approval_name: approval_name.trim().to_string(),
        form_snapshot: form_snapshot.to_string(),
        columns: columns.to_vec(),
        widgets,
    })
}

/// 把「`field_id` 或列名」的可选坐标解析成 `field_id`：`None`/空/未命中都返回 `None`。
///
/// 与 [`resolve_field_coord`] 同一套解析（优先 id、退到列名），只是把「没给」也算
/// 成 `None`——六个可选回填列都用它。
pub(crate) fn resolve_optional_id(columns: &[Column], given: Option<&str>) -> Option<String> {
    given
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| resolve_field_coord(columns, value))
        .map(|column| column.field_id.clone())
}

/// 校验六个可选回填目标列：列存在且类型匹配（设计 §5.1）。未给（`None`/空）跳过。
///
/// 全部原因一次性返回——照本模块「配置期报全」的纪律，改一条看一条要多跑几轮。
/// 供 `build_plan` 与 `update_config` 共用。
#[allow(clippy::too_many_arguments)] // 8 个参数：六列各一个 Option 坐标，收成结构体会改动更大的接口面（照 bootstrap.rs 同款处置）。
pub(crate) fn check_backfill_targets(
    columns: &[Column],
    fields: &[FieldItem],
    status_field: Option<&str>,
    flow_field: Option<&str>,
    start_time_field: Option<&str>,
    end_time_field: Option<&str>,
    department_field: Option<&str>,
    handler_field: Option<&str>,
) -> Vec<String> {
    // (中文名, 输入坐标, 允许的目标列类型码)。顺序与 `ProvisionInput` 的六字段一致。
    let targets: [(&str, Option<&str>, &[i32]); 6] = [
        ("申请状态", status_field, &[1, 3]),
        ("审批流程", flow_field, &[1, 3]),
        ("发起时间", start_time_field, &[5]),
        ("完成时间", end_time_field, &[5]),
        ("发起人部门", department_field, &[1]),
        ("当前处理人", handler_field, &[11]),
    ];
    let mut reasons = Vec::new();
    for (label, given, allowed) in targets {
        let Some(given) = given.map(str::trim).filter(|value| !value.is_empty()) else {
            continue;
        };
        let Some(column) = resolve_field_coord(columns, given) else {
            reasons.push(format!(
                "{label}回填列在多维表格里没有对应列（给定「{given}」）"
            ));
            continue;
        };
        // 列由 `columns_from_fields(fields)` 派生，取不到字段或类型码缺失都不拦——照
        // `check_coordinate_field` 的既有纪律：拿不到元数据就不拒绝本来可用的配置。
        let Some(field) = fields
            .iter()
            .find(|field| field.field_id == column.field_id)
        else {
            continue;
        };
        let Some(field_type) = field.field_type else {
            continue;
        };
        if !allowed.contains(&field_type) {
            let type_label = field
                .ui_type
                .clone()
                .unwrap_or_else(|| format!("type={field_type}"));
            reasons.push(format!(
                "{label}回填列「{}」的类型是 {type_label}（type={field_type}），应为{}",
                column.field_name,
                expected_label(allowed),
            ));
        }
    }
    reasons
}

/// 目标列类型约束的可读名（设计 §5.1 的表）。
fn expected_label(allowed: &[i32]) -> &'static str {
    match allowed {
        [1, 3] => "文本或单选",
        [1] => "文本",
        [5] => "日期",
        [11] => "人员",
        _ => "未知类型",
    }
}

/// 把计划落进调用方的事务：**配置与映射同一事务**，缺一不可。
///
/// 返回新配置的 `id`。映射一条都插不进时回滚整个事务——留一条没有映射的配置
/// 比不建更坏，它会让每一次派发都走到「该配置没有字段映射」，而配置行看着是好的。
///
/// # 为什么行在这里组装
///
/// 两张 `Record` 都放在**表接收者**（`approval_configs()` / `approval_field_maps()`）
/// 的同一语句里构造。离线对账门禁按最近的那个表接收者判定归属——把行搬到别处
/// 填会让归属解析成别的表，于是拿**那张表**的声明去验本表的列，报出一串假的
/// `FieldNotFound`（实测解析成了 `feishu_option`）。
pub(crate) async fn insert_plan(
    context: &FeishuContext,
    transaction: &mut Transaction,
    plan: &ProvisionPlan,
) -> Result<i64, BaseError> {
    // 行**内联**在这里构造，不抽成 helper：离线对账门禁靠「`Record::new()` 的变量
    // 被交给了哪张表的 `*_in_tx`」来判定归属，而那个判定只认**同一函数内**的绑定。
    // 抽成 `fn config_row() -> Record` 之后绑定断了，归属退回宽松档
    // （实测直接报成 `feishu_option`，拿别的表的声明来验本表的列）。
    let mut config_row = Record::new();
    // 标题是必填列。取定义名——比 `table_id` 好读，而控制台按它筛。
    config_row.insert(
        "title",
        serde_json::json!(truncate_title(&plan.approval_name, &plan.table_id)),
    );
    config_row.insert(
        "base_name",
        serde_json::json!(display_name(plan.base_name.as_deref())),
    );
    config_row.insert(
        "table_name",
        serde_json::json!(display_name(plan.table_name.as_deref())),
    );
    config_row.insert(
        "approval_name",
        serde_json::json!(display_name(Some(&plan.approval_name))),
    );
    config_row.insert(
        "applicant_field_name",
        serde_json::json!(display_name(Some(&plan.applicant_field_name))),
    );
    config_row.insert(
        "backfill_field_name",
        serde_json::json!(display_name(Some(&plan.backfill_field_name))),
    );
    config_row.insert("base_token", serde_json::json!(&plan.base_token));
    config_row.insert("table_id", serde_json::json!(&plan.table_id));
    config_row.insert("approval_code", serde_json::json!(&plan.approval_code));
    // **存 id**：改列名不会让配置失效。
    config_row.insert(
        "applicant_field",
        serde_json::json!(&plan.applicant_field_id),
    );
    config_row.insert("backfill_field", serde_json::json!(&plan.backfill_field_id));
    // 六个可选回填列（设计 §5.1）：`None` 落 `NULL`，与 `option_source_key` 同一写法。
    config_row.insert("status_field", serde_json::json!(&plan.status_field_id));
    config_row.insert("flow_field", serde_json::json!(&plan.flow_field_id));
    config_row.insert(
        "start_time_field",
        serde_json::json!(&plan.start_time_field_id),
    );
    config_row.insert("end_time_field", serde_json::json!(&plan.end_time_field_id));
    config_row.insert(
        "department_field",
        serde_json::json!(&plan.department_field_id),
    );
    config_row.insert("handler_field", serde_json::json!(&plan.handler_field_id));
    config_row.insert("base_timezone", serde_json::json!(&plan.base_timezone));
    config_row.insert("enabled", serde_json::json!(true));
    // 定义原文快照。存原文而不是 `FormWidget` 序列化回去的结果——后者只保留本服务
    // 要读的字段，会把 `printable` / `display_condition` 丢掉，而快照的价值正在于
    // 「定义下次改了能不能看出来」。
    config_row.insert("form_snapshot", serde_json::json!(&plan.form_snapshot));
    config_row.insert("form_snapshot_at", serde_json::json!(now_unix_secs()));

    let (affected, config_id) = context
        .approval_configs()
        .query()
        .insert_returning_id_in_tx(transaction, config_row)
        .await?;
    if affected == 0 {
        return Err(BaseError::ConfigError(
            "审批配置插入后未返回主键".to_string(),
        ));
    }

    insert_maps(context, transaction, config_id as i64, plan).await?;
    Ok(config_id as i64)
}

pub(crate) async fn insert_maps(
    context: &FeishuContext,
    transaction: &mut Transaction,
    config_id: i64,
    plan: &ProvisionPlan,
) -> Result<(), BaseError> {
    for widget in &plan.widgets {
        // `bitable_field_name` 是**展示**用的：对齐仍靠 `bitable_field`（id），
        // 名字可被用户改、id 不会。
        let column_name = plan
            .columns
            .iter()
            .find(|column| column.field_id == widget.bitable_field)
            .map(|column| column.field_name.clone())
            .unwrap_or_default();

        let mut map_row = Record::new();
        map_row.insert("config_id", serde_json::json!(config_id));
        map_row.insert("widget_id", serde_json::json!(&widget.widget_id));
        map_row.insert("widget_type", serde_json::json!(&widget.widget_type));
        map_row.insert("required", serde_json::json!(widget.required));
        map_row.insert("bitable_field", serde_json::json!(&widget.bitable_field));
        map_row.insert("bitable_field_name", serde_json::json!(column_name));
        map_row.insert(
            "option_source_key",
            serde_json::json!(&widget.option_source_key),
        );
        map_row.insert(
            "converter",
            serde_json::json!(converter_key(widget.converter)),
        );
        map_row.insert(
            "option_map",
            serde_json::json!(
                serde_json::to_string(&widget.option_map).unwrap_or_else(|_| "{}".to_string())
            ),
        );
        context
            .approval_field_maps()
            .query()
            .insert_in_tx(transaction, map_row)
            .await?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// 外部选项（链接态控件的 option_map 来源）
// ---------------------------------------------------------------------------

/// 验证来源非空，返回控件 ID → source_key；派发表与选项台账的字段 ID 无关。
async fn load_external_options(
    context: &FeishuContext,
    form: &[FormWidget],
    mapped_ids: Option<&[&str]>,
    explicit_sources: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, ProvisionError> {
    let sources = load_option_sources(context, form, mapped_ids, explicit_sources).await?;
    for source in sources.values().collect::<std::collections::BTreeSet<_>>() {
        let rows = context
            .options()
            .query()
            .select_fields(&["option_id"])?
            .where_eq("source_key", serde_json::json!(source))?
            .where_eq("enabled", serde_json::json!(true))?
            .page(1, 1)?
            .all()
            .await
            .map_err(|e| ProvisionError::Store(e.to_string()))?;
        if rows.is_empty() {
            return Err(ProvisionError::Invalid(vec![format!(
                "外部选项数据源「{source}」里没有启用中的选项"
            )]));
        }
    }
    Ok(sources)
}

fn linked_widgets<'a>(form: &'a [FormWidget], mapped_ids: Option<&[&str]>) -> Vec<&'a FormWidget> {
    form.iter()
        .flat_map(|widget| widget.walk())
        .filter(|widget| widget.links_to_our_options())
        .filter(|widget| mapped_ids.map_or(true, |ids| ids.contains(&widget.id.as_str())))
        .collect()
}

async fn load_option_sources(
    context: &FeishuContext,
    form: &[FormWidget],
    mapped_ids: Option<&[&str]>,
    explicit_sources: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, ProvisionError> {
    let linked = linked_widgets(form, mapped_ids);
    if linked
        .iter()
        .any(|widget| widget.name.trim().is_empty() && !explicit_sources.contains_key(&widget.id))
    {
        return Err(ProvisionError::Invalid(vec![
            "外部选项控件缺少名称，无法定位数据源绑定".to_string(),
        ]));
    }
    if linked.is_empty() {
        return Ok(BTreeMap::new());
    }

    let sources = list_option_sources(context).await?;
    select_option_sources(&linked, explicit_sources, &sources)
}

/// 向导与保存/派发共用的启用来源候选，不返回 Token 或其他凭据。
#[derive(Debug, serde::Serialize)]
pub(crate) struct ApprovalOptionSource {
    pub(crate) source_key: String,
    pub(crate) field_name: String,
    pub(crate) datasource_title: String,
}

pub(crate) async fn list_option_sources(
    context: &FeishuContext,
) -> Result<Vec<ApprovalOptionSource>, ProvisionError> {
    let titles: BTreeMap<i64, String> = all_pages(
        context
            .datasources()
            .query()
            .select_fields(&["id", "title"])?
            .where_eq("status", serde_json::json!("active"))?,
        MAX_BINDING_PAGES,
    )
    .await
    .map_err(|error| ProvisionError::Store(error.to_string()))?
    .iter()
    .map(|row| Ok((row.require("id")?, row.require("title")?)))
    .collect::<Result<_, BaseError>>()?;
    let bindings = all_pages(
        context
            .datasource_fields()
            .query()
            .select_fields(&["datasource_id", "field_name", "source_key"])?
            .where_eq("enabled", serde_json::json!(true))?,
        MAX_BINDING_PAGES,
    )
    .await
    .map_err(|error| ProvisionError::Store(error.to_string()))?;

    let mut out = Vec::new();
    for binding in &bindings {
        let Some(title) = titles.get(&binding.require::<i64>("datasource_id")?) else {
            continue;
        };
        let name = binding_display_name(binding)?;
        if name.is_none() {
            tracing::warn!(
                source_key = %binding.optional::<String>("source_key")?.unwrap_or_default(),
                "字段绑定的 field_name 为空，仅支持按 source_key 手动关联"
            );
        }
        out.push(ApprovalOptionSource {
            source_key: binding.require("source_key")?,
            field_name: name.unwrap_or_default(),
            datasource_title: title.clone(),
        });
    }
    Ok(out)
}

fn select_option_sources(
    linked: &[&FormWidget],
    explicit_sources: &BTreeMap<String, String>,
    sources: &[ApprovalOptionSource],
) -> Result<BTreeMap<String, String>, ProvisionError> {
    let mut out = BTreeMap::new();
    let mut reasons = Vec::new();
    for widget in linked {
        if let Some(key) = explicit_sources.get(&widget.id) {
            if sources.iter().any(|source| source.source_key == *key) {
                out.insert(widget.id.clone(), key.clone());
            } else {
                reasons.push(format!(
                    "控件「{}」指定的外部选项来源「{key}」不存在或已停用，请重新选择",
                    widget.name
                ));
            }
            continue;
        }
        let candidates: Vec<_> = sources
            .iter()
            .filter(|source| source.field_name == widget.name.trim())
            .collect();
        match candidates.as_slice() {
            [source] => {
                out.insert(widget.id.clone(), source.source_key.clone());
            }
            [] => reasons.push(format!(
                "控件「{}」链接了外部选项数据源，但本系统没有该列的绑定，请手动选择外部选项来源",
                widget.name
            )),
            _ => reasons.push(format!(
                "控件「{}」在多个外部选项数据源中同名，请手动选择外部选项来源",
                widget.name
            )),
        }
    }
    if !reasons.is_empty() {
        return Err(ProvisionError::Invalid(reasons));
    }
    Ok(out)
}

/// 缺失或空白名称返回 `None`，保留按 source_key 手选的能力，不阻断其他绑定。
///
/// 调用方**必须**为这个 `None` 留一条带 `source_key` 的告警：空名不是常态，
/// 它是上游漏写的症状。
fn binding_display_name(binding: &Record) -> Result<Option<String>, BaseError> {
    let Some(name) = binding.optional::<String>("field_name")? else {
        return Ok(None);
    };
    let name = name.trim().to_string();
    if name.is_empty() {
        return Ok(None);
    }
    Ok(Some(name))
}

/// 查询当前记录用到的文案，仍严格拒绝同名歧义，不猜级联选项 id。
pub(crate) async fn resolve_external_options(
    context: &FeishuContext,
    snapshot: &str,
    widgets: &mut [WidgetMap],
    cells: &serde_json::Map<String, Value>,
) -> Result<(), ProvisionError> {
    let form = parse_form(&Value::String(snapshot.to_string()))?;
    let mapped_ids: Vec<&str> = widgets
        .iter()
        .map(|widget| widget.widget_id.as_str())
        .collect();
    let explicit_sources = widgets
        .iter()
        .filter_map(|widget| {
            widget
                .option_source_key
                .as_ref()
                .map(|source| (widget.widget_id.clone(), source.clone()))
        })
        .collect();
    let sources = load_option_sources(context, &form, Some(&mapped_ids), &explicit_sources).await?;
    for definition in form
        .iter()
        .flat_map(|w| w.walk())
        .filter(|w| w.links_to_our_options())
    {
        let Some(widget) = widgets.iter_mut().find(|w| w.widget_id == definition.id) else {
            continue;
        };
        let Some(cell) = cells.get(&widget.bitable_field) else {
            continue;
        };
        tracing::debug!(stage = "external_options", widget_id = %widget.widget_id,
            bitable_field = %widget.bitable_field, "解析外部选项控件的当前选择");
        let source = sources.get(&definition.id).ok_or_else(|| {
            ProvisionError::Invalid(vec![format!(
                "外部选项控件「{}」缺少数据源绑定",
                definition.name
            )])
        })?;
        widget.option_map.clear();
        for label in super::approval_convert::labels_from_cell(cell)
            .into_iter()
            .filter(|l| !l.trim().is_empty())
        {
            let rows = context
                .options()
                .query()
                .select_fields(&["option_id", "label"])?
                .where_eq("source_key", serde_json::json!(source))?
                .where_eq("enabled", serde_json::json!(true))?
                .where_eq("label", serde_json::json!(label))?
                .page(1, 2)?
                .all()
                .await
                .map_err(|e| ProvisionError::Store(e.to_string()))?;
            let option_id = unique_option(&rows, source, &label).inspect_err(|_| {
                tracing::info!(stage = "external_options", widget_id = %widget.widget_id,
                    bitable_field = %widget.bitable_field, source_key = source,
                    match_count = rows.len(), reason = "当前文案没有唯一且完全匹配的启用选项",
                    "审批外部选项匹配失败");
            })?;
            widget.option_map.insert(label, option_id);
        }
    }
    Ok(())
}

fn unique_option(rows: &[Record], source: &str, label: &str) -> Result<String, ProvisionError> {
    if rows.len() != 1 || rows[0].require::<String>("label")? != label {
        return Err(ProvisionError::Invalid(vec![format!(
            "数据源「{source}」的文案「{label}」没有唯一且完全匹配的启用选项"
        )]));
    }
    Ok(rows[0].require("option_id")?)
}

/// 扫字段绑定表时的页数上界。绑定行数量级在百级（每数据源几列），给足余量。
const MAX_BINDING_PAGES: usize = 20;

/// 标题截断到列上限（100），避免插入期才报「字符串超长」。
/// 展示元数据无效时留空，不阻断配置保存或覆盖已有名称。
pub(crate) fn display_name(name: Option<&str>) -> Option<&str> {
    name.map(str::trim)
        .filter(|name| !name.is_empty() && name.chars().count() <= 512)
}

fn truncate_title(approval_name: &str, table_id: &str) -> String {
    let source = approval_name.trim();
    let source = if source.is_empty() {
        table_id.trim()
    } else {
        source
    };
    source.chars().take(100).collect()
}

pub(crate) fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// `Converter` → 库里的字符串。与 `Converter::parse` 成对。
fn converter_key(converter: Converter) -> &'static str {
    match converter {
        Converter::Direct => "direct",
        Converter::Date => "date",
        Converter::Option => "option",
    }
}

#[cfg(test)]
mod tests {
    use super::super::approval_match::{FormWidget, WidgetOptions};
    use super::*;

    #[test]
    fn selected_option_requires_one_exact_match() {
        let row = |label: &str| Record::new().set("label", label).set("option_id", "id1");
        assert_eq!(
            unique_option(&[row("银行")], "source", "银行").unwrap_or_else(|e| panic!("{e}")),
            "id1"
        );
        assert!(unique_option(&[], "source", "银行").is_err());
        assert!(unique_option(&[row("银行"), row("银行")], "source", "银行").is_err());
        assert!(unique_option(&[row("银行 ")], "source", "银行").is_err());
    }

    /// 取出失败值。不用 `unwrap_err()`：clippy 的 `unwrap_used` 是 deny，
    /// 且它把「取错误值」的变体一并归入——`#[cfg(test)]` 也不例外。
    fn expect_invalid<T>(result: Result<T, ProvisionError>) -> ProvisionError {
        match result {
            Ok(_) => panic!("期望校验失败，实际通过了"),
            Err(error) => error,
        }
    }

    #[test]
    fn display_names_reject_invalid_metadata_without_blocking_config() {
        assert_eq!(display_name(Some("  申请人  ")), Some("申请人"));
        assert_eq!(display_name(None), None);
        assert_eq!(display_name(Some("  ")), None);
        assert_eq!(display_name(Some(&"名".repeat(513))), None);
        assert_eq!(
            display_name(Some(&"名".repeat(512)))
                .map(str::chars)
                .map(Iterator::count),
            Some(512)
        );
    }

    fn column(id: &str, name: &str, options: &[&str]) -> Column {
        Column {
            field_id: id.to_string(),
            field_name: name.to_string(),
            options: options.iter().map(|value| (*value).to_string()).collect(),
        }
    }

    fn input<'a>(applicant: &'a str, backfill: &'a str) -> ProvisionInput<'a> {
        ProvisionInput {
            base_token: "appTest",
            table_id: "tblTest",
            approval_code: "CODE-TEST",
            applicant_field: applicant,
            backfill_field: backfill,
            base_timezone: "Asia/Shanghai",
            maps: None,
            status_field: None,
            flow_field: None,
            start_time_field: None,
            end_time_field: None,
            department_field: None,
            handler_field: None,
        }
    }

    /// 一个字段的最小 `FieldItem`：只给类型校验要用的两样。
    fn field(id: &str, name: &str, field_type: i32) -> FieldItem {
        FieldItem {
            field_id: id.to_string(),
            field_name: name.to_string(),
            field_type: Some(field_type),
            ui_type: None,
            property: None,
        }
    }

    /// 真实形态的最小复刻：**`form` 是 JSON 字符串**。
    const FORM_STRING: &str = r#"[{"id":"w1","name":"SWIFT Address","type":"input","required":true},
        {"id":"w2","name":"收款方类型/Type of payee","type":"radioV2","required":true,
         "option":[{"value":"mpuvnw0h-1","text":"个人"}]}]"#;

    #[test]
    fn explicit_mapping_only_loads_selected_external_sources() {
        let form = parse_form(&serde_json::json!([
            {"id":"plain","name":"备注","type":"input"},
            {"id":"detail","name":"明细","type":"fieldList","children":[
                {"id":"linked","name":" 公司 ","type":"radioV2",
                 "externalData":{"externalDataLinkage":true}},
                {"id":"omitted","name":"银行","type":"radioV2",
                 "externalData":{"externalDataLinkage":true}}
            ]}
        ]))
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            linked_widgets(&form, None)
                .iter()
                .map(|widget| widget.id.as_str())
                .collect::<Vec<_>>(),
            ["linked", "omitted"]
        );
        assert_eq!(
            linked_widgets(&form, Some(&["plain", "linked"]))
                .iter()
                .map(|widget| widget.id.as_str())
                .collect::<Vec<_>>(),
            ["linked"]
        );
        assert!(linked_widgets(&form, Some(&[])).is_empty());
    }

    #[test]
    fn manual_external_source_accepts_different_widget_and_source_names() {
        let maps: Vec<FieldMapping> = serde_json::from_value(serde_json::json!([{
            "widget_id": "company", "bitable_field": "fldCompany",
            "option_source_key": "fldq7lcb6y"
        }]))
        .unwrap_or_else(|error| panic!("手动来源应能被接收: {error}"));
        let form = parse_form(&serde_json::json!([{
            "id": "company", "name": "付款公司名称/Company name", "type": "radioV2",
            "required": true, "option": [], "externalData": {"externalDataLinkage": true}
        }]))
        .unwrap_or_else(|error| panic!("{error}"));
        let result = match_fields(
            &form,
            &[column("fldCompany", "实际付款公司", &[])],
            Some(&maps),
        );
        let MatchOutcome::Matched(widgets) = result else {
            panic!("显式映射应允许名称不同: {result:?}");
        };
        assert_eq!(widgets[0].bitable_field, "fldCompany");
        let sources = vec![ApprovalOptionSource {
            source_key: "fldq7lcb6y".into(),
            field_name: "付款公司名称/Payer Company name".into(),
            datasource_title: "财税费用付款".into(),
        }];
        let external = select_option_sources(
            &linked_widgets(&form, None),
            &BTreeMap::from([("company".into(), "fldq7lcb6y".into())]),
            &sources,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        let result = plan(
            &ProvisionInput {
                maps: Some(&maps),
                ..input("fldP", "fldB")
            },
            "t",
            &form,
            &[
                column("fldCompany", "实际付款公司", &[]),
                column("fldP", "申请人", &[]),
                column("fldB", "审批编号", &[]),
            ],
            &external,
            "[]",
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            result.widgets[0].option_source_key.as_deref(),
            Some("fldq7lcb6y")
        );
        assert!(result.widgets[0].option_map.is_empty());
    }

    #[test]
    fn explicit_source_disambiguates_names_without_falling_back() {
        let form = parse_form(&serde_json::json!([
            {"id":"a","name":"公司","type":"radioV2","externalData":{"externalDataLinkage":true}},
            {"id":"detail","type":"fieldList","children":[
                {"id":"b","name":"公司","type":"radioV2","externalData":{"externalDataLinkage":true}}
            ]}
        ])).unwrap_or_else(|error| panic!("{error}"));
        let sources: Vec<_> = ["first", "second"]
            .iter()
            .map(|key| ApprovalOptionSource {
                source_key: (*key).into(),
                field_name: "公司".into(),
                datasource_title: (*key).into(),
            })
            .collect();
        let linked = linked_widgets(&form, None);
        assert!(select_option_sources(&linked, &BTreeMap::new(), &sources).is_err());
        let explicit =
            BTreeMap::from([("a".into(), "first".into()), ("b".into(), "second".into())]);
        assert_eq!(
            select_option_sources(&linked, &explicit, &sources)
                .unwrap_or_else(|error| panic!("{error}")),
            explicit
        );
        assert!(select_option_sources(
            &linked,
            &BTreeMap::from([("a".into(), "missing".into())]),
            &sources[..1]
        )
        .is_err());
        assert_eq!(
            select_option_sources(&linked, &BTreeMap::new(), &sources[..1])
                .unwrap_or_else(|error| panic!("{error}")),
            BTreeMap::from([("a".into(), "first".into()), ("b".into(), "first".into())])
        );
    }

    #[test]
    fn external_source_is_rejected_for_plain_widgets_and_invalid_keys() {
        let plain = parse_form(&serde_json::json!([{"id":"plain","name":"备注","type":"input"}]))
            .unwrap_or_else(|error| panic!("{error}"));
        let linked = parse_form(&serde_json::json!([{"id":"plain","name":"备注","type":"radioV2","externalData":{"externalDataLinkage":true}}]))
            .unwrap_or_else(|error| panic!("{error}"));
        for (form, source) in [
            (&plain, "source".to_string()),
            (&linked, " ".to_string()),
            (&linked, "x".repeat(65)),
        ] {
            let maps = [FieldMapping {
                widget_id: "plain".into(),
                bitable_field: "field".into(),
                option_source_key: Some(source),
            }];
            assert!(matches!(
                match_fields(form, &[column("field", "备注", &[])], Some(&maps)),
                MatchOutcome::Invalid(_)
            ));
        }
    }

    #[test]
    fn explicitly_empty_mapping_is_rejected() {
        let form = parse_form(&serde_json::json!([
            {"id":"optional","name":"备注","type":"input"}
        ]))
        .unwrap_or_else(|error| panic!("{error}"));
        let error = expect_invalid(plan(
            &ProvisionInput {
                maps: Some(&[]),
                ..input("fldP", "fldB")
            },
            "测试",
            &form,
            &[column("fldP", "申请人", &[]), column("fldB", "编号", &[])],
            &BTreeMap::new(),
            "[]",
        ));
        assert!(error.to_string().contains("至少需要配置一条有效字段映射"));
    }

    #[test]
    fn form_as_json_string_parses() {
        let form = parse_form(&Value::String(FORM_STRING.to_string()))
            .unwrap_or_else(|error| panic!("字符串形态应可解析: {error}"));
        assert_eq!(form.len(), 2);
        assert_eq!(form[0].name, "SWIFT Address");
        assert!(matches!(form[1].option, Some(WidgetOptions::Fixed(_))));
    }

    #[test]
    fn form_as_array_also_parses() {
        let value: Value = serde_json::from_str(FORM_STRING).unwrap_or_else(|e| panic!("{e}"));
        let form = parse_form(&value).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(form.len(), 2);
    }

    #[test]
    fn empty_form_is_rejected_with_reason() {
        let error = expect_invalid(parse_form(&Value::String("[]".to_string())));
        assert!(error.to_string().contains("没有任何控件"), "{error}");
        let error = expect_invalid(parse_form(&Value::Null));
        assert!(error.to_string().contains("没有返回表单"), "{error}");
    }

    #[test]
    fn successful_plan_keeps_both_id_and_name_for_key_columns() {
        // 「存 id」这条在计划里就要落死：改列名不能让配置失效。
        let plan = plan(
            &input("fldP", "fldB"),
            "往来付款类型",
            &[FormWidget {
                id: "w1".to_string(),
                name: "SWIFT Address".to_string(),
                r#type: "input".to_string(),
                required: false,
                option: None,
                external_data: None,
                value: serde_json::Value::Null,
                children: Vec::new(),
            }],
            &[
                column("fldP", "申请人", &[]),
                column("fldB", "审批编号", &[]),
                column("fldC", "SWIFT Address", &[]),
            ],
            &BTreeMap::new(),
            "[]",
        )
        .unwrap_or_else(|error| panic!("{error}"));

        // 断言在**校验产物**上，不在待插行上：行的字段是否与表声明对得上由
        // `schema_anchor` 那条离线门禁严格把关（它按接收者判定归属），这里只验
        // 「校验阶段选对了什么」。
        assert_eq!(plan.applicant_field_id, "fldP", "存 id 不存列名");
        assert_eq!(plan.backfill_field_id, "fldB");
        assert_eq!(plan.applicant_field_name, "申请人");
        assert_eq!(plan.backfill_field_name, "审批编号");
        assert_eq!(plan.approval_name, "往来付款类型");
        // 快照存的是**审批定义的原文**（本用例传 "[]"），不是 `FormWidget` 序列化
        // 回去的结果——后者只保留本服务要读的字段，会丢掉 `printable` /
        // `display_condition`，而快照的价值正在于「定义下次改了能不能看出来」。
        assert_eq!(plan.form_snapshot, "[]");
        assert_eq!(plan.widgets.len(), 1, "只匹配到一个控件");
    }

    #[test]
    fn missing_key_columns_are_all_reported_at_once() {
        let error = expect_invalid(plan(
            &input("没有的列", "也没有的列"),
            "t",
            &[],
            &[column("fldX", "SWIFT Address", &[])],
            &BTreeMap::new(),
            "[]",
        ));
        let ProvisionError::Invalid(reasons) = error else {
            panic!("应是 Invalid，实际 {error}");
        };
        assert_eq!(reasons.len(), 2, "两个坐标都要报出来: {reasons:?}");
        assert!(reasons[0].contains("申请人"), "{}", reasons[0]);
        assert!(reasons[1].contains("回填列"), "{}", reasons[1]);
    }

    #[test]
    fn same_column_for_applicant_and_backfill_is_rejected() {
        let error = expect_invalid(plan(
            &input("fldP", "fldP"),
            "t",
            &[],
            &[column("fldP", "申请人", &[])],
            &BTreeMap::new(),
            "[]",
        ));
        assert!(error.to_string().contains("不能是同一列"), "{error}");
    }

    #[test]
    fn matched_widget_with_linkage_gets_options_from_the_binding() {
        // 链接态控件：`match_by_name` 不报「派生不出」，选项由本系统补。
        let linked = FormWidget {
            id: "w1".to_string(),
            name: "公司名称/Company name".to_string(),
            r#type: "radioV2".to_string(),
            required: true,
            option: Some(WidgetOptions::Fixed(vec![])),
            external_data: Some(serde_json::json!({
                "externalDataLinkage": true, "key": ""
            })),
            value: serde_json::Value::Null,
            children: Vec::new(),
        };
        let mut external = BTreeMap::new();
        external.insert("w1".to_string(), "fldq7lcb6y".to_string());
        let plan = plan(
            &input("fldP", "fldB"),
            "t",
            &[linked],
            &[
                column("fldP", "申请人", &[]),
                column("fldB", "审批编号", &[]),
                column("fldQ", "公司名称/Company name", &[]),
            ],
            &external,
            "[]",
        )
        .unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(plan.widgets.len(), 1);
        assert_eq!(
            plan.widgets[0].option_source_key.as_deref(),
            Some("fldq7lcb6y"),
            "链接态控件应保存稳定来源"
        );
    }

    #[test]
    fn detail_prefixed_column_uses_child_name_for_external_binding() {
        let form = parse_form(&serde_json::json!([{"id":"parent","name":"明细","type":"fieldList","children":[{"id":"child","name":"类别","type":"radioV2","required":true,"option":[],"externalData":{"externalDataLinkage":false,"externalUrl":"https://example.test/api/v1/feishu/approval/options/source"}}]}])).unwrap_or_else(|e| panic!("{e}"));
        let external = BTreeMap::from([("child".to_string(), "source".to_string())]);
        let result = plan(
            &input("applicant", "backfill"),
            "test",
            &form,
            &[
                column("applicant", "申请人", &[]),
                column("backfill", "编号", &[]),
                column("field", "明细_类别", &[]),
            ],
            &external,
            "[]",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(result.widgets[0].bitable_field, "field");
        assert_eq!(
            result.widgets[0].option_source_key.as_deref(),
            Some("source")
        );
    }

    #[test]
    fn linked_widget_without_binding_is_reported() {
        let linked = FormWidget {
            id: "w1".to_string(),
            name: "公司名称/Company name".to_string(),
            r#type: "radioV2".to_string(),
            required: true,
            option: Some(WidgetOptions::Fixed(vec![])),
            external_data: Some(serde_json::json!({ "externalDataLinkage": true })),
            value: serde_json::Value::Null,
            children: Vec::new(),
        };
        let error = expect_invalid(plan(
            &input("fldP", "fldB"),
            "t",
            &[linked],
            &[
                column("fldP", "申请人", &[]),
                column("fldB", "审批编号", &[]),
                // 该控件必须**有同名列**，否则会在链接态校验之前被
                // 「必填缺列」先拦掉——那是另一条路径，测不到本用例的目的。
                column("fldQ", "公司名称/Company name", &[]),
            ],
            &BTreeMap::new(),
            "[]",
        ));
        assert!(error.to_string().contains("没有该列的绑定"), "{error}");
    }

    #[test]
    fn no_common_name_is_rejected() {
        let error = expect_invalid(plan(
            &input("fldP", "fldB"),
            "t",
            &[FormWidget {
                id: "w1".to_string(),
                name: "完全不同的列".to_string(),
                r#type: "input".to_string(),
                required: false,
                option: None,
                external_data: None,
                value: serde_json::Value::Null,
                children: Vec::new(),
            }],
            &[
                column("fldP", "申请人", &[]),
                column("fldB", "审批编号", &[]),
            ],
            &BTreeMap::new(),
            "[]",
        ));
        assert!(error.to_string().contains("没有任何一条同名"), "{error}");
    }

    #[test]
    fn title_is_truncated_to_the_column_limit() {
        let long: String = "长".repeat(300);
        let title = truncate_title(&long, "tblX");
        assert!(title.chars().count() <= 100, "{}", title.chars().count());
        // 定义名为空时退到 table_id，否则必填列会拿到空串。
        assert_eq!(truncate_title("", "tblX"), "tblX");
    }

    fn json_str(value: &str) -> Value {
        Value::String(value.to_string())
    }

    #[test]
    fn a_binding_row_with_a_blank_name_is_skipped_not_fatal() {
        // 装配扫的是**全表**的启用绑定，所以任何一条名字为空的绑定
        // 都会把整批装配打挂——包括与本数据源无关的那些。
        // 空名/缺名应当**跳过这一条**（它本来也无从匹配），而不是让整个
        // 建配置流程失败。
        use yang_base::table::Record;
        let mut row = Record::new();
        row.insert("field_name", serde_json::json!(null));
        row.insert("source_key", serde_json::json!("orphan"));
        row.insert("enabled", serde_json::json!(true));
        assert_eq!(
            binding_display_name(&row).unwrap_or_else(|error| panic!("{error}")),
            None,
            "名字为空或缺失的绑定应被跳过"
        );
    }

    #[test]
    fn a_binding_row_with_a_blank_string_name_is_also_skipped() {
        // 上一条走的是 `null`（列可空、xlsx 绑定漏写时的样子），它只穿到
        // `optional` 那一层就返回了；**空串与纯空白**是另一条路径——要穿过
        // trim 之后的那道判定。两条分支会被不同的改动打穿，得分开钉住：
        // 实测把 `is_empty` 那道判定删掉，上一条照样绿，只有这一条会红。
        for value in ["", "   ", "\t"] {
            let mut row = Record::new();
            row.insert("field_name", serde_json::json!(value));
            row.insert("source_key", serde_json::json!("orphan"));
            assert_eq!(
                binding_display_name(&row).unwrap_or_else(|error| panic!("{error}")),
                None,
                "空串/纯空白（{value:?}）也要跳过——拿空名去按名匹配等于没名字"
            );
        }

        // 键整个缺失（老行没这一列）同理。
        let row = Record::new();
        assert_eq!(
            binding_display_name(&row).unwrap_or_else(|error| panic!("{error}")),
            None,
            "没有 field_name 这一列时也要跳过"
        );
    }

    #[test]
    fn backfill_targets_accept_matching_types() {
        let fields = [
            field("fldStatus", "申请状态", 1),
            field("fldFlow", "审批流程", 1),
            field("fldStart", "发起时间", 5),
            field("fldEnd", "完成时间", 5),
            field("fldDept", "发起人部门", 1),
            field("fldHandler", "当前处理人", 11),
        ];
        let columns = columns_from_fields(&fields);
        let reasons = check_backfill_targets(
            &columns,
            &fields,
            Some("fldStatus"),
            Some("fldFlow"),
            Some("fldStart"),
            Some("fldEnd"),
            Some("fldDept"),
            Some("fldHandler"),
        );
        assert!(reasons.is_empty(), "{reasons:?}");
    }

    #[test]
    fn status_accepts_single_select_but_rejects_other_types() {
        let fields = [field("fldSel", "状态", 3), field("fldNum", "状态数", 2)];
        let columns = columns_from_fields(&fields);
        // 单选(3) 是允许的（设计 §5.1：申请状态 = 文本(1) 或 单选(3)）。
        assert!(check_backfill_targets(
            &columns,
            &fields,
            Some("fldSel"),
            None,
            None,
            None,
            None,
            None
        )
        .is_empty());
        // 数字(2) 不是。
        let reasons = check_backfill_targets(
            &columns,
            &fields,
            Some("fldNum"),
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(reasons.len(), 1, "{reasons:?}");
        assert!(
            reasons[0].contains("申请状态") && reasons[0].contains("文本或单选"),
            "{}",
            reasons[0]
        );
    }

    #[test]
    fn flow_accepts_single_select_as_well_as_text() {
        // 真机表格里「审批流程」建成了单选（设计 §11 生产快照），写形态与申请状态同路
        // （`write_many` 的 `Some(3)` 臂写裸字符串）。
        let fields = [
            field("fldFlow", "审批流程", 3),
            field("fldText", "流程文本", 1),
        ];
        let columns = columns_from_fields(&fields);
        for given in ["fldFlow", "fldText"] {
            assert!(
                check_backfill_targets(
                    &columns,
                    &fields,
                    None,
                    Some(given),
                    None,
                    None,
                    None,
                    None
                )
                .is_empty(),
                "{given}"
            );
        }
    }

    #[test]
    fn every_backfill_mismatch_is_reported_at_once() {
        // 六列全错（或不存在）→ 六条原因一次报全，不遇到第一条就停。
        let fields = [field("fldNum", "数字列", 2), field("fldMulti", "多选列", 4)];
        let columns = columns_from_fields(&fields);
        let reasons = check_backfill_targets(
            &columns,
            &fields,
            Some("fldNum"),   // 申请状态：要 1/3，给了 2
            Some("没有的列"), // 审批流程：不存在
            Some("fldMulti"), // 发起时间：要 5，给了 4
            Some("也没有"),   // 完成时间：不存在
            Some("fldNum"),   // 发起人部门：要 1，给了 2
            Some("fldMulti"), // 当前处理人：要 11，给了 4
        );
        assert_eq!(reasons.len(), 6, "六列都要报出来: {reasons:?}");
        assert!(reasons[0].contains("申请状态"), "{}", reasons[0]);
        assert!(
            reasons[1].contains("审批流程") && reasons[1].contains("没有对应列"),
            "{}",
            reasons[1]
        );
        assert!(
            reasons[5].contains("当前处理人") && reasons[5].contains("人员"),
            "{}",
            reasons[5]
        );
    }

    #[test]
    fn absent_or_blank_backfill_targets_are_skipped() {
        let fields = [field("fldNum", "数字列", 2)];
        let columns = columns_from_fields(&fields);
        // 全 `None`，或给了空串/纯空白：一条都不查，不报错。
        assert!(check_backfill_targets(
            &columns,
            &fields,
            None,
            None,
            Some(""),
            Some("  "),
            None,
            None
        )
        .is_empty());
    }

    #[test]
    fn plan_resolves_backfill_targets_to_field_ids() {
        let plan = plan(
            &ProvisionInput {
                // 给列名，要解析成 id；给 id，直接用。
                status_field: Some("申请状态"),
                handler_field: Some("fldHandler"),
                ..input("fldP", "fldB")
            },
            "t",
            &[FormWidget {
                id: "w1".to_string(),
                name: "SWIFT Address".to_string(),
                r#type: "input".to_string(),
                required: false,
                option: None,
                external_data: None,
                value: serde_json::Value::Null,
                children: Vec::new(),
            }],
            &[
                column("fldP", "申请人", &[]),
                column("fldB", "审批编号", &[]),
                column("fldStatus", "申请状态", &[]),
                column("fldHandler", "当前处理人", &[]),
                column("fldC", "SWIFT Address", &[]),
            ],
            &BTreeMap::new(),
            "[]",
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            plan.status_field_id.as_deref(),
            Some("fldStatus"),
            "存 id 不存列名"
        );
        assert_eq!(plan.handler_field_id.as_deref(), Some("fldHandler"));
        assert_eq!(plan.flow_field_id, None, "未给的列落 None");
    }
}

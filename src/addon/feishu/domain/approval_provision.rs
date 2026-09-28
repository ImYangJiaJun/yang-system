//! 自动建配置（默认链路）：坐标 + 审批定义 + 多维表格列 → config 行 + field_map 行。
//!
//! # 为什么要有这一层
//!
//! 使用方的多维表格列名与审批控件名**严格对应**，因此不该要求人工逐条配
//! `feishu_approval_field_map`。派发端点在**配置不存在**时走这条路径：取定义、
//! 取列、按名匹配、校验，全过才落库。
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
    columns_from_fields, match_by_name, resolve_field_coord, Column, FormWidget, MatchOutcome,
};
use super::bitable::{list_all_fields, BitableCoordinates};
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
    pub(crate) base_token: String,
    pub(crate) table_id: String,
    pub(crate) approval_code: String,
    pub(crate) applicant_field_id: String,
    pub(crate) backfill_field_id: String,
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

    // 链接本系统外部数据源的控件：先解析它们的 `source_key`，再把选项拉出来——
    // 这一步必须在匹配**之前**，因为匹配本身不碰库。
    let external_options = load_external_options(context, &form).await?;

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

/// 纯校验 + 组装。全部失败原因一次性返回。
fn plan(
    input: &ProvisionInput<'_>,
    approval_name: &str,
    form: &[FormWidget],
    columns: &[Column],
    external_options: &BTreeMap<String, BTreeMap<String, String>>,
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

    // ---- 按名匹配 ----
    let mut widgets = match match_by_name(form, columns) {
        MatchOutcome::Matched(widgets) => widgets,
        MatchOutcome::Invalid(errors) => {
            return Err(ProvisionError::Invalid(
                errors.into_iter().map(|error| error.to_string()).collect(),
            ))
        }
    };
    if widgets.is_empty() {
        return Err(ProvisionError::Invalid(vec![
            "审批定义的控件与多维表格的列没有任何一条同名".to_string(),
        ]));
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
        let column = match columns
            .iter()
            .find(|column| column.field_id == widget.bitable_field)
        {
            Some(column) => column,
            None => continue,
        };
        match external_options.get(column.field_name.as_str()) {
            Some(map) => widget.option_map = map.clone(),
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
        base_token: input.base_token.trim().to_string(),
        table_id: input.table_id.trim().to_string(),
        approval_code: input.approval_code.trim().to_string(),
        // **存 id**：改列名不会让配置失效。
        applicant_field_id: applicant.field_id.clone(),
        backfill_field_id: backfill.field_id.clone(),
        base_timezone: input.base_timezone.trim().to_string(),
        approval_name: approval_name.trim().to_string(),
        form_snapshot: form_snapshot.to_string(),
        columns: columns.to_vec(),
        widgets,
    })
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
    config_row.insert("base_token", serde_json::json!(&plan.base_token));
    config_row.insert("table_id", serde_json::json!(&plan.table_id));
    config_row.insert("approval_code", serde_json::json!(&plan.approval_code));
    // **存 id**：改列名不会让配置失效。
    config_row.insert(
        "applicant_field",
        serde_json::json!(&plan.applicant_field_id),
    );
    config_row.insert("backfill_field", serde_json::json!(&plan.backfill_field_id));
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

    Ok(config_id as i64)
}

// ---------------------------------------------------------------------------
// 外部选项（链接态控件的 option_map 来源）
// ---------------------------------------------------------------------------

/// 解析链接到本系统外部数据源的控件 → 每列的 `label → option_id`。
///
/// # 为什么按**列名**查而不是按 `field_id`
///
/// 外部选项的绑定登记在**台账表**（`feishu_datasource_field`），而派发用的是**另一张
/// 表**——两边的 `field_id` 毫无关系，只有**列名**是共同语言（使用方正是按名严格对应
/// 才走到这一层）。所以拿派发侧的 `field_id` 去查绑定必然查空。
async fn load_external_options(
    context: &FeishuContext,
    form: &[FormWidget],
) -> Result<BTreeMap<String, BTreeMap<String, String>>, ProvisionError> {
    let linked: Vec<String> = form
        .iter()
        .flat_map(|widget| widget.walk())
        .filter(|widget| widget.links_to_our_options())
        .map(|widget| widget.name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect();
    if linked.is_empty() {
        return Ok(BTreeMap::new());
    }

    // `feishu_datasource_field.field_name` 是**缓存列**、不是 `filterable`，
    // 所以这里不能把 WHERE 下推到库（会吃 FieldPermissionDenied）——扫全表后在内存里
    // 按名配对。这张表是「字段绑定」，行数量级在百级（每数据源几列）。
    let bindings = all_pages(
        context.datasource_fields().query().select_fields(&[
            "field_name",
            "source_key",
            "enabled",
        ])?,
        MAX_BINDING_PAGES,
    )
    .await
    .map_err(|error| ProvisionError::Store(error.to_string()))?;

    // 名字 → source_key。**重名即歧义**：两张表都有「公司名称/Company name」时，
    // 取哪一个都是猜——猜错的后果是把选项映射建到别的数据源上，而症状是提交时
    // 「选项没有配置映射」或更糟：映射成功但 id 对不上。
    // 收集到 `String` 而不是借用绑定行：`binding` 是每轮循环里的临时 `Record`，
    // 借用它的字段会让歧义列表的存活短于自身（E0597）。
    let mut name_to_source: BTreeMap<String, String> = BTreeMap::new();
    let mut ambiguous: Vec<String> = Vec::new();
    for binding in &bindings {
        let enabled: bool = binding.optional("enabled")?.unwrap_or(false);
        if !enabled {
            continue;
        }
        // 名字为空/缺失：这条绑定无从参与按名匹配，跳过——但**必须留痕**（见下）。
        let Some(name) = binding_display_name(binding)? else {
            // 「跳过」是兜底，不是常态：走到这里说明上游有绑定漏写了名字
            // （`create_datasource_table` / `update_datasource_table` 都该写上），
            // 那是个要修的 bug。静默 `continue` 会让它永远不被发现，而它的后果
            // （某个控件少一个候选列）又很难归因——所以这里明确 warn 并带上
            // 能定位的 `source_key`。
            tracing::warn!(
                source_key = %binding.optional::<String>("source_key")?.unwrap_or_default(),
                "字段绑定的 field_name 为空，跳过它——列名是身份，没有名字就无法按名匹配"
            );
            continue;
        };
        let source: String = binding.require("source_key")?;
        match name_to_source.get(&name) {
            Some(existing) if *existing != source && !ambiguous.contains(&name) => {
                ambiguous.push(name.clone());
            }
            Some(_) => {}
            None => {
                name_to_source.insert(name, source);
            }
        }
    }
    if !ambiguous.is_empty() {
        return Err(ProvisionError::Invalid(vec![format!(
            "以下控件的列名在多个外部选项数据源里都出现，无法判定用哪一个：{}",
            ambiguous.join("、")
        )]));
    }

    let mut out = BTreeMap::new();
    let mut missing: Vec<&str> = Vec::new();
    for name in &linked {
        let Some(source) = name_to_source.get(name) else {
            missing.push(name.as_str());
            continue;
        };
        out.insert(name.clone(), load_options(context, source).await?);
    }
    if !missing.is_empty() {
        return Err(ProvisionError::Invalid(vec![format!(
            "以下控件链接了外部选项数据源，但本系统没有该列的绑定：{}",
            missing.join("、")
        )]));
    }
    Ok(out)
}

/// 取一条绑定的展示名。**缺失或空白一律返回 `None`（跳过），不报错。**
///
/// 为什么不能 `require`：这个函数扫的是**全表**的启用绑定，任何一条名字为空的
/// 绑定（历史数据、或建源时漏写了 `field_name` 的 xlsx 绑定）都会让
/// 「首次派发自动建配置」整批失败。跳过它只是少一个候选列，
/// 而整批失败是功能不可用——两者代价差一个量级。
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

/// 某个 `source_key` 下**启用中**的 `label → option_id`。
///
/// # label 在一个 source 内必须唯一
///
/// `option_id = hash(source_key, parent_key, label)`（级联时把父键哈了进去），
/// 所以**同一个文案挂在不同父下会有不同 id**。此时静态映射就是错的——一个
/// 文案对应两个 id，随便取一个会让提交的 id 属于另一个父。
///
/// 实测里 9 个数据源有 8 个 source 存在同名 label，所以这条**不是理论风险**。
/// 撞上就报错，让使用方改文案或改配手动映射。
async fn load_options(
    context: &FeishuContext,
    source_key: &str,
) -> Result<BTreeMap<String, String>, ProvisionError> {
    let rows = all_pages(
        context
            .options()
            .query()
            .select_fields(&["option_id", "label"])?
            .where_eq("source_key", serde_json::json!(source_key))?
            .where_eq("enabled", serde_json::json!(true))?,
        MAX_OPTION_PAGES,
    )
    .await
    .map_err(|error| ProvisionError::Store(error.to_string()))?;

    if rows.is_empty() {
        return Err(ProvisionError::Invalid(vec![format!(
            "外部选项数据源「{source_key}」里没有启用中的选项"
        )]));
    }

    let mut map: BTreeMap<String, String> = BTreeMap::new();
    let mut conflicts: Vec<String> = Vec::new();
    for row in &rows {
        let label: String = row.require("label")?;
        let option_id: String = row.require("option_id")?;
        let label = label.trim().to_string();
        match map.get(&label) {
            Some(existing) if *existing != option_id => {
                if !conflicts.contains(&label) {
                    conflicts.push(label);
                }
            }
            Some(_) => {}
            None => {
                map.insert(label, option_id);
            }
        }
    }
    if !conflicts.is_empty() {
        return Err(ProvisionError::Invalid(vec![format!(
            "数据源「{source_key}」里同一个文案对应多个选项 id（级联时常见）：{}",
            conflicts.join("、")
        )]));
    }
    Ok(map)
}

/// 扫字段绑定表时的页数上界。绑定行数量级在百级（每数据源几列），给足余量。
const MAX_BINDING_PAGES: usize = 20;

/// 读单个数据源选项时的页数上界。实测最大的一个源有 226 条选项（3 页），
/// 给到 50 页是留量而不是预期。
const MAX_OPTION_PAGES: usize = 50;

/// 标题截断到列上限（100），避免插入期才报「字符串超长」。
fn truncate_title(approval_name: &str, table_id: &str) -> String {
    let source = approval_name.trim();
    let source = if source.is_empty() {
        table_id.trim()
    } else {
        source
    };
    source.chars().take(100).collect()
}

fn now_unix_secs() -> i64 {
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

    /// 取出失败值。不用 `unwrap_err()`：clippy 的 `unwrap_used` 是 deny，
    /// 且它把「取错误值」的变体一并归入——`#[cfg(test)]` 也不例外。
    fn expect_invalid<T>(result: Result<T, ProvisionError>) -> ProvisionError {
        match result {
            Ok(_) => panic!("期望校验失败，实际通过了"),
            Err(error) => error,
        }
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
        }
    }

    /// 真实形态的最小复刻：**`form` 是 JSON 字符串**。
    const FORM_STRING: &str = r#"[{"id":"w1","name":"SWIFT Address","type":"input","required":true},
        {"id":"w2","name":"收款方类型/Type of payee","type":"radioV2","required":true,
         "option":[{"value":"mpuvnw0h-1","text":"个人"}]}]"#;

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
            children: Vec::new(),
        };
        let mut external = BTreeMap::new();
        external.insert(
            "公司名称/Company name".to_string(),
            [("华为".to_string(), "fldq7lcb6y:abc".to_string())]
                .into_iter()
                .collect::<BTreeMap<String, String>>(),
        );
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
            plan.widgets[0].option_map.get("华为").map(String::as_str),
            Some("fldq7lcb6y:abc"),
            "链接态控件的选项要从绑定补进来"
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
}

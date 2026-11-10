//! 回写规则的保存期校验与库内读取。
//!
//! # 为什么单独成域
//!
//! 建规则、改规则、批量体检三条路径必须给**同一套保证**：定义里的控件存在且类型
//! 与转换器相容、明细映射确实来自所选明细控件、条件引用的控件存在、目标列可写、
//! 状态列类型可写。任何一条只在其中一条路径上实现，都会让「检查说没问题、保存却
//! 写出坏配置」成为可能。
//!
//! 本模块只做校验与解析，不落库；`Record` 一律在 Action 里就地构造，满足离线
//! 对账门禁 `schema_anchor` 的归属判定（见 `approval_provision` 的同类说明）。
//!
//! # 日志纪律
//!
//! 校验错误文案只包含控件 id / 列名 / 规则标题，不包含任何审批表单值。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use yang_base::table::Record;
use yang_base::BaseError;

use sha2::{Digest, Sha256};

use super::super::approval::get_approval_definition;
use super::super::approval_convert::labels_equivalent;
use super::super::approval_match::FormWidget;
use super::super::approval_provision::parse_form;
use super::super::bitable::{list_all_fields, FieldItem};
use super::super::context::FeishuContext;
use super::super::outbound::{OutboundTransport, Sleeper};
use super::super::repository::all_pages;
use super::super::tenant_token::TenantTokenProvider;
use super::condition::{CompareOp, ConditionItem};
use super::mapping::KNOWN_SYSTEM_KEYS;
use super::{
    Condition, Converter, ExportError, ExportFieldMap, ExportRule, RowFilter, SourceKind,
    StatusLabels, TriggerMode,
};

/// 保存期校验的失败分类。
#[derive(Debug)]
pub(crate) enum ExportConfigError {
    /// 配置本身不合法（全部原因，逐条可行动）。
    Invalid(Vec<String>),
    /// 飞书侧取数失败（网络 / 权限 / 定义不存在）。
    Fetch(String),
}

impl std::fmt::Display for ExportConfigError {
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
        }
    }
}

impl std::error::Error for ExportConfigError {}

/// 待校验的回写规则（Action 入参的领域形态）。
#[derive(Debug, Clone, Default)]
pub(crate) struct ExportRuleDraft {
    pub(crate) title: String,
    pub(crate) approval_code: String,
    pub(crate) trigger_mode: String,
    pub(crate) trigger_node_ids: Vec<String>,
    pub(crate) require_final_approval: bool,
    pub(crate) detail_widget_id: String,
    pub(crate) row_filter: String,
    pub(crate) condition_json: Option<String>,
    pub(crate) target_base_token: String,
    pub(crate) target_table_id: String,
    pub(crate) status_field: Option<String>,
    pub(crate) status_labels_json: Option<String>,
    pub(crate) mark_enabled: bool,
    pub(crate) enabled: bool,
    pub(crate) maps: Vec<ExportFieldMapDraft>,
}

/// 待校验的一条字段映射。
#[derive(Debug, Clone, Default)]
pub(crate) struct ExportFieldMapDraft {
    pub(crate) source_kind: String,
    pub(crate) source_widget_id: String,
    pub(crate) source_system_key: String,
    pub(crate) constant_value: Option<String>,
    pub(crate) target_field_id: String,
    pub(crate) converter: String,
    pub(crate) option_map_json: Option<String>,
    pub(crate) required: bool,
}

/// 校验通过的一条字段映射（目标列名已按当前表解析）。
#[derive(Debug, Clone)]
pub(crate) struct ValidatedExportMap {
    pub(crate) source_kind: SourceKind,
    pub(crate) source_widget_id: String,
    pub(crate) source_system_key: String,
    pub(crate) constant_value: Option<String>,
    pub(crate) target_field_id: String,
    pub(crate) target_field_name: String,
    pub(crate) converter: Converter,
    pub(crate) option_map: BTreeMap<String, String>,
    pub(crate) required: bool,
}

/// 校验通过的回写规则。字段与 `feishu_approval_export_rule` 一一对应。
#[derive(Debug, Clone)]
pub(crate) struct ValidatedExportRule {
    pub(crate) title: String,
    pub(crate) approval_code: String,
    pub(crate) approval_name: String,
    pub(crate) trigger_mode: TriggerMode,
    pub(crate) trigger_node_ids: Vec<String>,
    pub(crate) require_final_approval: bool,
    pub(crate) detail_widget_id: String,
    pub(crate) row_filter: RowFilter,
    /// 规范化后的条件原文；空串表示无条件。
    pub(crate) condition_json: String,
    pub(crate) target_base_token: String,
    pub(crate) target_table_id: String,
    pub(crate) status_field: Option<String>,
    pub(crate) status_field_name: Option<String>,
    pub(crate) status_labels_json: Option<String>,
    pub(crate) mark_enabled: bool,
    pub(crate) enabled: bool,
    pub(crate) form_snapshot: String,
    pub(crate) node_snapshot: String,
    pub(crate) definition_hash: String,
    pub(crate) maps: Vec<ValidatedExportMap>,
}

struct LocalRule {
    title: String,
    approval_code: String,
    trigger_mode: TriggerMode,
    trigger_node_ids: Vec<String>,
    require_final_approval: bool,
    detail_widget_id: String,
    row_filter: RowFilter,
    condition: Condition,
    condition_json: String,
    target_base_token: String,
    target_table_id: String,
    status_field: Option<String>,
    status_labels: StatusLabels,
    status_labels_json: Option<String>,
    mark_enabled: bool,
    enabled: bool,
    maps: Vec<LocalMap>,
}

struct LocalMap {
    source_kind: SourceKind,
    source_widget_id: String,
    source_system_key: String,
    constant_value: Option<String>,
    target_field_id: String,
    converter: Converter,
    option_map: BTreeMap<String, String>,
    required: bool,
}

/// 校验一条待保存规则。
///
/// 先做本地语法/坐标校验，再依次取审批定义与目标列；**所有原因一次性返回**，
/// 避免「修一个跑一轮」。
pub(crate) async fn validate_export_rule(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    draft: &ExportRuleDraft,
) -> Result<ValidatedExportRule, ExportConfigError> {
    let local = parse_local(draft).map_err(ExportConfigError::Invalid)?;

    let definition =
        get_approval_definition(transport, sleeper, tokens, local.approval_code.as_str())
            .await
            .map_err(|failure| ExportConfigError::Fetch(failure.message))?;

    let mut reasons: Vec<String> = Vec::new();
    if definition.is_external {
        reasons.push(
            "该 approval_code 是三方审批定义：回写只支持本系统可读取实例表单的定义".to_string(),
        );
    }
    let widgets = match definition.form.as_ref() {
        Some(form) => match parse_form(form) {
            Ok(widgets) => Some(widgets),
            Err(error) => {
                reasons.push(error.to_string());
                None
            }
        },
        None => {
            reasons.push("审批定义没有返回表单（`form` 为空），无法校验控件".to_string());
            None
        }
    };

    let fields = list_all_fields(
        transport,
        sleeper,
        tokens,
        &super::super::bitable::BitableCoordinates {
            app_token: local.target_base_token.clone(),
            table_id: local.target_table_id.clone(),
            view_id: None,
        },
    )
    .await
    .map_err(|failure| ExportConfigError::Fetch(failure.message))?;

    let mut resolved_maps: Vec<ValidatedExportMap> = Vec::new();
    // 定义不可用时也校验目标列：让用户一次看到两边的全部问题。
    let status_field_name = validate_targets(&local, &fields, &mut reasons, &mut resolved_maps);
    if let Some(widgets) = widgets.as_deref() {
        let catalog = WidgetCatalog::build(widgets, &local.detail_widget_id);
        validate_definition(&local, &catalog, &mut reasons);
    }

    if !reasons.is_empty() {
        return Err(ExportConfigError::Invalid(reasons));
    }

    let form_snapshot = form_snapshot_text(definition.form.as_ref());
    let node_snapshot = serde_json::to_string(&definition.node_list.clone().unwrap_or_default())
        .map_err(|error| {
            ExportConfigError::Invalid(vec![format!("审批节点快照序列化失败：{error}")])
        })?;
    let definition_hash = sha256_hex(&format!("{form_snapshot}\n{node_snapshot}"));

    Ok(ValidatedExportRule {
        title: local.title,
        approval_code: local.approval_code,
        approval_name: definition.approval_name.trim().to_string(),
        trigger_mode: local.trigger_mode,
        trigger_node_ids: local.trigger_node_ids,
        require_final_approval: local.require_final_approval,
        detail_widget_id: local.detail_widget_id,
        row_filter: local.row_filter,
        condition_json: local.condition_json,
        target_base_token: local.target_base_token,
        target_table_id: local.target_table_id,
        status_field: local.status_field,
        status_field_name,
        status_labels_json: local.status_labels_json,
        mark_enabled: local.mark_enabled,
        enabled: local.enabled,
        form_snapshot,
        node_snapshot,
        definition_hash,
        maps: resolved_maps,
    })
}

fn parse_local(draft: &ExportRuleDraft) -> Result<LocalRule, Vec<String>> {
    let mut reasons = Vec::new();

    let title = draft.title.trim().to_string();
    if title.is_empty() || title.chars().count() > 100 {
        reasons.push("规则名必须在 1..=100 字符".to_string());
    }
    let approval_code = draft.approval_code.trim().to_string();
    if approval_code.is_empty() || approval_code.chars().count() > 128 {
        reasons.push("审批定义 Code 必须在 1..=128 字符".to_string());
    }
    let trigger_mode = match TriggerMode::parse(&draft.trigger_mode) {
        Ok(mode) => mode,
        Err(error) => {
            reasons.push(error.to_string());
            TriggerMode::NodePassed
        }
    };
    let row_filter = match RowFilter::parse(&draft.row_filter) {
        Ok(filter) => filter,
        Err(error) => {
            reasons.push(error.to_string());
            RowFilter::MatchedRows
        }
    };
    let detail_widget_id = draft.detail_widget_id.trim().to_string();
    if detail_widget_id.is_empty() {
        reasons.push("明细控件 ID 不能为空".to_string());
    }

    let mut trigger_node_ids: Vec<String> = Vec::new();
    let mut seen_nodes = BTreeSet::new();
    for node in &draft.trigger_node_ids {
        let node = node.trim();
        if node.is_empty() || !seen_nodes.insert(node.to_string()) {
            continue;
        }
        trigger_node_ids.push(node.to_string());
    }
    if trigger_mode == TriggerMode::NodePassed && trigger_node_ids.is_empty() {
        reasons.push("触发模式为「指定节点通过」时必须选择至少一个触发节点".to_string());
    }

    let condition_json = draft
        .condition_json
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let condition = match Condition::parse(&condition_json) {
        Ok(condition) => condition,
        Err(error) => {
            reasons.push(error.to_string());
            Condition::default()
        }
    };

    // 目标坐标允许「完整多维表格链接」：用户直接粘贴
    // `https://…/base/<token>?table=<table_id>` 时，Base Token 与数据表 ID
    // 都从链接里取；同时保留旧的裸 token + 单独 table_id 输入。两种来源都给出
    // 且指向不同坐标时 `resolve_bitable_link` 会拒绝，避免读写错表。
    let (base_token, table_id) = match super::super::bitable::resolve_bitable_link(
        draft.target_base_token.as_str(),
        Some(draft.target_table_id.as_str()),
        None,
    ) {
        Ok(parsed) => (parsed.app_token, parsed.table_id.unwrap_or_default()),
        Err(error) => {
            reasons.push(format!("目标多维表格链接非法：{error}"));
            (
                draft.target_base_token.trim().to_string(),
                draft.target_table_id.trim().to_string(),
            )
        }
    };
    if table_id.is_empty() {
        reasons.push(
            "目标数据表 ID 缺失：请选择数据表，或粘贴包含 ?table= 的完整多维表格链接".to_string(),
        );
    }

    let status_field = draft
        .status_field
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if draft.mark_enabled && status_field.is_none() {
        reasons.push("启用状态标记时必须配置目标状态列".to_string());
    }

    let status_labels = match StatusLabels::parse(draft.status_labels_json.as_deref()) {
        Ok(labels) => labels,
        Err(error) => {
            reasons.push(error.to_string());
            StatusLabels::default()
        }
    };
    let status_labels_json = draft
        .status_labels_json
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    if draft.maps.is_empty() {
        reasons.push("至少需要一条字段映射".to_string());
    }
    let mut maps = Vec::with_capacity(draft.maps.len());
    for (index, map) in draft.maps.iter().enumerate() {
        match parse_local_map(index, map) {
            Ok(map) => maps.push(map),
            Err(mut more) => reasons.append(&mut more),
        }
    }

    let mut targets = BTreeSet::new();
    for map in &maps {
        if !targets.insert(map.target_field_id.clone()) {
            reasons.push(format!(
                "多条映射指向同一目标列「{}」：字段会互相覆盖",
                map.target_field_id
            ));
        }
    }
    let mut unique = BTreeSet::new();
    for map in &maps {
        let key = format!(
            "{}|{}|{}|{}",
            map.source_kind_key(),
            map.source_widget_id,
            map.source_system_key,
            map.target_field_id
        );
        if !unique.insert(key) {
            reasons.push("存在重复的字段映射（来源与目标完全相同）".to_string());
            break;
        }
    }

    if !reasons.is_empty() {
        return Err(reasons);
    }

    Ok(LocalRule {
        title,
        approval_code,
        trigger_mode,
        trigger_node_ids,
        require_final_approval: draft.require_final_approval,
        detail_widget_id,
        row_filter,
        condition,
        condition_json,
        target_base_token: base_token,
        target_table_id: table_id,
        status_field,
        status_labels,
        status_labels_json,
        mark_enabled: draft.mark_enabled,
        enabled: draft.enabled,
        maps,
    })
}

impl LocalMap {
    fn source_kind_key(&self) -> &'static str {
        match self.source_kind {
            SourceKind::Detail => "detail",
            SourceKind::Top => "top",
            SourceKind::System => "system",
            SourceKind::Constant => "constant",
        }
    }
}

fn parse_local_map(index: usize, input: &ExportFieldMapDraft) -> Result<LocalMap, Vec<String>> {
    let label = format!("映射 #{}", index + 1);
    let mut reasons = Vec::new();

    let source_kind = match SourceKind::parse(&input.source_kind) {
        Ok(kind) => Some(kind),
        Err(error) => {
            reasons.push(format!("{label}：{error}"));
            None
        }
    };
    let converter = match Converter::parse(&input.converter) {
        Ok(converter) => Some(converter),
        Err(error) => {
            reasons.push(format!("{label}：{error}"));
            None
        }
    };
    let target_field_id = input.target_field_id.trim().to_string();
    if target_field_id.is_empty() {
        reasons.push(format!("{label}：目标列 ID 不能为空"));
    }
    let option_map_json = input
        .option_map_json
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let option_map = match super::mapping::parse_option_map(option_map_json) {
        Ok(map) => map,
        Err(error) => {
            reasons.push(format!("{label}：{error}"));
            BTreeMap::new()
        }
    };

    let source_widget_id = input.source_widget_id.trim().to_string();
    let source_system_key = input.source_system_key.trim().to_string();
    let constant_value = input
        .constant_value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    match source_kind {
        Some(SourceKind::Detail) | Some(SourceKind::Top) => {
            if source_widget_id.is_empty() {
                reasons.push(format!("{label}：控件来源必须填写控件 ID"));
            }
            if !source_system_key.is_empty() {
                reasons.push(format!("{label}：控件来源不应填写系统字段键"));
            }
        }
        Some(SourceKind::System) => {
            if source_system_key.is_empty() {
                reasons.push(format!("{label}：系统字段来源必须填写字段键"));
            }
            if !source_widget_id.is_empty() {
                reasons.push(format!("{label}：系统字段来源不应填写控件 ID"));
            }
        }
        Some(SourceKind::Constant) => {
            if constant_value.is_none() {
                reasons.push(format!("{label}：常量来源必须填写常量值"));
            }
            if !source_widget_id.is_empty() || !source_system_key.is_empty() {
                reasons.push(format!("{label}：常量来源不应填写控件 ID 或系统字段键"));
            }
        }
        None => {}
    }

    let (Some(source_kind), Some(converter)) = (source_kind, converter) else {
        return Err(reasons);
    };
    if !reasons.is_empty() {
        return Err(reasons);
    }
    Ok(LocalMap {
        source_kind,
        source_widget_id,
        source_system_key,
        constant_value,
        target_field_id,
        converter,
        option_map,
        required: input.required,
    })
}

/// 一张表/一个定义里的控件目录。
struct WidgetCatalog<'a> {
    top: BTreeMap<&'a str, &'a FormWidget>,
    detail: Option<&'a FormWidget>,
}

impl<'a> WidgetCatalog<'a> {
    fn build(widgets: &'a [FormWidget], detail_widget_id: &str) -> Self {
        let mut all = BTreeMap::new();
        collect_all(widgets, &mut all);
        let mut top = BTreeMap::new();
        collect_top(widgets, &mut top);
        let detail = all
            .get(detail_widget_id)
            .copied()
            .filter(|widget| widget.r#type == "fieldList");
        Self { top, detail }
    }

    fn detail_children(&self) -> BTreeMap<&'a str, &'a FormWidget> {
        let mut children = BTreeMap::new();
        if let Some(detail) = self.detail {
            for child in &detail.children {
                children.insert(child.id.as_str(), child);
            }
        }
        children
    }
}

fn collect_all<'a>(widgets: &'a [FormWidget], out: &mut BTreeMap<&'a str, &'a FormWidget>) {
    for widget in widgets {
        out.insert(widget.id.as_str(), widget);
        collect_all(&widget.children, out);
    }
}

/// 顶层取值控件：控件组的子控件在实例表单里也是顶层节点；明细子控件不是。
fn collect_top<'a>(widgets: &'a [FormWidget], out: &mut BTreeMap<&'a str, &'a FormWidget>) {
    for widget in widgets {
        out.insert(widget.id.as_str(), widget);
        if widget.r#type != "fieldList" {
            collect_top(&widget.children, out);
        }
    }
}

fn validate_definition(local: &LocalRule, catalog: &WidgetCatalog<'_>, reasons: &mut Vec<String>) {
    let detail_children = catalog.detail_children();
    if catalog.detail.is_none() {
        reasons.push(format!(
            "审批定义里找不到明细控件「{}」（或它的类型不是 fieldList）",
            local.detail_widget_id
        ));
    } else if detail_children.is_empty() {
        reasons.push(format!(
            "明细控件「{}」没有任何子控件",
            local.detail_widget_id
        ));
    }

    for (index, map) in local.maps.iter().enumerate() {
        let label = format!("映射 #{}", index + 1);
        let widget = match map.source_kind {
            SourceKind::Detail => match detail_children.get(map.source_widget_id.as_str()) {
                Some(widget) => Some(*widget),
                None => {
                    reasons.push(format!(
                        "{label}：明细控件「{}」里找不到子控件「{}」",
                        local.detail_widget_id, map.source_widget_id
                    ));
                    None
                }
            },
            SourceKind::Top => match catalog.top.get(map.source_widget_id.as_str()) {
                Some(widget) if widget.r#type == "fieldList" => {
                    reasons.push(format!(
                        "{label}：明细控件「{}」应使用 detail 来源，而不是 top",
                        map.source_widget_id
                    ));
                    None
                }
                Some(widget) => Some(*widget),
                None => {
                    reasons.push(format!(
                        "{label}：审批定义里找不到顶层控件「{}」",
                        map.source_widget_id
                    ));
                    None
                }
            },
            SourceKind::System => {
                if !KNOWN_SYSTEM_KEYS.contains(&map.source_system_key.as_str()) {
                    reasons.push(format!(
                        "{label}：未知的系统字段键「{}」",
                        map.source_system_key
                    ));
                }
                None
            }
            SourceKind::Constant => None,
        };
        if let Some(widget) = widget {
            if !converter_widget_compatible(map.converter, &widget.r#type) {
                reasons.push(format!(
                    "{label}：转换器「{}」不适用于控件「{}」的类型 {}",
                    converter_name(map.converter),
                    widget.id,
                    widget.r#type
                ));
            }
        }
    }

    validate_condition(
        &local.condition,
        &local.detail_widget_id,
        catalog,
        &detail_children,
        reasons,
    );
}

fn validate_condition(
    condition: &Condition,
    detail_widget_id: &str,
    catalog: &WidgetCatalog<'_>,
    detail_children: &BTreeMap<&str, &FormWidget>,
    reasons: &mut Vec<String>,
) {
    for item in &condition.items {
        match item {
            ConditionItem::Instance(leaf) => match catalog.top.get(leaf.widget_id.as_str()) {
                Some(widget) => check_numeric_op(leaf.op, &widget.r#type, &leaf.widget_id, reasons),
                None => reasons.push(format!(
                    "条件引用的顶层控件「{}」在审批定义里不存在",
                    leaf.widget_id
                )),
            },
            ConditionItem::Detail(detail) => {
                if detail.detail_widget_id != detail_widget_id {
                    reasons.push(format!(
                        "行级条件必须引用本规则选择的明细控件「{detail_widget_id}」，\
                         实际为「{}」",
                        detail.detail_widget_id
                    ));
                }
                for leaf in &detail.items {
                    match detail_children.get(leaf.widget_id.as_str()) {
                        Some(widget) => {
                            check_numeric_op(leaf.op, &widget.r#type, &leaf.widget_id, reasons)
                        }
                        None => reasons.push(format!(
                            "行级条件引用的明细子控件「{}」在明细「{}」里不存在",
                            leaf.widget_id, detail.detail_widget_id
                        )),
                    }
                }
            }
        }
    }
}

fn check_numeric_op(op: CompareOp, widget_type: &str, widget_id: &str, reasons: &mut Vec<String>) {
    let numeric_op = matches!(
        op,
        CompareOp::Gt | CompareOp::Gte | CompareOp::Lt | CompareOp::Lte | CompareOp::Between
    );
    if numeric_op && !numeric_widget(widget_type) {
        reasons.push(format!(
            "条件对控件「{widget_id}」（类型 {widget_type}）使用了数值比较，\
             该控件不是数字/金额/公式/日期"
        ));
    }
}

fn numeric_widget(widget_type: &str) -> bool {
    matches!(
        widget_type,
        "number" | "amount" | "formula" | "date" | "input" | "textarea"
    )
}

fn converter_widget_compatible(converter: Converter, widget_type: &str) -> bool {
    match converter {
        Converter::Text | Converter::Json => true,
        Converter::Number => matches!(
            widget_type,
            "number" | "amount" | "formula" | "input" | "textarea"
        ),
        // `dateInterval` 的实例值是 `{start, end}`，本回写映射没有「结束日期列」
        // 这一位（派发的 `interval_end_field` 在回写里不存在），写单列只会得到
        // 半个区间，所以保存期直接拒绝，而不是运行时静默写空。
        Converter::Date => widget_type == "date",
        Converter::Person => matches!(widget_type, "contact" | "contacts" | "person"),
        Converter::OptionText | Converter::MultiOption => matches!(
            widget_type,
            "radio" | "radioV2" | "checkbox" | "checkboxV2" | "select" | "multiSelect"
        ),
        Converter::Checkbox => matches!(widget_type, "checkbox" | "checkboxV2" | "switch"),
        Converter::Url => matches!(
            widget_type,
            "attachment" | "attachmentV2" | "image" | "imageV2" | "input" | "textarea"
        ),
    }
}

fn converter_name(converter: Converter) -> &'static str {
    match converter {
        Converter::Text => "text",
        Converter::Number => "number",
        Converter::Date => "date",
        Converter::Person => "person",
        Converter::OptionText => "option_text",
        Converter::MultiOption => "multi_option",
        Converter::Checkbox => "checkbox",
        Converter::Url => "url",
        Converter::Json => "json",
    }
}

/// 目标列校验；通过时把每条映射的当前列名填进 `resolved`。
fn validate_targets(
    local: &LocalRule,
    fields: &[FieldItem],
    reasons: &mut Vec<String>,
    resolved: &mut Vec<ValidatedExportMap>,
) -> Option<String> {
    let field_of = |field_id: &str| fields.iter().find(|field| field.field_id == field_id);

    let mut status_field_name = None;
    if let Some(status_field) = local.status_field.as_deref() {
        match field_of(status_field) {
            Some(field) => {
                let field_type = field.field_type;
                if !matches!(field_type, Some(1) | Some(3)) {
                    reasons.push(format!(
                        "状态列「{}」的类型 {:?} 不支持写入文案（需要文本或单选列）",
                        field.field_name, field_type
                    ));
                }
                if field.is_primary {
                    reasons.push(format!(
                        "状态列「{}」是多维表格主字段，不能用来写状态文案；请另建文本/单选列",
                        field.field_name
                    ));
                }
                if field_type == Some(3) {
                    validate_status_options(field, &local.status_labels, reasons);
                }
                status_field_name = Some(field.field_name.trim().to_string());
            }
            None => reasons.push(format!("目标表里找不到状态列 field_id「{status_field}」")),
        }
    }

    if let (Some(status_field), Some(_)) =
        (local.status_field.as_deref(), status_field_name.as_ref())
    {
        if local
            .maps
            .iter()
            .any(|map| map.target_field_id == status_field)
        {
            reasons.push(format!(
                "状态列「{}」不能同时作为映射目标：写入与标记会互相覆盖",
                local
                    .status_field
                    .as_deref()
                    .and_then(field_of)
                    .map(|field| field.field_name.as_str())
                    .unwrap_or(status_field)
            ));
        }
    }

    for (index, map) in local.maps.iter().enumerate() {
        let label = format!("映射 #{}", index + 1);
        let Some(field) = field_of(&map.target_field_id) else {
            reasons.push(format!(
                "{label}：目标表里找不到字段 id「{}」",
                map.target_field_id
            ));
            continue;
        };
        let field_name = field.field_name.trim().to_string();
        let Some(field_type) = field.field_type else {
            reasons.push(format!(
                "{label}：目标列「{field_name}」的类型未知，拒绝写入"
            ));
            continue;
        };
        if !super::mapping::is_writable_field_type(field_type) {
            reasons.push(format!(
                "{label}：目标列「{field_name}」类型 {field_type} 不支持回写\
                 （公式/查找/关联/附件列不可写；附件请用文本或超链接）"
            ));
            continue;
        }
        if !converter_target_compatible(map.converter, field_type) {
            reasons.push(format!(
                "{label}：转换器「{}」不能写入目标列「{field_name}」的类型 {field_type}",
                converter_name(map.converter)
            ));
        }
        if matches!(
            map.converter,
            Converter::OptionText | Converter::MultiOption
        ) && matches!(field_type, 3 | 4)
        {
            let options = field.option_names();
            if !options.is_empty() {
                for mapped in map.option_map.values() {
                    if !options
                        .iter()
                        .any(|option| labels_equivalent(option, mapped))
                    {
                        reasons.push(format!(
                            "{label}：目标列「{field_name}」没有选项「{mapped}」"
                        ));
                    }
                }
            }
        }
        resolved.push(ValidatedExportMap {
            source_kind: map.source_kind,
            source_widget_id: map.source_widget_id.clone(),
            source_system_key: map.source_system_key.clone(),
            constant_value: map.constant_value.clone(),
            target_field_id: map.target_field_id.clone(),
            target_field_name: field_name,
            converter: map.converter,
            option_map: map.option_map.clone(),
            required: map.required,
        });
    }

    status_field_name
}

/// 单选状态列的每个状态文案都必须是已有选项。
///
/// 文本列随意；单选列写不存在的选项只会在运行期（标记时）才失败。选项列表为空
/// 时跳过——与映射选项校验同一口径，避免因接口未回传 `property` 而误拒。
fn validate_status_options(field: &FieldItem, labels: &StatusLabels, reasons: &mut Vec<String>) {
    let options = field.option_names();
    if options.is_empty() {
        return;
    }
    let mut missing: BTreeSet<&str> = BTreeSet::new();
    for label in [
        labels.valid.as_str(),
        labels.rolled_back.as_str(),
        labels.rejected.as_str(),
        labels.canceled.as_str(),
        labels.reverted.as_str(),
        labels.deleted.as_str(),
    ] {
        if !options
            .iter()
            .any(|option| labels_equivalent(option, label))
        {
            missing.insert(label);
        }
    }
    for label in missing {
        reasons.push(format!(
            "状态列「{}」缺少单选选项「{label}」：请先在目标表补齐，或改用文本列",
            field.field_name
        ));
    }
}

fn converter_target_compatible(converter: Converter, field_type: i32) -> bool {
    match converter {
        Converter::Text => matches!(field_type, 1 | 13),
        Converter::Number => field_type == 2,
        Converter::Date => field_type == 5,
        Converter::Person => field_type == 11,
        Converter::OptionText => field_type == 3,
        Converter::MultiOption => field_type == 4,
        Converter::Checkbox => field_type == 7,
        Converter::Url => matches!(field_type, 1 | 15),
        Converter::Json => field_type == 1,
    }
}

fn form_snapshot_text(form: Option<&Value>) -> String {
    match form {
        Some(Value::String(raw)) => raw.clone(),
        Some(value @ Value::Array(_)) => serde_json::to_string(value).unwrap_or_default(),
        _ => String::new(),
    }
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// ---------------------------------------------------------------------------
// 库内读取
// ---------------------------------------------------------------------------

/// 规则表的读取列。
pub(crate) const RULE_FIELDS: &[&str] = &[
    "id",
    "title",
    "approval_code",
    "trigger_mode",
    "trigger_node_ids",
    "require_final_approval",
    "detail_widget_id",
    "row_filter",
    "condition_json",
    "target_base_token",
    "target_table_id",
    "status_field",
    "status_field_name",
    "status_labels_json",
    "mark_enabled",
    "enabled",
    "effective_from",
];

/// 映射表的读取列。
pub(crate) const FIELD_MAP_FIELDS: &[&str] = &[
    "rule_id",
    "source_kind",
    "source_widget_id",
    "source_system_key",
    "constant_value",
    "target_field_id",
    "target_field_name",
    "converter",
    "option_map",
    "required",
];

/// 读出一条规则及其映射（预览与人工重试共用）。
pub(crate) async fn load_rule_and_maps(
    context: &FeishuContext,
    rule_id: i64,
) -> Result<Option<(ExportRule, Vec<ExportFieldMap>)>, BaseError> {
    let Some(row) = context
        .approval_export_rules()
        .query()
        .select_fields(RULE_FIELDS)?
        .where_eq("id", serde_json::json!(rule_id))?
        .optional()
        .await?
    else {
        return Ok(None);
    };
    let rule = parse_rule_record(&row)
        .map_err(|error| BaseError::ParamInvalid("rule_id".to_string(), error.to_string()))?;
    // `.all()` 默认只取一页（10 行），映射多于 10 条时预览/体检会少算字段。
    let map_rows = all_pages(
        context
            .approval_export_field_maps()
            .query()
            .select_fields(FIELD_MAP_FIELDS)?
            .where_eq("rule_id", serde_json::json!(rule_id))?,
        10,
    )
    .await?;
    let mut maps = Vec::with_capacity(map_rows.len());
    for map_row in map_rows {
        maps.push(
            parse_field_map_record(&map_row).map_err(|error| {
                BaseError::ParamInvalid("rule_id".to_string(), error.to_string())
            })?,
        );
    }
    Ok(Some((rule, maps)))
}

/// 把库内规则行解析成领域结构（与 worker 的解析口径一致）。
pub(crate) fn parse_rule_record(row: &Record) -> Result<ExportRule, ExportError> {
    let fatal = |field: &str, error: BaseError| {
        ExportError::Fatal(format!("规则 {field} 读取失败: {error}"))
    };
    let id: i64 = row.require("id").map_err(|error| fatal("id", error))?;
    let title: String = row
        .optional("title")
        .map_err(|error| fatal("title", error))?
        .unwrap_or_default();
    let approval_code: String = row
        .require("approval_code")
        .map_err(|error| fatal("approval_code", error))?;
    let trigger_mode_raw: String = row
        .optional("trigger_mode")
        .map_err(|error| fatal("trigger_mode", error))?
        .unwrap_or_else(|| "node_passed".to_string());
    let trigger_mode = TriggerMode::parse(&trigger_mode_raw)?;
    let node_ids_raw: Option<String> = row
        .optional("trigger_node_ids")
        .map_err(|error| fatal("trigger_node_ids", error))?;
    let trigger_node_ids = parse_node_ids(node_ids_raw.as_deref())?;
    let require_final_approval = row
        .optional::<bool>("require_final_approval")
        .map_err(|error| fatal("require_final_approval", error))?
        .unwrap_or(true);
    let detail_widget_id: String = row
        .require("detail_widget_id")
        .map_err(|error| fatal("detail_widget_id", error))?;
    let row_filter_raw: String = row
        .optional("row_filter")
        .map_err(|error| fatal("row_filter", error))?
        .unwrap_or_else(|| "matched_rows".to_string());
    let row_filter = RowFilter::parse(&row_filter_raw)?;
    let condition_raw: Option<String> = row
        .optional("condition_json")
        .map_err(|error| fatal("condition_json", error))?;
    let condition = Condition::parse(condition_raw.as_deref().unwrap_or(""))?;
    let target_base_token: String = row
        .require("target_base_token")
        .map_err(|error| fatal("target_base_token", error))?;
    let target_table_id: String = row
        .require("target_table_id")
        .map_err(|error| fatal("target_table_id", error))?;
    let status_field = optional_nonempty(row, "status_field");
    let status_field_name = optional_nonempty(row, "status_field_name");
    let labels_raw: Option<String> = row
        .optional("status_labels_json")
        .map_err(|error| fatal("status_labels_json", error))?;
    let status_labels = StatusLabels::parse(labels_raw.as_deref())?;
    let mark_enabled = row
        .optional::<bool>("mark_enabled")
        .map_err(|error| fatal("mark_enabled", error))?
        .unwrap_or(true);
    let enabled = row
        .optional::<bool>("enabled")
        .map_err(|error| fatal("enabled", error))?
        .unwrap_or(true);
    let effective_from = row
        .optional::<i64>("effective_from")
        .map_err(|error| fatal("effective_from", error))?;

    Ok(ExportRule {
        id,
        title,
        approval_code,
        trigger_mode,
        trigger_node_ids,
        require_final_approval,
        detail_widget_id,
        row_filter,
        condition,
        target_base_token,
        target_table_id,
        status_field,
        status_field_name,
        status_labels,
        mark_enabled,
        enabled,
        effective_from,
    })
}

pub(crate) fn parse_field_map_record(row: &Record) -> Result<ExportFieldMap, ExportError> {
    let fatal = |field: &str, error: BaseError| {
        ExportError::Fatal(format!("字段映射 {field} 读取失败: {error}"))
    };
    let source_kind_raw: String = row
        .require("source_kind")
        .map_err(|error| fatal("source_kind", error))?;
    let source_kind = SourceKind::parse(&source_kind_raw)?;
    let source_widget_id = row
        .optional::<String>("source_widget_id")
        .map_err(|error| fatal("source_widget_id", error))?
        .unwrap_or_default();
    let source_system_key = row
        .optional::<String>("source_system_key")
        .map_err(|error| fatal("source_system_key", error))?
        .unwrap_or_default();
    let constant_value = row
        .optional::<String>("constant_value")
        .map_err(|error| fatal("constant_value", error))?;
    let target_field_id: String = row
        .require("target_field_id")
        .map_err(|error| fatal("target_field_id", error))?;
    let converter_raw: String = row
        .optional("converter")
        .map_err(|error| fatal("converter", error))?
        .unwrap_or_else(|| "text".to_string());
    let converter = Converter::parse(&converter_raw)?;
    let option_map_raw = row
        .optional::<String>("option_map")
        .map_err(|error| fatal("option_map", error))?;
    let option_map = super::mapping::parse_option_map(option_map_raw.as_deref())?;
    let required = row
        .optional::<bool>("required")
        .map_err(|error| fatal("required", error))?
        .unwrap_or(false);

    Ok(ExportFieldMap {
        source_kind,
        source_widget_id,
        source_system_key,
        constant_value,
        target_field_id,
        converter,
        option_map,
        required,
    })
}

fn parse_node_ids(raw: Option<&str>) -> Result<Vec<String>, ExportError> {
    let Some(raw) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(Vec::new());
    };
    let values: Vec<String> = serde_json::from_str(raw)
        .map_err(|error| ExportError::Fatal(format!("触发节点列表不是字符串数组：{error}")))?;
    Ok(values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect())
}

fn optional_nonempty(row: &Record, field: &str) -> Option<String> {
    row.optional::<String>(field)
        .ok()
        .flatten()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 把库内规则行（+ 映射行）还原成待校验草稿，供批量体检重放。
pub(crate) fn stored_draft(row: &Record, map_rows: &[Record]) -> Result<ExportRuleDraft, String> {
    let text = |field: &str| -> Result<String, String> {
        row.optional::<String>(field)
            .map_err(|error| format!("{field}: {error}"))
            .map(Option::unwrap_or_default)
    };
    let trigger_ids = match text("trigger_node_ids")? {
        raw if raw.trim().is_empty() => Vec::new(),
        raw => serde_json::from_str::<Vec<String>>(&raw)
            .map_err(|error| format!("trigger_node_ids: {error}"))?,
    };
    let mut maps = Vec::with_capacity(map_rows.len());
    for map_row in map_rows {
        maps.push(ExportFieldMapDraft {
            source_kind: map_row
                .optional::<String>("source_kind")
                .map_err(|error| format!("source_kind: {error}"))?
                .unwrap_or_default(),
            source_widget_id: map_row
                .optional::<String>("source_widget_id")
                .map_err(|error| format!("source_widget_id: {error}"))?
                .unwrap_or_default(),
            source_system_key: map_row
                .optional::<String>("source_system_key")
                .map_err(|error| format!("source_system_key: {error}"))?
                .unwrap_or_default(),
            constant_value: map_row
                .optional::<String>("constant_value")
                .map_err(|error| format!("constant_value: {error}"))?,
            target_field_id: map_row
                .optional::<String>("target_field_id")
                .map_err(|error| format!("target_field_id: {error}"))?
                .unwrap_or_default(),
            converter: map_row
                .optional::<String>("converter")
                .map_err(|error| format!("converter: {error}"))?
                .unwrap_or_else(|| "text".to_string()),
            option_map_json: map_row
                .optional::<String>("option_map")
                .map_err(|error| format!("option_map: {error}"))?,
            required: map_row
                .optional::<bool>("required")
                .map_err(|error| format!("required: {error}"))?
                .unwrap_or(false),
        });
    }
    Ok(ExportRuleDraft {
        title: text("title")?,
        approval_code: text("approval_code")?,
        trigger_mode: text("trigger_mode")?,
        trigger_node_ids: trigger_ids,
        require_final_approval: row
            .optional::<bool>("require_final_approval")
            .map_err(|error| format!("require_final_approval: {error}"))?
            .unwrap_or(true),
        detail_widget_id: text("detail_widget_id")?,
        row_filter: text("row_filter")?,
        condition_json: row
            .optional::<String>("condition_json")
            .map_err(|error| format!("condition_json: {error}"))?,
        target_base_token: text("target_base_token")?,
        target_table_id: text("target_table_id")?,
        status_field: row
            .optional::<String>("status_field")
            .map_err(|error| format!("status_field: {error}"))?,
        status_labels_json: row
            .optional::<String>("status_labels_json")
            .map_err(|error| format!("status_labels_json: {error}"))?,
        mark_enabled: row
            .optional::<bool>("mark_enabled")
            .map_err(|error| format!("mark_enabled: {error}"))?
            .unwrap_or(true),
        enabled: row
            .optional::<bool>("enabled")
            .map_err(|error| format!("enabled: {error}"))?
            .unwrap_or(true),
        maps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft() -> ExportRuleDraft {
        ExportRuleDraft {
            title: "付款回写".to_string(),
            approval_code: "CODE-1".to_string(),
            trigger_mode: "node_passed".to_string(),
            trigger_node_ids: vec!["n1".to_string()],
            require_final_approval: true,
            detail_widget_id: "detail".to_string(),
            row_filter: "matched_rows".to_string(),
            condition_json: None,
            target_base_token: "bascnTest".to_string(),
            target_table_id: "tblTest".to_string(),
            status_field: Some("fldStatus".to_string()),
            status_labels_json: None,
            mark_enabled: true,
            enabled: true,
            maps: vec![ExportFieldMapDraft {
                source_kind: "detail".to_string(),
                source_widget_id: "amount".to_string(),
                source_system_key: String::new(),
                constant_value: None,
                target_field_id: "fldAmount".to_string(),
                converter: "number".to_string(),
                option_map_json: None,
                required: true,
            }],
        }
    }

    #[test]
    fn a_valid_draft_parses_without_io() {
        let local = parse_local(&draft()).unwrap_or_else(|reasons| panic!("{reasons:?}"));
        assert_eq!(local.trigger_mode, TriggerMode::NodePassed);
        assert_eq!(local.row_filter, RowFilter::MatchedRows);
        assert_eq!(local.maps.len(), 1);
        assert!(local.maps[0].required);
    }

    #[test]
    fn complete_bitable_link_supplies_target_coordinates() {
        let mut value = draft();
        value.target_base_token =
            "https://tenant.example/base/bascnLink?table=tblLink&view=vewA".to_string();
        value.target_table_id.clear();
        let local = parse_local(&value).unwrap_or_else(|reasons| panic!("{reasons:?}"));
        assert_eq!(local.target_base_token, "bascnLink");
        assert_eq!(local.target_table_id, "tblLink");
    }

    #[test]
    fn conflicting_link_and_table_id_are_rejected() {
        let mut value = draft();
        value.target_base_token = "https://tenant.example/base/bascnLink?table=tblLink".to_string();
        value.target_table_id = "tblOther".to_string();
        let reasons = parse_local(&value).err().unwrap_or_default();
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains("目标多维表格链接非法")),
            "{reasons:?}"
        );
    }

    #[test]
    fn node_passed_requires_at_least_one_trigger_node() {
        let mut value = draft();
        value.trigger_node_ids.clear();
        let reasons = parse_local(&value).err().unwrap_or_default();
        assert!(
            reasons.iter().any(|reason| reason.contains("触发节点")),
            "{reasons:?}"
        );
    }

    #[test]
    fn final_approved_mode_does_not_require_trigger_nodes() {
        let mut value = draft();
        value.trigger_mode = "final_approved".to_string();
        value.trigger_node_ids.clear();
        assert!(parse_local(&value).is_ok());
    }

    #[test]
    fn marking_without_a_status_column_is_rejected() {
        let mut value = draft();
        value.status_field = None;
        let reasons = parse_local(&value).err().unwrap_or_default();
        assert!(
            reasons.iter().any(|reason| reason.contains("状态列")),
            "{reasons:?}"
        );
    }

    #[test]
    fn duplicate_target_columns_are_rejected() {
        let mut value = draft();
        let mut second = value.maps[0].clone();
        second.source_kind = "system".to_string();
        second.source_widget_id.clear();
        second.source_system_key = "instance_code".to_string();
        second.converter = "text".to_string();
        value.maps.push(second);
        let reasons = parse_local(&value).err().unwrap_or_default();
        assert!(
            reasons.iter().any(|reason| reason.contains("同一目标列")),
            "{reasons:?}"
        );
    }

    #[test]
    fn system_sources_carry_no_widget_id() {
        let mut value = draft();
        value.maps[0].source_kind = "system".to_string();
        value.maps[0].source_system_key = "instance_code".to_string();
        // 仍然带着 detail 的控件 id：必须被拒，避免来源身份有二义。
        let reasons = parse_local(&value).err().unwrap_or_default();
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains("不应填写控件 ID")),
            "{reasons:?}"
        );
    }

    #[test]
    fn the_system_key_list_only_contains_known_keys() {
        assert!(KNOWN_SYSTEM_KEYS.contains(&"instance_code"));
        assert!(KNOWN_SYSTEM_KEYS.contains(&"node_time"));
        assert!(!KNOWN_SYSTEM_KEYS.contains(&"magic"));
    }

    #[test]
    fn converter_target_compatibility_is_strict() {
        assert!(converter_target_compatible(Converter::Person, 11));
        assert!(!converter_target_compatible(Converter::Person, 1));
        assert!(converter_target_compatible(Converter::OptionText, 3));
        assert!(!converter_target_compatible(Converter::MultiOption, 3));
        assert!(converter_target_compatible(Converter::Url, 15));
        assert!(!converter_target_compatible(Converter::Number, 1));
    }

    #[test]
    fn converter_widget_compatibility_catches_obvious_mismatches() {
        assert!(converter_widget_compatible(Converter::Number, "amount"));
        assert!(!converter_widget_compatible(Converter::Number, "radioV2"));
        assert!(converter_widget_compatible(
            Converter::OptionText,
            "radioV2"
        ));
        assert!(!converter_widget_compatible(Converter::Person, "input"));
        assert!(converter_widget_compatible(Converter::Url, "attachmentV2"));
    }

    #[test]
    fn single_select_status_column_must_already_have_every_label_as_an_option() {
        let field: FieldItem = serde_json::from_value(serde_json::json!({
            "field_id": "fldStatus",
            "field_name": "状态",
            "type": 3,
            "ui_type": "SingleSelect",
            "property": {"options": [
                {"name": "有效"}, {"name": "已退回"}, {"name": "已拒绝"}, {"name": "已撤回"}
            ]}
        }))
        .unwrap_or_else(|error| panic!("{error}"));
        let mut reasons = Vec::new();
        validate_status_options(&field, &StatusLabels::default(), &mut reasons);
        assert_eq!(reasons.len(), 2, "缺已撤销/已删除：{reasons:?}");
        assert!(reasons.iter().any(|reason| reason.contains("已撤销")));
        assert!(reasons.iter().any(|reason| reason.contains("已删除")));

        let text: FieldItem = serde_json::from_value(serde_json::json!({
            "field_id": "fldStatus", "field_name": "状态", "type": 1
        }))
        .unwrap_or_else(|error| panic!("{error}"));
        let mut reasons = Vec::new();
        validate_status_options(&text, &StatusLabels::default(), &mut reasons);
        assert!(reasons.is_empty(), "文本列不校验选项");
    }

    #[test]
    fn form_and_node_snapshots_hash_stably() {
        let form = r#"[{"id":"a"}]"#;
        assert_eq!(sha256_hex(form), sha256_hex(form));
        assert_eq!(sha256_hex(form).len(), 64);
        assert_ne!(sha256_hex(form), sha256_hex("[]"));
        assert_eq!(
            form_snapshot_text(Some(&Value::String(form.to_string()))),
            form
        );
        assert_eq!(
            form_snapshot_text(Some(&serde_json::json!([{"id":"a"}]))),
            form
        );
        assert_eq!(form_snapshot_text(None), "");
    }
}

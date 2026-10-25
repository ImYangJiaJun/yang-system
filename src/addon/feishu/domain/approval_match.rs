//! 审批定义控件的**按名自动匹配**（默认链路）。
//!
//! # 为什么需要这一层
//!
//! 使用方的多维表格列名与审批控件名**严格对应**，因此不该要求人工逐条配
//! `feishu_approval_field_map`。本模块把「审批定义的 `form` + 多维表格的列名」
//! 折成一张 `WidgetMap` 表——**匹配规则全部在这里，且是纯函数**，可以直接拿
//! 真实响应喂进来测，不需要碰飞书 API 或数据库。
//!
//! # 两条匹配轴
//!
//! 1. **控件 ↔ 列名**：按 `name` 精确匹配（实测中审批控件的 `name` 是服务端解好
//!    的可读中文，不是 `@i18n@` 键）。
//! 2. **选项值 ↔ 列里的选项**：只在控件 `option` 是**数组**（固定选项）时派生，
//!    按 `option[].text` ↔ 列选项名对上后取 `option[].value`。
//!
//! 级联链接态或使用本系统选项接口路径的控件，从本地外部选项
//! 数据源（`feishu_option`）取值，由调用方把 `option_map` 补进 `WidgetMap`——那一段
//! 不在本模块内，因为它读的是数据库而不是飞书响应。
//!
//! # `option` 是多态字段（实测）
//!
//! 同一个 `option` 键在不同控件上分别是 **缺失 / `null` / `array` / `object`**：
//!
//! | 形态 | 实测出现在 | 语义 |
//! |---|---|---|
//! | 缺失 | `input` | 该控件没有选项概念 |
//! | `null` | `input`（另一个控件） | 同上，但**键存在**——两种都要当作「无选项」 |
//! | `array` | `radioV2`（固定选项） | 固定选项，`text`/`value` 可派生 |
//! | `object` | `fieldList` / `connect` | **不是选项**，是该控件的配置 |
//!
//! 因此判别器是**两维**的：`type` 决定控件语义，`option` 的**类型**决定值能否
//! 派生。`null` 与「缺失」在 serde 里会撞到同一个 arm，但它们都是「无选项」，
//! 结局相同——这条巧合是安全的，不是被依赖的。
//!
//! # 为什么用 untagged 枚举而不是手写 `match`
//!
//! 手写 `match` 需要在五处（缺失、null、string、array、object）分别写 arm，
//! 漏一处就静默当成「有选项但解析不出」——而那会表现为「该控件的选项映射为空」
//! 这种查不到源头的错。serde 的 untagged 让形态在**类型系统**里显式化。

#![allow(dead_code)] // 按名匹配先落地并自带 15 个测试（含真实响应形状）；消费者（dispatch 端点的自动建配置）在下一步接入。

use std::collections::BTreeMap;

use serde::Deserialize;

use super::approval_convert::{Converter, WidgetMap};
use super::bitable::FieldItem;
use super::widget_rules;

/// 审批定义 `form` 字段里的多态 `option` 取值。
///
/// # 为什么是 `Option` 外面套 untagged
///
/// 实测里同一个 `option` 键有**四种**形态：缺失 / `null` / 数组 / 对象。
/// `Option<WidgetOptions>` 让「缺失」与 `null` 由 serde 直接折成 `None`
/// ——这是唯一**不依赖变体声明顺序**的表达方式（untagged 单元变体会抢在
/// 数组与对象之前匹配，把后面两个 arm 全部挡死）。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub(crate) enum WidgetOptions {
    /// 固定选项数组（`radioV2` / `checkboxV2` 的真实形态）。
    Fixed(Vec<OptionItem>),
    /// 明细 / 关联审批等控件的配置对象——**不是**选项。
    Config(serde_json::Value),
}

/// 一个固定选项：`text` 是可读文案，`value` 是提交时要用的值。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub(crate) struct OptionItem {
    #[serde(default)]
    pub(crate) text: String,
    #[serde(default)]
    pub(crate) value: String,
}

/// 审批定义 `form` 数组里的一项。
///
/// # 只声明真正要用的字段
///
/// 官方响应里还有 `visible` / `printable` / `display_condition` / `widget_default_value` /
/// `enable_default_value` / `default_value_type` 等字段，这里一概不声明——
/// **缺字段不会失败**（serde 忽略未知键），而多声明几个用不到的字段只会让
/// 「什么进了配置」变得更难读。`externalData` 也不是为了它的内容，而是为了让
/// **类型系统确认这确实是 JSON**——见下方 `external_data` 的说明。
#[derive(Debug, Clone, Deserialize)]
#[serde(from = "FormWidgetRaw")]
pub(crate) struct FormWidget {
    /// 控件 id。创建实例时要的 `form[].id`。
    pub(crate) id: String,
    /// 控件名。**实测是服务端解好的可读中文**（不是 `@i18n@` 键）——这是按名匹配的依据。
    pub(crate) name: String,
    /// 控件类型（`input` / `radioV2` / `fieldList` / …）。
    pub(crate) r#type: String,
    /// 是否必填。判定「配置期报错」的依据。
    pub(crate) required: bool,
    /// 关联审批允许的定义 Code；其他控件的 value 保持原始形态。
    pub(crate) value: serde_json::Value,
    /// 多态选项；`None` 表示「无选项」（键缺失或值为 `null`）。
    pub(crate) option: Option<WidgetOptions>,
    /// 外部数据源链接。
    ///
    /// 声明成 `serde_json::Value` 是**为了能识别出这个键存在**——实测里
    /// 识别级联链接态与非级联选项接口；实际选项由调用方查本地绑定。
    pub(crate) external_data: Option<serde_json::Value>,
    /// 子控件（明细在 `children` 里，控件组由 `value` 解析而来）。
    pub(crate) children: Vec<FormWidget>,
}

/// [`FormWidget`] 的反序列化中间形态。
///
/// 控件组的子控件定义在 `value` 数组里（不是 `children`），serde 无法直接
/// 落到同一个字段上，所以先原样收下，再在 `From` 里按控件组规则筛选与排序。
#[derive(Debug, Clone, Deserialize)]
struct FormWidgetRaw {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    r#type: String,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    value: serde_json::Value,
    #[serde(default)]
    option: Option<WidgetOptions>,
    /// **`rename` 不能省**：JSON 键是驼峰的 `externalData`，Rust 字段是
    /// snake_case。没有这一行，字段恒为 `None`，而症状是「链接态被当成非链接」
    /// ——一条静默的、指向完全相反方向的判定。
    #[serde(default, rename = "externalData")]
    external_data: Option<serde_json::Value>,
    #[serde(default)]
    children: Vec<FormWidget>,
}

impl From<FormWidgetRaw> for FormWidget {
    fn from(raw: FormWidgetRaw) -> Self {
        // 控件组：子控件在 `value` 数组里，只留白名单内且可见的，并按白名单排序。
        // 其它类型（明细等）沿用 `children`。
        let children = match widget_rules::group_child_ids(&raw.r#type) {
            Some(whitelist) => group_children(&raw, whitelist),
            None => raw.children,
        };
        Self {
            id: raw.id,
            name: raw.name,
            r#type: raw.r#type,
            required: raw.required,
            value: raw.value,
            option: raw.option,
            external_data: raw.external_data,
            children,
        }
    }
}

/// 解析控件组的子控件：丢弃 `visible=false`（缺省可见）与白名单外的项，
/// 再按白名单顺序稳定排序；`value` 不是数组（含 JSON 字符串形态）时视为没有子控件。
fn group_children(raw: &FormWidgetRaw, whitelist: &[&str]) -> Vec<FormWidget> {
    let parsed;
    let items: &[serde_json::Value] = match &raw.value {
        serde_json::Value::Array(items) => items,
        serde_json::Value::String(text) => {
            parsed = serde_json::from_str::<Vec<serde_json::Value>>(text).unwrap_or_default();
            &parsed
        }
        _ => return Vec::new(),
    };
    let mut children: Vec<FormWidget> = items
        .iter()
        .filter(|item| {
            item.get("visible")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true)
        })
        .filter_map(|item| serde_json::from_value::<FormWidget>(item.clone()).ok())
        .filter(|child| whitelist.contains(&child.id.as_str()))
        .collect();
    children.sort_by_key(|child| {
        whitelist
            .iter()
            .position(|id| *id == child.id)
            .unwrap_or(usize::MAX)
    });

    // 加班控件组的两个条件必填项必须按实例文档校正：
    // - 关联了加班规则（isSetRule=1）时，加班类型由规则决定，定义里的 required
    //   可能仍是 true，但提单时无需填写；
    // - 加班时段无论定义怎么写，实例接口都要求至少一段。
    if raw.r#type == "workGroup" {
        let rule_linked = matches!(
            raw.option.as_ref(),
            Some(WidgetOptions::Config(option)) if option_flag_is_one(option.get("isSetRule"))
        );
        for child in &mut children {
            match child.id.as_str() {
                "widgetWorkGroupType" if rule_linked => child.required = false,
                "widgetWorkGroupTimeRangeFieldList" => child.required = true,
                _ => {}
            }
        }
    }
    children
}

/// `isSetRule` 在真实响应里可能是数字 1，也可能是布尔 true。
fn option_flag_is_one(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Number(number)) => number.as_i64() == Some(1),
        Some(serde_json::Value::Bool(flag)) => *flag,
        _ => false,
    }
}

impl FormWidget {
    /// 返回关联审批允许的定义 Code；`None` 表示定义明确声明不限制范围。
    pub(crate) fn connect_definition_codes(&self) -> Result<Option<Vec<String>>, String> {
        if self.r#type != "connect" {
            return Ok(None);
        }
        if let Some(WidgetOptions::Config(option)) = self.option.as_ref() {
            if option
                .get("notLimitScope")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            {
                return Ok(None);
            }
        }
        if let Some(values) = self.value.as_array() {
            let codes = values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|code| !code.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            return if codes.is_empty() {
                Err("关联审批控件没有有效的审批定义 Code".to_string())
            } else {
                Ok(Some(codes))
            };
        }
        let Some(WidgetOptions::Config(option)) = self.option.as_ref() else {
            return Err("关联审批控件缺少关联范围配置".to_string());
        };
        let codes = option
            .get("definitionIds")
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|code| !code.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if codes.is_empty() {
            Err("关联审批控件没有有效的审批定义 Code，也未声明不限制范围".to_string())
        } else {
            Ok(Some(codes))
        }
    }

    /// 收集自己与全部后代——子控件也参与按名匹配。
    ///
    /// 返回 `Vec` 而不是 `impl Iterator`：递归返回的匿名类型无法在 trait 方法里
    /// 稳定地表达（E0720），而控件数量只有几十个，收集一次的开销可忽略。
    pub(crate) fn walk(&self) -> Vec<&FormWidget> {
        let mut out = vec![self];
        for child in &self.children {
            out.extend(child.walk());
        }
        out
    }

    /// 该控件是否需要由调用方补齐本地外部选项。
    ///
    /// 有 URL 时只识别本系统接口的路径形状，不判定主机归属或访问该 URL；
    /// 无 URL 时回退到级联标志，实际数据源身份由调用方验证本地绑定。
    pub(crate) fn links_to_our_options(&self) -> bool {
        let Some(external) = &self.external_data else {
            return false;
        };
        if let Some(url) = external
            .get("externalUrl")
            .and_then(serde_json::Value::as_str)
            .filter(|u| !u.is_empty())
        {
            return url.split_once("://").is_some_and(|(scheme, rest)| {
                matches!(scheme, "http" | "https")
                    && rest.split_once('/').is_some_and(|(host, path)| {
                        !host.is_empty()
                            && !host.chars().any(char::is_whitespace)
                            && format!("/{path}")
                                .split(['?', '#'])
                                .next()
                                .unwrap_or_default()
                                .strip_prefix("/api/v1/feishu/approval/options/")
                                .is_some_and(|key| {
                                    !key.is_empty()
                                        && key.bytes().all(|c| {
                                            c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-')
                                        })
                                })
                    })
            });
        }
        external
            .get("externalDataLinkage")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }
}

/// 多维表格侧的列定义（按名匹配与类型兼容性判定要用的几样）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Column {
    pub(crate) field_id: String,
    pub(crate) field_name: String,
    /// 列里的选项名。只有 select 类型的列才有。
    pub(crate) options: Vec<String>,
    /// 官方字段类型码。**多选 `4` 与单选 `3` 的选项列表一模一样**，只靠
    /// `options` 分不开；而「多选列不能填进单选控件」正是要拦的那一类。
    pub(crate) field_type: Option<i32>,
}

/// 由《列出字段》的响应构造按名匹配用的列集合。
///
/// # 为什么这一步不能省
///
/// 按名匹配要的是**列名 + 选项名**，而两样都只在 `list_all_fields` 里——
/// 直接用调用方传来的列名会丢掉选项（`property.options[].name`），
/// 单选控件的选项值就派生不出来。
pub(crate) fn columns_from_fields(fields: &[FieldItem]) -> Vec<Column> {
    fields
        .iter()
        .map(|field| Column {
            field_id: field.field_id.clone(),
            field_name: field.field_name.trim().to_string(),
            options: field.option_names(),
            field_type: field.field_type,
        })
        .collect()
}

/// 解析一个「可能是 `field_id`、也可能是列名」的坐标字符串。
///
/// # 为什么两种都要接受
///
/// 请求体由人在工作流的 `raw_body` 里写死，两种形态都自然——有人写 id（从
/// 列表接口复制来的），有人写列名（自己起的）。**优先按 `field_id` 匹配**：
/// id 是稳定标识，而列名可以被改、也可能有重名；只在 id 不命中时才退到按名匹配。
///
/// # 用 id 存、用名匹配是两条轴，不要混
///
/// 本函数**只做解析**，不决定存什么：解析结果总带 `field_id` 与 `field_name`，
/// 存储一律用 id（用户改列名不会让配置失效），而 API 的投影与回写用名
/// （见 `bitable::get_records_by_ids` 的 `field_names` 与 `batch_update` 的 `fields` key）。
pub(crate) fn resolve_field_coord<'a>(columns: &'a [Column], given: &str) -> Option<&'a Column> {
    let target = given.trim();
    if target.is_empty() {
        return None;
    }
    columns
        .iter()
        .find(|column| column.field_id == target)
        .or_else(|| columns.iter().find(|column| column.field_name == target))
}

/// 与 [`resolve_field_coord`] 对称的**逐项解析**：一条配置的两个关键坐标
/// 与全部映射行，全部一次性解析完。
///
/// 返回 `None` 表示「坐标里有一个在表里找不到」——调用方据此报错并回填，
/// 而不是带着半套坐标继续跑（半套的后果是：创建成功了，但回填写进错的列）。
pub(crate) struct ResolvedCoords<'a> {
    pub(crate) applicant: &'a Column,
    pub(crate) backfill: &'a Column,
}

/// 解析申请与回填两个坐标；任一失败即 `None`。
pub(crate) fn resolve_key_coords<'a>(
    columns: &'a [Column],
    applicant: &str,
    backfill: &str,
) -> Option<ResolvedCoords<'a>> {
    let applicant = resolve_field_coord(columns, applicant)?;
    let backfill = resolve_field_coord(columns, backfill)?;
    // 同一列被当申请又当回填：提交时会把发起人当成编号读，两边都是错的。
    // 与其让它静默错，不如在配置期拒绝。
    if applicant.field_id == backfill.field_id {
        return None;
    }
    Some(ResolvedCoords {
        applicant,
        backfill,
    })
}

/// 按名匹配的失败原因。
///
/// 每一条都会**写进回填字段**（使用方要的就是「配置校验不过就报错」），
/// 所以文案要指向**可行动的修复**，不能只是「匹配失败」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MatchError {
    InvalidMapping(String),
    /// 必填控件在多维表格里没有同名列。
    MissingRequiredColumn {
        widget: String,
        widget_type: String,
    },
    /// 列名与控件名匹配上了，但类型对不上（例如列是单选而控件是数字）。
    TypeMismatch {
        widget: String,
        widget_type: String,
        column: String,
        column_kind: String,
    },
    /// 多选列被填进单选控件：一条记录可能有多个值，单选控件装不下。
    MultiValueColumn {
        widget: String,
        widget_type: String,
        column: String,
    },
    /// 同名列出现多次（多维表格列名不唯一）。
    AmbiguousColumn {
        widget: String,
        column: String,
        count: usize,
    },
    /// 必填控件的选项**无法从列里派生**（`option` 不是数组）。
    UnresolvableOptions {
        widget: String,
        widget_type: String,
    },
    /// 该控件类型不支持通过 API 提单（官方不支持清单）。
    UnsupportedWidget {
        widget: String,
        widget_type: String,
    },
    /// 需要人工准备值的控件（旧附件控件 / 文档 / 地址），配置期直接拒绝。
    ManualValueWidget {
        widget: String,
        widget_type: String,
    },
    /// 两个不同的控件名落在同一列上。
    ColumnReused {
        column: String,
        first: String,
        second: String,
    },
}

impl std::fmt::Display for MatchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidMapping(message) => formatter.write_str(message),
            Self::MissingRequiredColumn {
                widget,
                widget_type,
            } => write!(
                formatter,
                "必填控件「{widget}」（{widget_type}）没有配置有效的多维表格列（自动匹配时需同名列）"
            ),
            Self::TypeMismatch {
                widget,
                widget_type,
                column,
                column_kind,
            } => write!(
                formatter,
                "控件「{widget}」（{widget_type}）与同名列「{column}」（{column_kind}）类型不匹配"
            ),
            Self::MultiValueColumn {
                widget,
                widget_type,
                column,
            } => write!(
                formatter,
                "控件「{widget}」（{widget_type}）是单选控件，但列「{column}」是多选列：\
                 多选列只能映射到多选控件（checkboxV2），或改用单值列"
            ),
            Self::AmbiguousColumn {
                widget,
                column,
                count,
            } => write!(
                formatter,
                "控件「{widget}」对应的列名「{column}」在表内出现 {count} 次，不唯一"
            ),
            Self::UnresolvableOptions {
                widget,
                widget_type,
            } => write!(
                formatter,
                "必填控件「{widget}」（{widget_type}）的选项无法从列里派生：该控件没有固定选项可对"
            ),
            Self::UnsupportedWidget { widget, widget_type } => write!(
                formatter,
                "控件「{widget}」（{widget_type}）不支持通过 API 提单：{}",
                widget_rules::unsupported_reason(widget_type)
            ),
            Self::ManualValueWidget { widget, widget_type } => write!(
                formatter,
                "控件「{widget}」（{widget_type}）需要人工准备值（旧附件控件 / 文档 / 地址），不参与自动派发"
            ),
            Self::ColumnReused {
                column,
                first,
                second,
            } => write!(
                formatter,
                "列「{column}」同时被控件「{first}」与「{second}」使用，一列只能对应一个控件"
            ),
        }
    }
}

impl std::error::Error for MatchError {}

/// 按名匹配的结果：成功是映射表，失败是**全部**失败原因。
#[derive(Debug)]
pub(crate) enum MatchOutcome {
    /// 全部控件都对上了。
    Matched(Vec<WidgetMap>),
    /// 至少一条不成立——把**所有**原因一起报出去（一次只报一条会让人
    /// 修完一条才能看到下一条，来回多轮才配好）。
    Invalid(Vec<MatchError>),
}

/// 由审批定义与列定义按名匹配出一张映射表。
///
/// **全量校验**：一次返回**所有**不成立的控件，而不是遇到第一个就停——
/// 使用方要「配置期就报错」，而配置期一次改完比改一条看一条快得多。
pub(crate) fn match_by_name(form: &[FormWidget], columns: &[Column]) -> MatchOutcome {
    match_fields(form, columns, None)
}

/// 显式映射省略时保留旧的按名匹配；传入时以控件和列 ID 为准。
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct FieldMapping {
    pub(crate) widget_id: String,
    pub(crate) bitable_field: String,
    /// 外部选项来源的稳定标识；省略时按控件名称自动识别。
    #[serde(default)]
    pub(crate) option_source_key: Option<String>,
}

/// 这类控件会不会出现在配置里（明细 / 控件组父级只作分组展示，仍然出现）。
///
/// 判据统一在 [`widget_rules`]：官方不支持提单的控件不出现在配置向导。
fn is_configurable_control(widget_type: &str) -> bool {
    widget_rules::is_configurable(widget_type)
}

/// 这类控件会不会被写进映射表。
///
/// **派发期的必填判定必须用同一个谓词**（`approval_convert::assemble_definition`）：
/// 配置期静默跳过的控件在映射表里没有行，派发期再按 `required` 拦，报出来的就是
/// 一个「来源字段」根本不存在的必填错误——而且补数据永远不会让它消失。
pub(crate) fn is_eligible_for_mapping(widget_type: &str) -> bool {
    widget_rules::is_mappable(widget_type)
}

pub(crate) fn mapping_widgets(form: &[FormWidget]) -> Vec<(&FormWidget, String)> {
    preview_widgets(form)
        .into_iter()
        .filter(|(widget, _)| is_eligible_for_mapping(&widget.r#type))
        .collect()
}

/// 预览/按名匹配用的控件名。控件组父级可能没有名称（官方定义里 leaveGroupV2
/// 的 name 就是空串），用类型标签兜底，避免子控件限定名变成 `_假期类型`。
fn preview_name(widget: &FormWidget) -> &str {
    let name = widget.name.trim();
    if !name.is_empty() {
        return name;
    }
    match widget.r#type.as_str() {
        "fieldList" => "明细",
        "leaveGroupV2" => "请假控件组",
        "workGroup" => "加班控件组",
        "outGroup" => "外出控件组",
        _ => "",
    }
}

/// 预览保留明细 / 控件组父级与定义顺序；父级仅展示，不参与列映射。
pub(crate) fn preview_widgets(form: &[FormWidget]) -> Vec<(&FormWidget, String)> {
    fn collect<'a>(form: &'a [FormWidget], parent: &str, flat: &mut Vec<(&'a FormWidget, String)>) {
        for widget in form {
            let display = preview_name(widget);
            let name = if parent.is_empty() {
                display.to_string()
            } else {
                format!("{parent}_{display}")
            };
            let supported = is_configurable_control(&widget.r#type);
            if supported {
                flat.push((widget, name.clone()));
            }
            collect(
                &widget.children,
                if supported { name.as_str() } else { parent },
                flat,
            );
        }
    }
    let mut flat = Vec::new();
    collect(form, "", &mut flat);
    flat
}

pub(crate) fn match_fields(
    form: &[FormWidget],
    columns: &[Column],
    maps: Option<&[FieldMapping]>,
) -> MatchOutcome {
    let mut errors: Vec<MatchError> = Vec::new();
    let mut widgets: Vec<WidgetMap> = Vec::new();
    let mut column_owner: BTreeMap<String, String> = BTreeMap::new();

    // 官方实例 API 不支持 / 本系统暂未实现的控件不会进 mapping_widgets（预览阶段
    // 已过滤），所以不能依赖下面的遍历来报错；这里单独走整棵定义树。只拦**必填**：
    // 可选项留空是合法的，静默跳过即可。
    for widget in form.iter().flat_map(FormWidget::walk) {
        if widget.required && widget_rules::must_reject_at_config(&widget.r#type) {
            errors.push(MatchError::UnsupportedWidget {
                widget: widget.name.clone(),
                widget_type: widget.r#type.clone(),
            });
        }
    }

    let flat = mapping_widgets(form);
    let mut selected = BTreeMap::new();
    for map in maps.into_iter().flatten() {
        if let Some(source) = &map.option_source_key {
            if source.trim().is_empty() || source.len() > 64 {
                errors.push(MatchError::InvalidMapping(format!(
                    "控件「{}」的外部选项来源必须是 1 至 64 字符的 source_key",
                    map.widget_id
                )));
            }
            if !flat
                .iter()
                .any(|(widget, _)| widget.id == map.widget_id && widget.links_to_our_options())
            {
                errors.push(MatchError::InvalidMapping(format!(
                    "控件「{}」没有使用本系统外部选项，不能指定选项来源",
                    map.widget_id
                )));
            }
        }
        if !flat.iter().any(|(widget, _)| widget.id == map.widget_id) {
            errors.push(MatchError::InvalidMapping(format!(
                "审批控件「{}」不存在",
                map.widget_id
            )));
        }
        if !columns
            .iter()
            .any(|column| column.field_id == map.bitable_field)
        {
            errors.push(MatchError::InvalidMapping(format!(
                "多维表格列「{}」不存在",
                map.bitable_field
            )));
        }
        if selected
            .insert(map.widget_id.as_str(), map.bitable_field.as_str())
            .is_some()
        {
            errors.push(MatchError::InvalidMapping(format!(
                "控件「{}」重复配置",
                map.widget_id
            )));
        }
    }

    for (widget, qualified_name) in flat {
        if widget_rules::is_unsupported(&widget.r#type) {
            if widget.required || selected.contains_key(widget.id.as_str()) {
                errors.push(MatchError::UnsupportedWidget {
                    widget: widget.name.clone(),
                    widget_type: widget.r#type.clone(),
                });
            }
            continue;
        }
        if widget_rules::needs_manual_value(&widget.r#type) {
            // **可选**的人工值控件不拦（留空提交是合法的，官方允许不传必填控件
            // 之外的任何控件）；**必填**的拦下来——留空会让审批单缺核心内容。
            if widget.required || selected.contains_key(widget.id.as_str()) {
                errors.push(MatchError::ManualValueWidget {
                    widget: widget.name.clone(),
                    widget_type: widget.r#type.clone(),
                });
            }
            continue;
        }

        // 有前缀列时只匹配它，歧义不能退回裸列名绕过。
        let name = if columns
            .iter()
            .any(|c| c.field_name.trim() == qualified_name)
        {
            qualified_name.as_str()
        } else {
            widget.name.as_str()
        };
        let column = if maps.is_some() {
            selected
                .get(widget.id.as_str())
                .and_then(|id| columns.iter().find(|c| c.field_id == *id))
        } else {
            find_column(columns, name, &mut errors)
        };
        let Some(column) = column else {
            if widget.required {
                errors.push(MatchError::MissingRequiredColumn {
                    widget: widget.name.clone(),
                    widget_type: widget.r#type.clone(),
                });
            }
            continue;
        };
        if let Err(reason) = widget.connect_definition_codes() {
            errors.push(MatchError::InvalidMapping(format!(
                "关联审批控件「{}」（{}）配置无效：{reason}",
                widget.name, widget.id
            )));
            continue;
        }
        if maps.is_some() && find_column(columns, &column.field_name, &mut errors).is_none() {
            continue;
        }

        // 一列只能服务一个控件：**键是 `field_id`**。
        //
        // 用 `field_name` 当键是错的——那样两条名字不同的控件指向同一个
        // `field_id`（列名被改过、映射表是旧的）时，`previous != name` 恒真但
        // `get(name)` 取不到，检测**永远不触发**，一条列被两个控件静默复用。
        // `field_id` 才是「同一列」的判据，而名字只是它的展示态。
        if let Some(previous) = column_owner.get(&column.field_id) {
            errors.push(MatchError::ColumnReused {
                column: column.field_name.clone(),
                first: previous.clone(),
                second: widget.name.clone(),
            });
            continue;
        }
        column_owner.insert(column.field_id.clone(), widget.name.clone());

        if let Err(error) = check_type_compatibility(widget, column) {
            // 类型不匹配只在**必填**时报错：可选控件对不上可以留空，
            // 让它进映射反而会在提交时被 1390001 拒掉整单。
            if widget.required || maps.is_some() {
                errors.push(error);
            }
            continue;
        }

        // 选项派生。固定选项（数组）按 `text` ↔ 列选项名；链接到本系统数据源的
        // 交给调用方补；两者都不是时——必填才报错（可选控件留空即可）。
        let option_map = match widget.option {
            Some(WidgetOptions::Fixed(ref items)) => {
                derive_option_map(items, &column.options, widget, &mut errors)
            }
            // 缺失 / null / 对象：都没有可派生的固定选项。
            _ => BTreeMap::new(),
        };
        // 假勤/加班类型子控件只有在「没有固定选项可派生」时才允许直取；
        // 有固定选项（例如未关联加班规则的加班类型）仍按常规选项映射处理。
        let raw_option_direct = widget_rules::is_raw_option_child(&widget.id)
            && !widget.links_to_our_options()
            && !has_fixed_options(widget);
        if (widget.required || maps.is_some())
            && !widget.links_to_our_options()
            && !raw_option_direct
            && option_map.is_empty()
            && matches!(
                widget.r#type.as_str(),
                "radioV2" | "radio" | "checkboxV2" | "checkbox"
            )
        {
            errors.push(MatchError::UnresolvableOptions {
                widget: widget.name.clone(),
                widget_type: widget.r#type.clone(),
            });
            continue;
        }

        widgets.push(WidgetMap {
            widget_id: widget.id.clone(),
            widget_type: widget.r#type.clone(),
            required: widget.required,
            bitable_field: column.field_id.clone(),
            converter: converter_for(widget),
            option_map,
            option_source_key: None,
            currency: None,
        });
    }

    if errors.is_empty() {
        MatchOutcome::Matched(widgets)
    } else {
        MatchOutcome::Invalid(errors)
    }
}

/// 按 `name` 找唯一同名列。
fn find_column<'a>(
    columns: &'a [Column],
    widget_name: &str,
    errors: &mut Vec<MatchError>,
) -> Option<&'a Column> {
    let mut matches = columns
        .iter()
        .filter(|column| column.field_name.trim() == widget_name.trim());
    let first = matches.next()?;
    // 列名唯一由**调用方**保证（多维表格允许重名，而 API 按名字取值会拿到
    // 不确定的一列）。这里仍显式判一次，因为它命中时的表现是「取到错的列」
    // ——那比「取不到」更难查。
    if matches.next().is_some() {
        errors.push(MatchError::AmbiguousColumn {
            widget: widget_name.to_string(),
            column: widget_name.to_string(),
            count: columns
                .iter()
                .filter(|column| column.field_name.trim() == widget_name.trim())
                .count(),
        });
        return None;
    }
    Some(first)
}

/// 列类型 ↔ 控件类型的兼容性。
///
/// 只对**少数几类**做强校验：多维表格的列类型是数字编码（文本 / 数字 / 单选 …），
/// 而审批控件是字符串（`input` / `number` / `radioV2` …）。两边的分类法**不是
/// 一一对应**的（例如多维表格的 `select` 对上审批的 `radioV2` 也对上 `checkboxV2`），
/// 所以这里只拦「明显会让提交失败」的两类：数字对选项列、多选列对单选控件。
///
/// 多选列 → 单选控件这一条是**配置期**必须拦的：一条记录可能带多个值，单选控件
/// 装不下；旧实现放行到派发期，只在日志里报 `BadValue`（控制台看到的是脱敏后的
/// 「值的格式不符合审批控件要求」），用户很难归因。
fn check_type_compatibility(widget: &FormWidget, column: &Column) -> Result<(), MatchError> {
    // 多选列只能进**多选控件**（checkbox/checkboxV2）；radio 虽然同为 Option
    // 转换器，但只装得下一个值，同样必须拒绝。其它控件（input/date/…）会走
    // Direct 转换并静默取数组的第一个元素，更是必须拦下。
    let multi_value_widget = matches!(widget.r#type.as_str(), "checkbox" | "checkboxV2");
    if column.field_type == Some(4) && !multi_value_widget {
        return Err(MatchError::MultiValueColumn {
            widget: widget.name.clone(),
            widget_type: widget.r#type.clone(),
            column: column.field_name.clone(),
        });
    }
    let numeric_widget = matches!(widget.r#type.as_str(), "number" | "amount");
    let numeric_column = column.options.is_empty() && is_numeric_column(column);
    if numeric_widget && !numeric_column && !column.options.is_empty() {
        return Err(MatchError::TypeMismatch {
            widget: widget.name.clone(),
            widget_type: widget.r#type.clone(),
            column: column.field_name.clone(),
            column_kind: "选项列".to_string(),
        });
    }
    Ok(())
}

/// 该列是否是数值型——只在有 `kind` 提示时才判得准。
///
/// 由调用方在构造 [`Column`] 时决定 `options`；本函数只处理没有选项的列，
/// 一律放行（文本列也能被 `number` 读成数字，`approval_convert` 已经处理了
/// 「数字字段以字符串形态出现」这一类）。
fn is_numeric_column(_column: &Column) -> bool {
    false
}

/// 派生选项映射：控件选项的 `text` ↔ 列里的选项名 → 取控件的 `value`。
///
/// **对不上就跳过而不是报错**：一列可能只列出了定义中选项的一个子集
/// （例如这台表只用到「个人」而定义里还有「企业」）。缺失的那项在提交时会由
/// `approval_convert` 报「选项没有配置映射」，而那条错误会指名道姓。
fn derive_option_map(
    items: &[OptionItem],
    column_options: &[String],
    widget: &FormWidget,
    _errors: &mut Vec<MatchError>,
) -> BTreeMap<String, String> {
    let _ = widget;
    let mut map = BTreeMap::new();
    for item in items {
        if item.value.is_empty() || item.text.trim().is_empty() {
            continue;
        }
        // 列里有同名选项才对得上；列选项本身可能带尾缀（`*` 之类），
        // 所以两侧都 trim 后再比。
        if column_options
            .iter()
            .any(|option| option.trim() == item.text.trim())
        {
            map.insert(item.text.trim().to_string(), item.value.clone());
        }
    }
    map
}

/// 控件是否带有非空的固定选项（`radioV2` / `checkboxV2` 的常见形态）。
fn has_fixed_options(widget: &FormWidget) -> bool {
    matches!(
        widget.option,
        Some(WidgetOptions::Fixed(ref items)) if !items.is_empty()
    )
}

/// 控件类型 → 值转换器。
///
/// 与 `approval_convert` 的判据同源，但这里**在配置期**就定好并落库——
/// 运行期不再按 `type` 推断，少一处可能漂移的地方。
///
/// 假勤 / 加班类型子控件（`widgetLeaveGroupType` / `widgetWorkGroupType`）的
/// 值可能是飞书类型 ID 而不是文案、`option` 也可能为空：没有固定选项可供派生时
/// 直接取值，不能套选项映射。
fn converter_for(widget: &FormWidget) -> Converter {
    if widget_rules::is_raw_option_child(&widget.id)
        && !widget.links_to_our_options()
        && !has_fixed_options(widget)
    {
        return Converter::Direct;
    }
    match widget.r#type.as_str() {
        "date" | "dateInterval" => Converter::Date,
        "radio" | "radioV2" | "checkbox" | "checkboxV2" => Converter::Option,
        _ => Converter::Direct,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // 只有本模块的测试构造 `FieldItem`，生产路径用的是 `columns_from_fields`。
    use super::super::bitable::{FieldProperty, PropertyOption};

    /// 真实响应的最小复刻：`option` 的四种形态**都**要覆盖，因为它们是
    /// 「按类型判别」这条设计的全部依据。
    fn fixed_radio(name: &str, id: &str) -> FormWidget {
        FormWidget {
            id: id.to_string(),
            name: name.to_string(),
            r#type: "radioV2".to_string(),
            required: true,
            option: Some(WidgetOptions::Fixed(vec![
                OptionItem {
                    text: "个人".to_string(),
                    value: "mpuvnw0h-f91ofki6wgl-0".to_string(),
                },
                OptionItem {
                    text: "企业".to_string(),
                    value: "mpuvnw0h-fe2sqa3x3xk-0".to_string(),
                },
            ])),
            external_data: None,
            value: serde_json::Value::Null,
            children: Vec::new(),
        }
    }

    fn input(name: &str, id: &str, required: bool) -> FormWidget {
        FormWidget {
            id: id.to_string(),
            name: name.to_string(),
            r#type: "input".to_string(),
            required,
            // 真实响应里这一类**键存在但值是 null**，另一类干脆没有这个键。
            // 两种都要解析成「无选项」。
            option: None,
            external_data: None,
            value: serde_json::Value::Null,
            children: Vec::new(),
        }
    }

    fn linked_radio(name: &str, id: &str) -> FormWidget {
        FormWidget {
            id: id.to_string(),
            name: name.to_string(),
            r#type: "radioV2".to_string(),
            required: true,
            option: Some(WidgetOptions::Fixed(vec![])),
            external_data: Some(serde_json::json!({
                "externalDataLinkage": true,
                "key": "",
                "linkageConfigs": [{"linkageWidgetID": "widgetX", "value": "成都", "key": "fldeysrdna"}]
            })),
            value: serde_json::Value::Null,
            children: Vec::new(),
        }
    }

    fn detail(name: &str, id: &str) -> FormWidget {
        FormWidget {
            id: id.to_string(),
            name: name.to_string(),
            r#type: "fieldList".to_string(),
            required: true,
            option: Some(WidgetOptions::Config(
                serde_json::json!({"input_type": "LIST"}),
            )),
            external_data: None,
            value: serde_json::Value::Null,
            children: vec![
                input("费用明细/Expense details", "child1", true),
                FormWidget {
                    id: "child2".to_string(),
                    name: "付款金额/Payment Amount".to_string(),
                    r#type: "number".to_string(),
                    required: true,
                    option: Some(WidgetOptions::Config(
                        serde_json::json!({"maxValue": "", "minValue": ""}),
                    )),
                    external_data: None,
                    value: serde_json::Value::Null,
                    children: Vec::new(),
                },
            ],
        }
    }

    fn column(name: &str, id: &str, options: &[&str]) -> Column {
        Column {
            field_id: id.to_string(),
            field_name: name.to_string(),
            options: options.iter().map(|value| (*value).to_string()).collect(),
            field_type: Some(3),
        }
    }

    fn multi_column(name: &str, id: &str, options: &[&str]) -> Column {
        Column {
            field_type: Some(4),
            ..column(name, id, options)
        }
    }

    fn linked_checkbox(name: &str, id: &str) -> FormWidget {
        FormWidget {
            r#type: "checkboxV2".to_string(),
            ..linked_radio(name, id)
        }
    }

    fn mapping(widget: &str, field: &str) -> FieldMapping {
        FieldMapping {
            widget_id: widget.to_string(),
            bitable_field: field.to_string(),
            option_source_key: None,
        }
    }

    #[test]
    fn connect_scope_and_mapping_accept_official_and_live_definition_shapes() {
        for (config, expected) in [
            (
                serde_json::json!({"value":[" DEF-A ","DEF-B"]}),
                Some(vec!["DEF-A", "DEF-B"]),
            ),
            (
                serde_json::json!({"option":{"definitionIds":["DEF-A"],"notLimitScope":false}}),
                Some(vec!["DEF-A"]),
            ),
            (
                serde_json::json!({"value":[],"option":{"definitionIds":[],"notLimitScope":true}}),
                None,
            ),
        ] {
            let mut value =
                serde_json::json!({"id":"w","name":"关联审批","type":"connect","required":true});
            value
                .as_object_mut()
                .unwrap_or_else(|| panic!("缺对象"))
                .extend(
                    config
                        .as_object()
                        .unwrap_or_else(|| panic!("缺配置"))
                        .clone(),
                );
            let widget: FormWidget =
                serde_json::from_value(value).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(
                widget
                    .connect_definition_codes()
                    .unwrap_or_else(|e| panic!("{e}")),
                expected.map(|codes| codes.into_iter().map(str::to_string).collect())
            );
            let result = match_by_name(&[widget], &[column("关联审批", "f", &[])]);
            assert!(
                matches!(result, MatchOutcome::Matched(maps) if maps[0].converter == Converter::Direct)
            );
        }
    }

    #[test]
    fn connect_missing_scope_is_rejected_when_mapped() {
        for config in [
            serde_json::json!({}),
            serde_json::json!({"value":[]}),
            serde_json::json!({"option":{"definitionIds":[],"notLimitScope":false}}),
        ] {
            let mut value = serde_json::json!({"id":"w","name":"关联审批","type":"connect"});
            value
                .as_object_mut()
                .unwrap_or_else(|| panic!("缺对象"))
                .extend(
                    config
                        .as_object()
                        .unwrap_or_else(|| panic!("缺配置"))
                        .clone(),
                );
            let widget: FormWidget =
                serde_json::from_value(value).unwrap_or_else(|e| panic!("{e}"));
            assert!(widget.connect_definition_codes().is_err());
            assert!(matches!(
                match_fields(
                    &[widget],
                    &[column("关联审批", "f", &[])],
                    Some(&[mapping("w", "f")])
                ),
                MatchOutcome::Invalid(_)
            ));
        }
    }

    #[test]
    fn explicit_maps_override_names_and_keep_optional_widgets_unmapped() {
        let form = vec![input("金额", "w1", true), input("备注", "w2", false)];
        let columns = vec![
            column("金额", "same", &[]),
            column("实际付款", "chosen", &[]),
            column("备注", "note", &[]),
        ];
        let result = match_fields(&form, &columns, Some(&[mapping("w1", "chosen")]));
        let MatchOutcome::Matched(maps) = result else {
            panic!("显式选择应通过: {result:?}")
        };
        assert_eq!(maps.len(), 1);
        assert_eq!(maps[0].bitable_field, "chosen");
    }

    #[test]
    fn explicit_maps_reject_unknown_duplicate_missing_and_reused_ids() {
        let form = vec![input("金额", "w1", true), input("备注", "w2", false)];
        let columns = vec![column("付款", "f1", &[])];
        for maps in [
            vec![],
            vec![mapping("w1", "f1"), mapping("unknown", "f1")],
            vec![mapping("w1", "f1"), mapping("w2", "missing")],
            vec![mapping("w1", "f1"), mapping("w1", "f1")],
            vec![mapping("w1", "f1"), mapping("w2", "f1")],
        ] {
            assert!(
                matches!(
                    match_fields(&form, &columns, Some(&maps)),
                    MatchOutcome::Invalid(_)
                ),
                "非法映射不得静默保存: {maps:?}"
            );
        }
        assert!(matches!(
            match_fields(
                &form,
                &[column("付款", "f1", &[]), column("付款", "f2", &[])],
                Some(&[mapping("w1", "f1")])
            ),
            MatchOutcome::Invalid(_)
        ));
    }

    #[test]
    fn explicit_optional_invalid_types_and_options_are_rejected() {
        // `image` / `imageV2` 已改由附件上传链路自动处理，换成旧 v1 附件
        // （人工值，仍须在配置期拒绝）。
        for kind in ["text", "attachment", "number", "radioV2"] {
            let mut widget = input("可选", "w", false);
            widget.r#type = kind.to_string();
            assert!(
                matches!(
                    match_fields(
                        &[widget],
                        &[column("列", "f", &["个人"])],
                        Some(&[mapping("w", "f")])
                    ),
                    MatchOutcome::Invalid(_)
                ),
                "显式选择 {kind} 不得被跳过"
            );
        }
    }

    #[test]
    fn explicit_detail_and_attachment_mappings_preserve_child_ids() {
        let mut attachment = input("凭证", "file", true);
        attachment.r#type = "attachmentV2".to_string();
        let form = vec![detail("付款明细", "parent"), attachment];
        let columns = vec![
            column("描述", "f1", &[]),
            column("实际金额", "f2", &[]),
            column("附件", "f3", &[]),
        ];
        let names = mapping_widgets(&form);
        assert_eq!(names[0].1, "付款明细_费用明细/Expense details");
        assert!(!names.iter().any(|(widget, _)| widget.id == "parent"));
        let result = match_fields(
            &form,
            &columns,
            Some(&[
                mapping("child1", "f1"),
                mapping("child2", "f2"),
                mapping("file", "f3"),
            ]),
        );
        let MatchOutcome::Matched(maps) = result else {
            panic!("明细和附件应通过: {result:?}")
        };
        assert_eq!(maps.len(), 3);
        assert_eq!(maps[2].widget_type, "attachmentV2");
    }

    #[test]
    fn preview_preserves_detail_groups_but_mapping_excludes_them() {
        let mut outer = detail("付款明细", "parent");
        outer.children.push(detail("子明细", "nested"));
        let form = vec![outer];
        let preview = preview_widgets(&form);
        assert_eq!(preview.len(), 6);
        assert_eq!(preview[0].0.id, "parent");
        assert_eq!(preview[0].1, "付款明细");
        assert!(preview[0].0.required);
        assert_eq!(preview[1].1, "付款明细_费用明细/Expense details");
        assert_eq!(preview[3].0.id, "nested");
        assert_eq!(preview[3].1, "付款明细_子明细");
        assert_eq!(preview[4].1, "付款明细_子明细_费用明细/Expense details");
        let mappings = mapping_widgets(&form);
        assert_eq!(mappings.len(), 4);
        assert!(mappings
            .iter()
            .all(|(widget, _)| widget.r#type != "fieldList"));
    }

    #[test]
    fn preview_hides_non_submittable_controls_but_keeps_detail_children() {
        let mut copy = input("-副本", "text-copy", true);
        copy.r#type = "text".to_string();
        let mut note = input("说明 1", "text-note", true);
        note.r#type = "text".to_string();
        let form = vec![
            copy,
            note,
            FormWidget {
                id: "formula".to_string(),
                name: "合计".to_string(),
                r#type: "formula".to_string(),
                required: true,
                option: None,
                external_data: None,
                value: serde_json::Value::Null,
                children: Vec::new(),
            },
            FormWidget {
                id: "mutable-group".to_string(),
                name: "引用多维表格".to_string(),
                r#type: "mutableGroup".to_string(),
                required: true,
                option: None,
                external_data: None,
                value: serde_json::Value::Null,
                children: vec![input("交易类型/Transaction Type", "group-child", true)],
            },
            detail("付款明细", "detail"),
            input("备注", "note", false),
        ];

        let preview = preview_widgets(&form);
        assert_eq!(
            preview
                .iter()
                .map(|(widget, _)| widget.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "formula",
                "group-child",
                "detail",
                "child1",
                "child2",
                "note"
            ]
        );
        assert_eq!(preview[1].1, "交易类型/Transaction Type");
        assert!(matches!(
            match_fields(
                &form,
                &[
                    column("合计", "f4", &[]),
                    column("交易类型/Transaction Type", "f0", &[]),
                    column("付款明细_费用明细/Expense details", "f1", &[]),
                    column("付款明细_付款金额/Payment Amount", "f2", &[]),
                    column("备注", "f3", &[]),
                ],
                None,
            ),
            MatchOutcome::Matched(_)
        ));
    }

    /// 从**真实响应的字节**解析，钉住多态形态——这条是本模块存在的理由。
    #[test]
    fn real_form_shape_parses_with_all_four_option_kinds() {
        // 形状取自 `approvals/get` 的真实返回（`D0557DA6-…`）。
        let form_json = r#"[
          {"id":"w1","name":"SWIFT Address","type":"input","required":true},
          {"id":"w2","name":"收款方类型/Type of payee","type":"radioV2","required":true,
           "option":[{"value":"mpuvnw0h-f91ofki6wgl-0","text":"个人"}]},
          {"id":"w3","name":"付款明细/Payment Details","type":"fieldList","required":true,
           "option":{"input_type":"LIST"},
           "children":[{"id":"c1","name":"付款金额/Payment Amount","type":"number","required":true,
                        "option":{"maxValue":"","minValue":""}}]},
          {"id":"w4","name":"付款备注/Payment Remarks","type":"input","required":false,"option":null},
          {"id":"w5","name":"公司名称/Company name","type":"radioV2","required":true,"option":[],
           "externalData":{"externalDataLinkage":true,"key":"","linkageConfigs":[{"key":"fldeysrdna"}]}}
        ]"#;
        let form: Vec<FormWidget> = serde_json::from_str(form_json)
            .unwrap_or_else(|error| panic!("真实形状应可解析: {error}"));
        // 上面的字面量只为验形状，不验匹配；匹配另测。

        assert_eq!(form.len(), 5);
        // 缺失的 option 键与 null 的 option 键都要落到 None。
        assert!(form[0].option.is_none());
        assert!(form[3].option.is_none());
        // 数组要带 text/value。
        match form[1]
            .option
            .as_ref()
            .unwrap_or_else(|| panic!("w2 是单选，应有固定选项"))
        {
            WidgetOptions::Fixed(items) => {
                assert_eq!(items[0].value, "mpuvnw0h-f91ofki6wgl-0");
                assert_eq!(items[0].text, "个人");
            }
            other => panic!("期望 Fixed，实际 {other:?}"),
        }
        // 明细的 option 是 object，**不是**数组。
        assert!(matches!(form[2].option, Some(WidgetOptions::Config(_))));
        // 外部链接要识别出来。
        assert!(form[4].links_to_our_options());
        assert!(!form[1].links_to_our_options());
    }

    #[test]
    fn matched_columns_produce_maps_with_derived_option_values() {
        let form = vec![fixed_radio("收款方类型/Type of payee", "w1")];
        let columns = vec![column(
            "收款方类型/Type of payee",
            "fldAAA",
            &["个人", "企业"],
        )];
        match match_by_name(&form, &columns) {
            MatchOutcome::Matched(maps) => {
                assert_eq!(maps.len(), 1);
                assert_eq!(maps[0].widget_id, "w1");
                assert_eq!(maps[0].bitable_field, "fldAAA");
                assert_eq!(maps[0].converter, Converter::Option);
                // 派生的是**审批控件的 option value**，不是列里的选项文案。
                assert_eq!(
                    maps[0].option_map.get("个人").map(String::as_str),
                    Some("mpuvnw0h-f91ofki6wgl-0")
                );
            }
            MatchOutcome::Invalid(errors) => panic!(
                "不应失败: {:?}",
                errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()
            ),
        }
    }

    #[test]
    fn columns_from_fields_carry_options_from_property() {
        // 选项在 `property.options` 里——没有这一步，单选控件一个值都派生不出。
        let fields = vec![
            FieldItem {
                is_primary: false,
                field_id: "fldA".to_string(),
                field_name: "  收款方类型/Type of payee  ".to_string(),
                field_type: Some(3),
                ui_type: Some("SingleSelect".to_string()),
                property: Some(FieldProperty {
                    options: vec![
                        PropertyOption {
                            name: "个人".to_string(),
                        },
                        PropertyOption {
                            name: "企业".to_string(),
                        },
                    ],
                }),
            },
            FieldItem {
                is_primary: false,
                field_id: "fldB".to_string(),
                field_name: "SWIFT Address".to_string(),
                field_type: Some(1),
                ui_type: Some("Text".to_string()),
                property: None,
            },
        ];
        let columns = columns_from_fields(&fields);
        assert_eq!(columns.len(), 2);
        // 名字要 trim：多维表格的列名首尾空格会被官方去一次，但保险起见本地再去。
        assert_eq!(columns[0].field_name, "收款方类型/Type of payee");
        assert_eq!(columns[0].options, vec!["个人", "企业"]);
        assert!(columns[1].options.is_empty(), "无 property 的列是空选项");
    }

    #[test]
    fn resolve_field_coord_accepts_id_and_name() {
        let columns = vec![
            column("SWIFT Address", "fldA", &[]),
            column("收款方类型/Type of payee", "fldB", &["个人"]),
        ];
        // 用 id 与用名都要能解析到同一条。
        assert_eq!(
            resolve_field_coord(&columns, "fldB").map(|column| column.field_id.as_str()),
            Some("fldB")
        );
        assert_eq!(
            resolve_field_coord(&columns, "收款方类型/Type of payee")
                .map(|column| column.field_id.as_str()),
            Some("fldB")
        );
        // id 优先于名字（同名不同 id 时取 id 那条）。
        assert!(resolve_field_coord(&columns, "fldA").is_some());
        // 找不到与空串都返回 None。
        assert!(resolve_field_coord(&columns, "不存在的列").is_none());
        assert!(resolve_field_coord(&columns, "   ").is_none());
    }

    #[test]
    fn same_column_as_applicant_and_backfill_is_rejected() {
        // 同一列被当申请又当回填：提交时会把发起人当成编号读。
        let columns = vec![column("申请人", "fldP", &[])];
        assert!(
            resolve_key_coords(&columns, "fldP", "fldP").is_none(),
            "申请与回填同列应拒绝"
        );
        // 不同列要能过。
        let columns = vec![
            column("申请人", "fldP", &[]),
            column("审批编号", "fldB", &[]),
        ];
        assert!(resolve_key_coords(&columns, "fldP", "fldB").is_some());
        // 任一坐标解析不到即整体 None（调用方据此报错，而不是带半套坐标继续跑）。
        assert!(resolve_key_coords(&columns, "fldP", "没有的列").is_none());
    }

    #[test]
    fn missing_required_column_is_reported_not_skipped() {
        // 使用方要「配置期就报错」——必填没列必须**整个配置**失败，
        // 而不是少一条映射地配进去（那样会到处理某一行时才失败）。
        let form = vec![input("SWIFT Address", "w1", true)];
        match match_by_name(&form, &[]) {
            MatchOutcome::Matched(maps) => panic!("必填缺列不应成功: {maps:?}"),
            MatchOutcome::Invalid(errors) => {
                assert_eq!(errors.len(), 1);
                assert!(errors[0].to_string().contains("SWIFT Address"));
            }
        }
    }

    #[test]
    fn optional_missing_column_is_fine() {
        let form = vec![input("付款备注/Payment Remarks", "w1", false)];
        match match_by_name(&form, &[]) {
            MatchOutcome::Matched(maps) => assert!(maps.is_empty(), "无列的可选控件不落映射"),
            MatchOutcome::Invalid(errors) => panic!("可选缺列不应报错: {errors:?}"),
        }
    }

    #[test]
    fn all_errors_are_collected_not_first_wins() {
        // 一次报全部——改一条看一条会来回多轮才配好。
        let form = vec![
            input("SWIFT Address", "w1", true),
            input("收款方账号/Recipient's account number", "w2", true),
            input("付款备注/Payment Remarks", "w3", true),
        ];
        match match_by_name(&form, &[]) {
            MatchOutcome::Matched(_) => panic!("三条都缺列，不应成功"),
            MatchOutcome::Invalid(errors) => {
                assert_eq!(errors.len(), 3, "应一次报全部: {:?}", errors);
            }
        }
    }

    #[test]
    fn required_attachment_is_mapped_and_optional_connect_is_ignored() {
        // 附件自动搬运，可选关联没有映射列时留空提交。
        let required_attach = FormWidget {
            id: "w1".to_string(),
            name: "附件attachment".to_string(),
            r#type: "attachmentV2".to_string(),
            required: true,
            option: None,
            external_data: None,
            value: serde_json::Value::Null,
            children: Vec::new(),
        };
        match match_by_name(
            &[required_attach],
            &[column("附件attachment", "fldFile", &[])],
        ) {
            MatchOutcome::Matched(maps) => {
                assert_eq!(maps.len(), 1);
                assert_eq!(maps[0].bitable_field, "fldFile");
                assert_eq!(maps[0].converter, Converter::Direct);
            }
            MatchOutcome::Invalid(errors) => panic!("附件应自动搬运：{errors:?}"),
        }

        let optional_connect = FormWidget {
            r#type: "connect".to_string(),
            required: false,
            ..input("关联审批/Related Approval Form", "w2", false)
        };
        assert!(matches!(
            match_by_name(&[optional_connect], &[]),
            MatchOutcome::Matched(_)
        ));
    }

    #[test]
    fn detail_prefixed_columns_take_priority_and_reject_ambiguity() {
        let form = vec![detail("明细", "parent")];
        let columns = vec![
            column("明细_费用明细/Expense details", "prefixed", &[]),
            column("费用明细/Expense details", "bare", &[]),
            column("明细_付款金额/Payment Amount", "amount", &[]),
        ];
        let MatchOutcome::Matched(maps) = match_by_name(&form, &columns) else {
            panic!("前缀列应匹配")
        };
        assert_eq!(maps[0].bitable_field, "prefixed");
        assert_eq!(maps[1].bitable_field, "amount");
        let mut duplicate = columns.clone();
        duplicate.push(column("明细_费用明细/Expense details", "duplicate", &[]));
        assert!(
            matches!(match_by_name(&form, &duplicate), MatchOutcome::Invalid(errors) if errors.iter().any(|e| matches!(e, MatchError::AmbiguousColumn { .. })))
        );
    }

    #[test]
    fn non_cascading_system_options_are_recognized_but_third_party_urls_are_not() {
        let mut widget = linked_radio("选项", "w");
        for url in [
            "https://example.test/api/v1/feishu/approval/options/col_3",
            "http://localhost:8080/api/v1/feishu/approval/options/fld_x-1",
        ] {
            widget.external_data =
                Some(serde_json::json!({"externalDataLinkage": false, "externalUrl": url}));
            assert!(widget.links_to_our_options());
            assert!(matches!(
                match_by_name(&[widget.clone()], &[column("选项", "f", &[])]),
                MatchOutcome::Matched(_)
            ));
        }
        for url in [
            "https://third.test/options/x",
            "invalid",
            "https://example.test/api/v1/feishu/approval/options/",
            "https://example.test/api/v1/feishu/approval/options/a/b",
            "ftp://example.test/api/v1/feishu/approval/options/x",
        ] {
            widget.external_data =
                Some(serde_json::json!({"externalDataLinkage": true, "externalUrl": url}));
            assert!(!widget.links_to_our_options(), "{url}");
        }
    }

    #[test]
    fn nested_detail_children_are_matched_by_name() {
        // 明细本身不落映射，由定义快照恢复结构；子控件按名字匹配。
        let form = vec![detail("付款明细/Payment Details", "w_detail")];
        let columns = vec![
            column("付款金额/Payment Amount", "fldAmt", &[]),
            column("费用明细/Expense details", "fldDesc", &[]),
        ];
        match match_by_name(&form, &columns) {
            MatchOutcome::Matched(maps) => {
                // `费用明细` 落 `input`（direct），`付款金额` 落 `number`（direct）
                // ——**列表无序**（按递归顺序），所以按 id 取而不是按下标。
                assert_eq!(maps.len(), 2, "两个子控件都要落: {:?}", maps);
                let amount = maps
                    .iter()
                    .find(|map| map.widget_id == "child2")
                    .unwrap_or_else(|| panic!("子控件应落在映射里: {:?}", maps));
                assert_eq!(amount.bitable_field, "fldAmt");
                assert_eq!(amount.converter, Converter::Direct);
            }
            MatchOutcome::Invalid(errors) => panic!(
                "应成功，实际: {:?}",
                errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()
            ),
        }
    }

    #[test]
    fn detail_without_required_children_columns_fails_whole_config() {
        // 明细的两个子控件都必填且都没有列 → 整个配置失败。
        let form = vec![detail("付款明细/Payment Details", "w_detail")];
        match match_by_name(&form, &[]) {
            MatchOutcome::Matched(_) => panic!("必填子控件缺列应失败"),
            MatchOutcome::Invalid(errors) => assert_eq!(errors.len(), 2),
        }
    }

    #[test]
    fn duplicate_column_name_blocks_the_whole_match() {
        // 多维表格**允许列名重复**，而 API 按名字取值会拿到不确定的一列。
        // 判据放在 `find_column`：一旦同名超过一列，该控件直接失败
        // ——症状是「取到错的列」，比「取不到」难查得多，所以宁可整条拒绝。
        let form = vec![fixed_radio("收款方类型/Type of payee", "w1")];
        let columns = vec![
            column("收款方类型/Type of payee", "fldAAA", &["个人", "企业"]),
            column("收款方类型/Type of payee", "fldBBB", &["个人", "企业"]),
        ];
        match match_by_name(&form, &columns) {
            MatchOutcome::Matched(_) => panic!("重名列应失败"),
            MatchOutcome::Invalid(errors) => {
                assert!(
                    errors[0].to_string().contains("在表内出现 2 次"),
                    "{:?}",
                    errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()
                );
            }
        }
    }

    #[test]
    fn column_reused_by_two_widgets_is_rejected() {
        // 两条控件各对上**不同名字**但指向同一 `field_id` 的列——真实数据里
        // 这不会发生（同一 field_id 只有一个名字），但列名被改过、映射表是旧的
        // 时会出现，所以仍然拦：一列只能服务一个控件。
        let form = vec![
            fixed_radio("收款方类型/Type of payee", "w1"),
            fixed_radio("公司名称/Company name", "w2"),
        ];
        let columns = vec![
            column("收款方类型/Type of payee", "fldSHARED", &["个人", "企业"]),
            column("公司名称/Company name", "fldSHARED", &["个人", "企业"]),
        ];
        match match_by_name(&form, &columns) {
            MatchOutcome::Matched(_) => panic!("同一 field_id 被两控件复用应失败"),
            MatchOutcome::Invalid(errors) => {
                assert!(
                    errors.iter().any(|e| e.to_string().contains("同时被控件")),
                    "{:?}",
                    errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()
                );
            }
        }
    }

    #[test]
    fn required_select_without_any_column_options_is_rejected() {
        // `办公地点` 这种：控件是单选、`option` 是空数组、也不链接外部数据源
        // ——派生不出值。必填时报错。
        let form = vec![FormWidget {
            id: "w1".to_string(),
            name: "办公地点(Office location)".to_string(),
            r#type: "radioV2".to_string(),
            required: true,
            option: Some(WidgetOptions::Fixed(vec![])),
            external_data: Some(serde_json::json!({"externalDataLinkage": false, "key": ""})),
            value: serde_json::Value::Null,
            children: Vec::new(),
        }];
        let columns = vec![column(
            "办公地点(Office location)",
            "fldA",
            &["成都", "深圳"],
        )];
        match match_by_name(&form, &columns) {
            MatchOutcome::Matched(_) => panic!("派生不出选项应失败"),
            MatchOutcome::Invalid(errors) => {
                assert!(errors[0].to_string().contains("选项无法从列里派生"));
            }
        }
    }

    #[test]
    fn linked_external_options_are_left_for_the_caller() {
        // 链接本系统数据源的控件**不**报「派生不出」——它的 option_map 由
        // 调用方从 `feishu_option` 补。
        let form = vec![linked_radio("公司名称/Company name", "w1")];
        let columns = vec![column("公司名称/Company name", "fldA", &[])];
        match match_by_name(&form, &columns) {
            MatchOutcome::Matched(maps) => {
                assert_eq!(maps.len(), 1);
                assert!(maps[0].option_map.is_empty(), "留给调用方补");
            }
            MatchOutcome::Invalid(errors) => panic!("链接态不应报派生失败: {errors:?}"),
        }
    }

    #[test]
    fn multi_select_column_cannot_map_to_a_single_select_control() {
        // 一条记录可能带多个附件类型，radioV2 只能装一个；配置期就拦，
        // 不要留到派发期报「值的格式不符合审批控件要求」。
        let form = vec![linked_radio("可提供的附件类型", "w1")];
        let columns = vec![multi_column(
            "可提供的附件类型",
            "fldA",
            &["付款申请单", "发票"],
        )];
        match match_by_name(&form, &columns) {
            MatchOutcome::Matched(_) => panic!("多选列填单选控件应失败"),
            MatchOutcome::Invalid(errors) => assert!(
                errors
                    .iter()
                    .any(|error| matches!(error, MatchError::MultiValueColumn { .. })),
                "{:?}",
                errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()
            ),
        }
    }

    #[test]
    fn multi_select_column_cannot_map_to_a_text_input_either() {
        // input 走 Direct 转换，`text_from_cell` 会静默取数组第一个元素；
        // 不拦的话其余标签会被悄悄丢掉。
        let form = vec![input("可提供的附件类型", "w1", true)];
        let columns = vec![multi_column(
            "可提供的附件类型",
            "fldA",
            &["付款申请单", "发票"],
        )];
        match match_by_name(&form, &columns) {
            MatchOutcome::Matched(_) => panic!("多选列填文本控件应失败"),
            MatchOutcome::Invalid(errors) => assert!(
                errors
                    .iter()
                    .any(|error| matches!(error, MatchError::MultiValueColumn { .. })),
                "{:?}",
                errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()
            ),
        }
    }

    #[test]
    fn multi_select_column_maps_to_a_checkbox_control() {
        // 同样的多选列 + 外部选项，映射到 checkboxV2 是合法的；转换器是 Option。
        let form = vec![linked_checkbox("可提供的附件类型", "w1")];
        let columns = vec![multi_column(
            "可提供的附件类型",
            "fldA",
            &["付款申请单", "发票"],
        )];
        match match_by_name(&form, &columns) {
            MatchOutcome::Matched(maps) => {
                assert_eq!(maps.len(), 1);
                assert_eq!(maps[0].widget_type, "checkboxV2");
                assert_eq!(maps[0].converter, Converter::Option);
            }
            MatchOutcome::Invalid(errors) => panic!("多选控件应接受多选列: {errors:?}"),
        }
    }

    #[test]
    fn non_submittable_required_widget_is_ignored() {
        let form = vec![FormWidget {
            id: "w1".to_string(),
            name: "流水号".to_string(),
            r#type: "serialNumber".to_string(),
            required: true,
            option: None,
            external_data: None,
            value: serde_json::Value::Null,
            children: Vec::new(),
        }];
        assert!(
            matches!(match_by_name(&form, &[]), MatchOutcome::Matched(maps) if maps.is_empty())
        );
    }

    #[test]
    fn required_formula_maps_to_the_same_named_numeric_column() {
        // 公式是可提单控件（官方清单），同名数字列存在时必须进映射，
        // 否则派发期会把它当成「必填却无映射」而整单失败。
        let mut total = input("合计付款金额/Payment amount", "total", true);
        total.r#type = "formula".to_string();
        let form = vec![total, input("备注", "note", false)];
        let columns = [
            column("合计付款金额/Payment amount", "f1", &[]),
            column("备注", "f2", &[]),
        ];
        let MatchOutcome::Matched(maps) = match_by_name(&form, &columns) else {
            panic!("公式控件应匹配到同名数字列")
        };
        assert_eq!(maps.len(), 2);
        let total = maps
            .iter()
            .find(|m| m.widget_id == "total")
            .unwrap_or_else(|| panic!("映射里应有公式控件"));
        assert_eq!(total.widget_type, "formula");
        assert_eq!(total.bitable_field, "f1");
        assert!(total.required);
    }

    #[test]
    fn option_derivation_only_trims_whitespace_not_semantic_suffixes() {
        // 列选项名带 `*` 之类语义尾缀时：trim **不会**去掉它，于是派不出值。
        // 必填控件因此报错——这正是使用方要的「配置期就报错」（列里写的是
        // 「个人 *」而定义里是「个人」，不一致必须在配表时暴露）。
        let form = vec![fixed_radio("收款方类型/Type of payee", "w1")];
        let columns = vec![column(
            "收款方类型/Type of payee",
            "fldAAA",
            &["个人 *", "别的选项"],
        )];
        match match_by_name(&form, &columns) {
            MatchOutcome::Matched(maps) => panic!("语义尾缀对不上应报错: {:?}", maps),
            MatchOutcome::Invalid(errors) => {
                assert!(
                    errors[0].to_string().contains("选项无法从列里派生"),
                    "{errors:?}"
                );
            }
        }
    }

    #[test]
    fn optional_select_with_no_derivable_options_is_accepted() {
        // 非必填的单选：派不出值也不必报——留空提交合法（官方允许不传非必填控件）。
        let mut form = fixed_radio("收款方类型/Type of payee", "w1");
        form.required = false;
        let columns = vec![column(
            "收款方类型/Type of payee",
            "fldAAA",
            &["对不上的选项"],
        )];
        match match_by_name(&[form], &columns) {
            MatchOutcome::Matched(maps) => assert!(maps[0].option_map.is_empty()),
            MatchOutcome::Invalid(errors) => panic!("非必填报错: {errors:?}"),
        }
    }

    #[test]
    fn group_value_children_are_parsed_filtered_and_ordered() {
        // 控件组的子控件在 `value` 数组里（不是 `children`）：只留白名单内
        // 且可见的，并按白名单顺序排序。
        let leave: FormWidget = serde_json::from_value(serde_json::json!({
            "id": "leave",
            "name": "请假",
            "type": "leaveGroupV2",
            "required": true,
            "value": [
                {"id": "widgetLeaveGroupEndTime", "name": "结束时间", "type": "date"},
                {"id": "widgetLeaveGroupReason", "name": "请假事由", "type": "textarea", "visible": false},
                {"id": "widgetLeaveGroupType", "name": "请假类型", "type": "radioV2"},
                {"id": "widgetOutOfWhitelist", "name": "说明", "type": "text"},
                {"id": "widgetLeaveGroupStartTime", "name": "开始时间", "type": "date"}
            ]
        }))
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            leave
                .children
                .iter()
                .map(|child| child.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "widgetLeaveGroupType",
                "widgetLeaveGroupStartTime",
                "widgetLeaveGroupEndTime"
            ]
        );
        assert_eq!(leave.children[0].name, "请假类型");
        assert_eq!(leave.children[0].r#type, "radioV2");

        // `value` 不是数组（空对象 / null）时没有子控件。
        for value in [serde_json::json!({}), serde_json::Value::Null] {
            let work: FormWidget = serde_json::from_value(serde_json::json!({
                "id": "work",
                "name": "加班",
                "type": "workGroup",
                "value": value
            }))
            .unwrap_or_else(|error| panic!("{error}"));
            assert!(work.children.is_empty(), "{value}");
        }
    }

    #[test]
    fn group_parents_are_previewed_but_not_mapped() {
        // 控件组父级只作分组展示，子控件才参与列映射。
        let form: Vec<FormWidget> = serde_json::from_value(serde_json::json!([{
            "id": "out",
            "name": "外出",
            "type": "outGroup",
            "required": true,
            "value": [
                {"id": "widgetOutGroupType", "name": "外出类型", "type": "radioV2", "required": true},
                {"id": "widgetOutGroupStartTime", "name": "开始时间", "type": "date"}
            ]
        }]))
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            preview_widgets(&form)
                .iter()
                .map(|(widget, name)| (widget.id.as_str(), name.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("out", "外出"),
                ("widgetOutGroupType", "外出_外出类型"),
                ("widgetOutGroupStartTime", "外出_开始时间"),
            ]
        );
        assert_eq!(
            mapping_widgets(&form)
                .iter()
                .map(|(widget, _)| widget.id.as_str())
                .collect::<Vec<_>>(),
            vec!["widgetOutGroupType", "widgetOutGroupStartTime"]
        );
    }

    #[test]
    fn raw_option_group_children_use_direct_converter() {
        // 假勤类型子控件的 `option` 可能为空、值是飞书类型 ID：直取，
        // 不报「选项无法从列里派生」。
        let form: Vec<FormWidget> = serde_json::from_value(serde_json::json!([{
            "id": "leave",
            "name": "请假",
            "type": "leaveGroupV2",
            "required": true,
            "value": [
                {"id": "widgetLeaveGroupType", "name": "请假类型", "type": "radioV2", "required": true, "option": []}
            ]
        }]))
        .unwrap_or_else(|error| panic!("{error}"));
        let columns = vec![column("请假类型", "f1", &["年假"])];
        let MatchOutcome::Matched(maps) = match_by_name(&form, &columns) else {
            panic!("类型 ID 子控件应可直取");
        };
        assert_eq!(maps.len(), 1);
        assert_eq!(maps[0].widget_id, "widgetLeaveGroupType");
        assert_eq!(maps[0].converter, Converter::Direct);
        assert!(maps[0].option_map.is_empty());

        // 普通无固定选项的单选仍然报 UnresolvableOptions。
        let mut plain = input("普通单选", "plain", true);
        plain.r#type = "radioV2".to_string();
        plain.option = Some(WidgetOptions::Fixed(vec![]));
        match match_by_name(&[plain], &[column("普通单选", "f2", &["年假"])]) {
            MatchOutcome::Invalid(errors) => assert!(
                errors
                    .iter()
                    .any(|error| matches!(error, MatchError::UnresolvableOptions { .. })),
                "{errors:?}"
            ),
            MatchOutcome::Matched(maps) => panic!("普通单选应报派生失败: {maps:?}"),
        }
    }

    #[test]
    fn account_and_old_group_names_are_unsupported() {
        // 官方清单里的 account；旧控件组名 leaveGroup / remedyGroup；
        // 以及官方支持但本系统未实现子控件映射的 shiftGroup。
        for kind in ["account", "remedyGroup", "leaveGroup", "shiftGroup"] {
            assert!(widget_rules::is_unsupported(kind), "{kind}");
            assert!(!is_configurable_control(kind), "{kind} 不应出现在配置里");
            assert!(!is_eligible_for_mapping(kind), "{kind} 不应进映射表");
            let widget: FormWidget = serde_json::from_value(serde_json::json!({
                "id": "w",
                "name": "控件",
                "type": kind,
                "required": true
            }))
            .unwrap_or_else(|error| panic!("{error}"));
            assert!(
                preview_widgets(std::slice::from_ref(&widget)).is_empty(),
                "{kind} 不应出现在配置预览里"
            );
        }
        assert!(
            widget_rules::unsupported_reason("shiftGroup").contains("暂未支持该控件组的子控件映射")
        );
        assert_eq!(
            widget_rules::unsupported_reason("leaveGroup"),
            "该控件组版本不在官方实例 API 支持范围内"
        );
        assert_eq!(
            widget_rules::unsupported_reason("account"),
            "官方实例 API 不支持该控件"
        );
        // 控件组父级可预览但不进映射；图片已是可映射控件。
        for kind in ["leaveGroupV2", "workGroup", "outGroup"] {
            assert!(is_configurable_control(kind), "{kind}");
            assert!(!is_eligible_for_mapping(kind), "容器不进映射: {kind}");
            let widget: FormWidget = serde_json::from_value(serde_json::json!({
                "id": "w",
                "name": "控件",
                "type": kind
            }))
            .unwrap_or_else(|error| panic!("{error}"));
            assert!(
                !preview_widgets(std::slice::from_ref(&widget)).is_empty(),
                "{kind} 应可预览"
            );
        }
        assert!(is_configurable_control("image") && is_eligible_for_mapping("image"));
    }

    #[test]
    fn required_config_reject_widgets_fail_at_config_time() {
        for kind in [
            "account",
            "remedyGroup",
            "remedyGroupV2",
            "leaveGroup",
            "shiftGroup",
            "shiftGroupV2",
        ] {
            let required: FormWidget = serde_json::from_value(serde_json::json!({
                "id": "w", "name": "控件", "type": kind, "required": true
            }))
            .unwrap_or_else(|error| panic!("{error}"));
            match match_by_name(std::slice::from_ref(&required), &[]) {
                MatchOutcome::Invalid(errors) => assert!(
                    errors
                        .iter()
                        .any(|error| matches!(error, MatchError::UnsupportedWidget { .. })),
                    "{kind}: {errors:?}"
                ),
                MatchOutcome::Matched(maps) => {
                    panic!("{kind} 必填应在配置期被拦下，实际 {maps:?}")
                }
            }

            // 可选时保持既有语义：不展示、不映射、不报错。
            let optional: FormWidget = serde_json::from_value(serde_json::json!({
                "id": "w", "name": "控件", "type": kind
            }))
            .unwrap_or_else(|error| panic!("{error}"));
            assert!(
                matches!(
                    match_by_name(std::slice::from_ref(&optional), &[]),
                    MatchOutcome::Matched(ref maps) if maps.is_empty()
                ),
                "{kind} 可选时不应阻断配置"
            );
        }
    }

    #[test]
    fn raw_option_child_with_fixed_options_still_needs_option_mapping() {
        let form: Vec<FormWidget> = serde_json::from_value(serde_json::json!([{
            "id": "work",
            "name": "加班",
            "type": "workGroup",
            "required": true,
            "value": [
                {
                    "id": "widgetWorkGroupType",
                    "name": "加班类型",
                    "type": "radioV2",
                    "required": true,
                    "option": [{"text": "调休", "value": "LEAVE"}]
                }
            ]
        }]))
        .unwrap_or_else(|error| panic!("{error}"));

        // 列里没有同名选项：即使显式映射也必须报错，不能存成 Option + 空 map。
        let columns = vec![column("加班类型", "f1", &["别的"])];
        match match_fields(
            &form,
            &columns,
            Some(&[mapping("widgetWorkGroupType", "f1")]),
        ) {
            MatchOutcome::Invalid(errors) => assert!(
                errors
                    .iter()
                    .any(|error| matches!(error, MatchError::UnresolvableOptions { .. })),
                "{errors:?}"
            ),
            MatchOutcome::Matched(maps) => panic!("固定选项未命中应报错: {maps:?}"),
        }

        // 命中时仍走 Option，并把选项值带出来。
        let columns = vec![column("加班类型", "f1", &["调休"])];
        let MatchOutcome::Matched(maps) = match_fields(
            &form,
            &columns,
            Some(&[mapping("widgetWorkGroupType", "f1")]),
        ) else {
            panic!("固定选项命中应通过");
        };
        assert_eq!(maps[0].converter, Converter::Option);
        assert_eq!(
            maps[0].option_map.get("调休").map(String::as_str),
            Some("LEAVE")
        );
    }

    #[test]
    fn work_group_required_flags_follow_rule_and_time_range() {
        let parse = |is_set_rule: serde_json::Value| -> FormWidget {
            serde_json::from_value(serde_json::json!({
                "id": "work",
                "name": "加班",
                "type": "workGroup",
                "required": true,
                "option": {"isSetRule": is_set_rule},
                "value": [
                    {
                        "id": "widgetWorkGroupType",
                        "name": "加班类型",
                        "type": "radioV2",
                        "required": true,
                        "option": []
                    },
                    {
                        "id": "widgetWorkGroupTimeRangeFieldList",
                        "name": "加班时段",
                        "type": "fieldList",
                        "required": false,
                        "children": [
                            {"id": "start", "name": "开始", "type": "date", "required": true},
                            {"id": "end", "name": "结束", "type": "date", "required": true}
                        ]
                    }
                ]
            }))
            .unwrap_or_else(|error| panic!("{error}"))
        };

        let linked = parse(serde_json::json!(1));
        let type_child = linked
            .children
            .iter()
            .find(|child| child.id == "widgetWorkGroupType")
            .unwrap_or_else(|| panic!("缺少加班类型子控件"));
        assert!(!type_child.required, "关联加班规则时类型不需要填写");
        let range = linked
            .children
            .iter()
            .find(|child| child.id == "widgetWorkGroupTimeRangeFieldList")
            .unwrap_or_else(|| panic!("缺少加班时段子控件"));
        assert!(range.required, "加班时段至少一段");

        let custom = parse(serde_json::json!(0));
        let type_child = custom
            .children
            .iter()
            .find(|child| child.id == "widgetWorkGroupType")
            .unwrap_or_else(|| panic!("缺少加班类型子控件"));
        assert!(type_child.required, "未关联规则时类型按定义必填");
    }

    #[test]
    fn empty_group_name_falls_back_to_type_label() {
        let form: Vec<FormWidget> = serde_json::from_value(serde_json::json!([{
            "id": "leave",
            "name": "",
            "type": "leaveGroupV2",
            "value": [
                {"id": "widgetLeaveGroupType", "name": "假期类型", "type": "radioV2"}
            ]
        }]))
        .unwrap_or_else(|error| panic!("{error}"));
        let preview = preview_widgets(&form);
        assert_eq!(preview[0].1, "请假控件组");
        assert_eq!(preview[1].1, "请假控件组_假期类型");
    }

    #[test]
    fn required_image_is_mappable() {
        let mut image = input("图片", "img", true);
        image.r#type = "image".to_string();
        let columns = vec![column("图片", "f1", &[])];
        let MatchOutcome::Matched(maps) = match_by_name(&[image], &columns) else {
            panic!("图片应可映射");
        };
        assert_eq!(maps[0].widget_type, "image");
        assert_eq!(maps[0].converter, Converter::Direct);
    }

    #[test]
    fn string_group_value_is_parsed() {
        let form: Vec<FormWidget> = serde_json::from_value(serde_json::json!([{
            "id": "leave",
            "type": "leaveGroupV2",
            "value": "[{\"id\":\"widgetLeaveGroupType\",\"type\":\"radioV2\"}]"
        }]))
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(form[0].children.len(), 1);
        assert_eq!(form[0].children[0].id, "widgetLeaveGroupType");
    }
}

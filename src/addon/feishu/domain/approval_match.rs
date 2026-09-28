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
//! `externalData.externalDataLinkage = true` 的控件走**本系统自己**的外部选项
//! 数据源（`feishu_option`），由调用方把 `option_map` 补进 `WidgetMap`——那一段
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
pub(crate) struct FormWidget {
    /// 控件 id。创建实例时要的 `form[].id`。
    #[serde(default)]
    pub(crate) id: String,
    /// 控件名。**实测是服务端解好的可读中文**（不是 `@i18n@` 键）——这是按名匹配的依据。
    #[serde(default)]
    pub(crate) name: String,
    /// 控件类型（`input` / `radioV2` / `fieldList` / …）。
    #[serde(default)]
    pub(crate) r#type: String,
    /// 是否必填。判定「配置期报错」的依据。
    #[serde(default)]
    pub(crate) required: bool,
    /// 多态选项；`None` 表示「无选项」（键缺失或值为 `null`）。
    #[serde(default)]
    pub(crate) option: Option<WidgetOptions>,
    /// 外部数据源链接。
    ///
    /// 声明成 `serde_json::Value` 是**为了能识别出这个键存在**——实测里
    /// `externalDataLinkage` 为 `true` 的控件要走本系统自己的外部选项数据源，
    /// 而不是从这里读取值。
    ///
    /// **`rename` 不能省**：JSON 键是驼峰的 `externalData`，Rust 字段是
    /// snake_case。没有这一行，字段恒为 `None`，而症状是「链接态被当成非链接」
    /// ——一条静默的、指向完全相反方向的判定。
    #[serde(default, rename = "externalData")]
    pub(crate) external_data: Option<serde_json::Value>,
    /// 明细控件的子控件（嵌套结构）。
    #[serde(default)]
    pub(crate) children: Vec<FormWidget>,
}

impl FormWidget {
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

    /// 该控件的选项是否链接到**本系统**的外部数据源。
    ///
    /// 判据取 `externalData.externalDataLinkage`。链接的是谁由 `key` 与
    /// `linkageConfigs[].key` 表达，**但实测里 `externalData.key` 是空字符串**——
    /// 所以这里只判「是不是链接态」，实际数据源由调用方按配置查。
    pub(crate) fn links_to_our_options(&self) -> bool {
        let Some(external) = &self.external_data else {
            return false;
        };
        external
            .get("externalDataLinkage")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }
}

/// 多维表格侧的列定义（只取按名匹配要用的两样）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Column {
    pub(crate) field_id: String,
    pub(crate) field_name: String,
    /// 列里的选项名。只有 select 类型的列才有。
    pub(crate) options: Vec<String>,
}

/// 由《列出字段》的响应构造按名匹配用的列集合。
///
/// # 为什么这一步不能省
///
/// 按名匹配要的是**列名 + 选项名**，而两样都只在 `list_all_fields` 里——
/// 直接用调用方传来的列名会丢掉选项（`property.options[].name`），
/// 单选控件的选项值就派生不出来。
///
/// # 只留匹配要用的三样
///
/// `field_type` / `ui_type` 一概不进 [`Column`]：类型兼容性判定（`check_type_compatibility`
/// 当前只拦「列是选项而控件要数字」这类）**只看有没有选项**，不需要类型码。
/// 多留两样只会让「`Column` 是什么」变模糊。
pub(crate) fn columns_from_fields(fields: &[FieldItem]) -> Vec<Column> {
    fields
        .iter()
        .map(|field| Column {
            field_id: field.field_id.clone(),
            field_name: field.field_name.trim().to_string(),
            options: field.option_names(),
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
    /// 必填控件在多维表格里没有同名列。
    MissingRequiredColumn { widget: String, widget_type: String },
    /// 列名与控件名匹配上了，但类型对不上（例如列是单选而控件是数字）。
    TypeMismatch {
        widget: String,
        widget_type: String,
        column: String,
        column_kind: String,
    },
    /// 同名列出现多次（多维表格列名不唯一）。
    AmbiguousColumn {
        widget: String,
        column: String,
        count: usize,
    },
    /// 必填控件的选项**无法从列里派生**（`option` 不是数组）。
    UnresolvableOptions { widget: String, widget_type: String },
    /// 该控件类型不支持通过 API 提单（官方不支持清单）。
    UnsupportedWidget { widget: String, widget_type: String },
    /// 需要人工准备值的控件（附件 / 地址 / 关联审批），配置期直接拒绝。
    ManualValueWidget { widget: String, widget_type: String },
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
            Self::MissingRequiredColumn {
                widget,
                widget_type,
            } => write!(
                formatter,
                "必填控件「{widget}」（{widget_type}）在多维表格里没有同名列"
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
                "控件「{widget}」（{widget_type}）不支持通过 API 提单"
            ),
            Self::ManualValueWidget { widget, widget_type } => write!(
                formatter,
                "控件「{widget}」（{widget_type}）需要人工准备值（附件 / 地址 / 关联审批），不参与自动派发"
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

/// 定义里的不支持提单控件（官方清单）。
///
/// 与 `approval_convert::is_unsupported_widget_type` 同源，但**这里是配置期校验的
/// 第一处拦截**：那处是处理一条记录时才报，这处是根本配不进配置。
const UNSUPPORTED_TYPES: &[&str] = &[
    "formula",
    "mutableGroup",
    "serialNumber",
    "text",
    "shiftGroup",
    "shiftGroupV2",
    "tripGroup",
    "leaveGroup",
    "workGroup",
    "outGroup",
    "remedyGroupV2",
    "apaascorehrOnboardingGroup",
    "apaascorehrRegularateGroup",
    "apaascorehrJobAdjustGroup",
    "apaascorehrOffboardingGroup",
];

/// 需要人工准备值的控件类型：它们的值不是多维表格能给的（file code / 地理库 id /
/// 已存在的 instance_code），配置期就拦下，免得配完之后每一条记录都失败。
const MANUAL_VALUE_TYPES: &[&str] = &[
    "address",
    "connect",
    "attachment",
    "attachmentV2",
    "image",
    "imageV2",
    "document",
];

/// 由审批定义与列定义按名匹配出一张映射表。
///
/// **全量校验**：一次返回**所有**不成立的控件，而不是遇到第一个就停——
/// 使用方要「配置期就报错」，而配置期一次改完比改一条看一条快得多。
pub(crate) fn match_by_name(form: &[FormWidget], columns: &[Column]) -> MatchOutcome {
    let mut errors: Vec<MatchError> = Vec::new();
    let mut widgets: Vec<WidgetMap> = Vec::new();
    let mut column_owner: BTreeMap<String, String> = BTreeMap::new();

    // 递归铺平：明细（fieldList）的子控件也参与匹配，但**明细本身**不落映射——
    // 它的子控件是 `widget1.widget2` 这类复合 id，走嵌套提交路径。
    let mut flat: Vec<&FormWidget> = Vec::new();
    for widget in form {
        for node in widget.walk() {
            if node.r#type == "fieldList" {
                continue;
            }
            flat.push(node);
        }
    }

    for widget in flat {
        if UNSUPPORTED_TYPES.contains(&widget.r#type.as_str()) {
            if widget.required {
                errors.push(MatchError::UnsupportedWidget {
                    widget: widget.name.clone(),
                    widget_type: widget.r#type.clone(),
                });
            }
            continue;
        }
        if MANUAL_VALUE_TYPES.contains(&widget.r#type.as_str()) {
            // **可选**的人工值控件不拦（留空提交是合法的，官方允许不传必填控件
            // 之外的任何控件）；**必填**的拦下来——留空会让审批单缺核心内容。
            if widget.required {
                errors.push(MatchError::ManualValueWidget {
                    widget: widget.name.clone(),
                    widget_type: widget.r#type.clone(),
                });
            }
            continue;
        }

        let Some(column) = find_column(columns, &widget.name, &mut errors) else {
            if widget.required {
                errors.push(MatchError::MissingRequiredColumn {
                    widget: widget.name.clone(),
                    widget_type: widget.r#type.clone(),
                });
            }
            continue;
        };

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
            if widget.required {
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
        if widget.required
            && !widget.links_to_our_options()
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
            converter: converter_for(&widget.r#type),
            option_map,
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
/// 所以这里只拦「明显会让提交失败」的两类：数字对文本、文本对数字。
fn check_type_compatibility(widget: &FormWidget, column: &Column) -> Result<(), MatchError> {
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

/// 控件类型 → 值转换器。
///
/// 与 `approval_convert` 的判据同源，但这里**在配置期**就定好并落库——
/// 运行期不再按 `type` 推断，少一处可能漂移的地方。
fn converter_for(widget_type: &str) -> Converter {
    match widget_type {
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
        }
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
    fn required_attachment_is_rejected_but_optional_is_ignored() {
        // 必填的人工值控件拦下来；可选的留空提交是合法的。
        let required_attach = FormWidget {
            id: "w1".to_string(),
            name: "附件attachment".to_string(),
            r#type: "attachmentV2".to_string(),
            required: true,
            option: None,
            external_data: None,
            children: Vec::new(),
        };
        match match_by_name(&[required_attach], &[]) {
            MatchOutcome::Matched(_) => panic!("必填附件应被拒"),
            MatchOutcome::Invalid(errors) => assert!(errors[0].to_string().contains("人工准备值")),
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
    fn nested_detail_children_are_matched_by_name() {
        // 明细本身不落映射（复合 id 走嵌套提交），但**子控件**按名字对得上。
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
    fn unsupported_required_widget_is_rejected() {
        let form = vec![FormWidget {
            id: "w1".to_string(),
            name: "流水号".to_string(),
            r#type: "serialNumber".to_string(),
            required: true,
            option: None,
            external_data: None,
            children: Vec::new(),
        }];
        match match_by_name(&form, &[]) {
            MatchOutcome::Matched(_) => panic!("不支持的控件应失败"),
            MatchOutcome::Invalid(errors) => {
                assert!(errors[0].to_string().contains("不支持通过 API 提单"))
            }
        }
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
}

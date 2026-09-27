//! 审批表单控件值的组装。
//!
//! # 为什么不能照搬审批定义的 `form`
//!
//! 「查看指定审批定义」返回的 `form` **不是创建实例可直接提交的模板**。它的用途是
//! 识别控件 `id`/`type`、选项值范围与明细子控件结构；`instances create` 的
//! `form[].value` 必须按控件类型单独组装。两者的字段集与嵌套形态都不同。
//!
//! # `form` 是**字符串**不是对象
//!
//! 创建实例接口要求 `form` 是「JSON 数组，传值时需要压缩转义为字符串」
//! （`instance/create.md:30`）。所以本模块的产物是 `String`，不是 `serde_json::Value`。
//!
//! # 不自动准备的值
//!
//! 三类值需要人工提供凭据，本模块**明确报错**而不静默传空（静默传空会让飞书侧
//! 收到一个看似合法的空控件，错误信息指向飞书而不是指向配置）：
//!
//! - `address` 的地理库 `id`；
//! - `connect` 的关联审批 `instance_code`；
//! - `attachmentV2` / `image` / `imageV2` 的 file code。

#![allow(dead_code)] // 转换器先落地并自带测试；消费者（派发编排）在后续任务接入。

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

/// 一个审批控件在库里的映射快照。
#[derive(Debug, Clone)]
pub(crate) struct WidgetMap {
    /// 审批控件 id（来自审批定义 form[].id）。
    pub(crate) widget_id: String,
    /// 审批控件类型（`input` / `date` / `radioV2` / …）。
    pub(crate) widget_type: String,
    /// 该控件是否必填（快照）。用于数据完整性判定，不用于能否组装的判定。
    pub(crate) required: bool,
    /// 多维表格字段 id（取值来源）。
    pub(crate) bitable_field: String,
    /// 值转换器。
    pub(crate) converter: Converter,
    /// 单选/多选的选项映射：多维表格文案 → 审批控件 option value。
    pub(crate) option_map: BTreeMap<String, String>,
    /// 金额控件的币种（`amount` 专用）。
    pub(crate) currency: Option<String>,
}

/// 值转换器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Converter {
    /// 文本/数字类直取。
    Direct,
    /// 多维表格毫秒时间戳 → 带偏移量的 RFC3339。
    Date,
    /// 多维表格选项文案 → 审批控件 option value。
    Option,
}

impl Converter {
    /// 从库里的字符串恢复。未知值 fail-closed——静默退化成 `Direct` 会把
    /// 日期或选项值原样送出去，表现为飞书侧的格式错误，排查方向完全跑偏。
    pub(crate) fn parse(value: &str) -> Result<Self, ConvertError> {
        match value {
            "direct" => Ok(Self::Direct),
            "date" => Ok(Self::Date),
            "option" => Ok(Self::Option),
            other => Err(ConvertError::UnknownConverter(other.to_string())),
        }
    }
}

/// 组装失败的原因。
///
/// 每条都带**可行动的定位信息**（控件 id 或多维表格字段 id），因为组装失败的
/// 处置是把原因写回多维表格——那张表的读者是业务人员，不是本服务的运维。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConvertError {
    /// 库里存了未知的转换器标识。
    UnknownConverter(String),
    /// 必填控件在多维表格里取不到值。
    MissingRequired { widget_id: String },
    /// 值存在但形态不对（例如日期字段拿到了非数字）。
    BadValue { widget_id: String, reason: String },
    /// 该控件类型无法通过 API 提单，需要人工准备值。
    Unsupported {
        widget_id: String,
        widget_type: String,
    },
    /// 选项文案在映射表里查不到。
    UnmappedOption { widget_id: String, label: String },
    /// 转换器与控件类型不匹配（例如给 `date` 控件配了 `direct`）。
    ///
    /// 这类配置错误**必须显式拦下**：静默按转换器走会把原始毫秒时间戳当字符串
    /// 发给飞书，报回来的是 1390001（控件参数错误），排查方向指向飞书而不是
    /// 指向这份配置。
    ConverterMismatch {
        widget_id: String,
        widget_type: String,
        converter: &'static str,
    },
}

impl std::fmt::Display for ConvertError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownConverter(value) => {
                write!(formatter, "未知的值转换器「{value}」")
            }
            Self::MissingRequired { widget_id } => {
                write!(formatter, "缺少必填控件「{widget_id}」的值")
            }
            Self::BadValue { widget_id, reason } => {
                write!(formatter, "控件「{widget_id}」的值不可用：{reason}")
            }
            Self::Unsupported {
                widget_id,
                widget_type,
            } => write!(
                formatter,
                "控件「{widget_id}」（{widget_type}）不支持通过 API 提单，需人工准备值"
            ),
            Self::UnmappedOption { widget_id, label } => write!(
                formatter,
                "控件「{widget_id}」的选项「{label}」没有配置映射"
            ),
            Self::ConverterMismatch {
                widget_id,
                widget_type,
                converter,
            } => write!(
                formatter,
                "控件「{widget_id}」（{widget_type}）与转换器「{converter}」不匹配：请把该映射的转换器改为对应的类型"
            ),
        }
    }
}

impl std::error::Error for ConvertError {}

/// 组装 `form` 所需的全部输入。
pub(crate) struct FormInput<'a> {
    pub(crate) widgets: &'a [WidgetMap],
    /// 多维表格记录的单元格值，键是字段 id。
    pub(crate) cells: &'a Map<String, Value>,
    /// 多维表格的时区（IANA 名），供 `Date` 转换器用。
    ///
    /// 多维表格的日期是**不带时区**的毫秒时间戳，而审批 `date` 控件要带偏移量的
    /// RFC3339。所以时区必须显式给定，不能猜——猜错会让审批里的时间整体偏移。
    pub(crate) timezone_offset: FixedOffset,
}

/// 固定的 UTC 偏移（秒）。只支持固定偏移，不支持夏令时。
///
/// 本部署的使用场景（境内业务）没有夏令时，用固定偏移换来「无需引入时区数据库」
/// 这个显著简化。若将来要服务有夏令时的地区，这个类型是替换点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FixedOffset {
    seconds: i32,
}

impl FixedOffset {
    /// 由秒数构造，范围校验为 ±18 小时（RFC3339 的上限）。
    pub(crate) fn from_seconds(seconds: i32) -> Result<Self, ConvertError> {
        const LIMIT: i32 = 18 * 3600;
        if seconds.abs() > LIMIT {
            return Err(ConvertError::BadValue {
                widget_id: String::new(),
                reason: format!("时区偏移 {seconds} 秒超出 ±18 小时"),
            });
        }
        Ok(Self { seconds })
    }

    /// 按 IANA 名构造。只认少数几个常用名——完整时区库不在本模块的责任范围。
    pub(crate) fn from_iana(name: &str) -> Result<Self, ConvertError> {
        match name {
            "Asia/Shanghai" | "Asia/Chongqing" | "Asia/Harbin" | "PRC" => {
                Self::from_seconds(8 * 3600)
            }
            "UTC" | "Etc/UTC" => Self::from_seconds(0),
            "Asia/Tokyo" => Self::from_seconds(9 * 3600),
            "Asia/Singapore" | "Asia/Hong_Kong" => Self::from_seconds(8 * 3600),
            other => Err(ConvertError::BadValue {
                widget_id: String::new(),
                reason: format!("不支持的 Base 时区「{other}」"),
            }),
        }
    }

    /// 渲染成 `+08:00` / `-05:30` 形态。
    fn render(self) -> String {
        let sign = if self.seconds < 0 { '-' } else { '+' };
        let total = self.seconds.unsigned_abs();
        format!("{sign}{:02}:{:02}", total / 3600, (total % 3600) / 60)
    }
}

/// 把一条多维表格记录组装成创建实例接口要求的 `form` 字符串。
///
/// 返回的是**压缩后的 JSON 数组字符串**（`instance/create.md:30`）。
pub(crate) fn build_form(input: &FormInput<'_>) -> Result<String, ConvertError> {
    let mut widgets = Vec::with_capacity(input.widgets.len());
    for widget in input.widgets {
        match assemble_widget(widget, input)? {
            // `None` 表示该控件在本记录上没有值。**必填的已经在 assemble 里报错**，
            // 所以到这里还能是 `None` 的只有可选控件——整个控件 JSON 都不传。
            //
            // 这是官方要求（`approval-related-faqs.md:86-89`）：不传必填控件时
            // 「该控件的完整 JSON 参数均不能传入」；一旦传入控件 JSON，就必须设
            // `value`，否则接口报错。
            None => {}
            Some(value) => widgets.push(value),
        }
    }

    serde_json::to_string(&Value::Array(widgets)).map_err(|error| ConvertError::BadValue {
        widget_id: String::new(),
        reason: format!("form 序列化失败：{error}"),
    })
}

/// 组装单个控件；无值且非必填时返回 `Ok(None)`。
fn assemble_widget(
    widget: &WidgetMap,
    input: &FormInput<'_>,
) -> Result<Option<Value>, ConvertError> {
    let cell = input.cells.get(&widget.bitable_field);

    // 先判「这个控件类型能不能提单」——它比有没有值更根本。含不支持控件的定义
    // 无论数据是否完整都提不出去，早点报错能让用户改定义而不是反复补数据。
    if is_unsupported_widget_type(&widget.widget_type) {
        return Err(ConvertError::Unsupported {
            widget_id: widget.widget_id.clone(),
            widget_type: widget.widget_type.clone(),
        });
    }

    let Some(cell) = cell.filter(|value| !is_empty(value)) else {
        return if widget.required {
            Err(ConvertError::MissingRequired {
                widget_id: widget.widget_id.clone(),
            })
        } else {
            Ok(None)
        };
    };

    check_converter_matches(widget)?;

    let value = match widget.converter {
        Converter::Direct => assemble_direct(widget, cell)?,
        Converter::Date => assemble_date(widget, cell, input.timezone_offset)?,
        Converter::Option => assemble_option(widget, cell)?,
    };

    let mut object = Map::new();
    object.insert("id".to_string(), Value::String(widget.widget_id.clone()));
    object.insert(
        "type".to_string(),
        Value::String(widget.widget_type.clone()),
    );
    object.insert("value".to_string(), value);
    // 金额控件的币种是 `value` 的**兄弟**字段，不在 value 里。
    if let Some(currency) = &widget.currency {
        object.insert("currency".to_string(), Value::String(currency.clone()));
    }
    Ok(Some(Value::Object(object)))
}

/// 转换器与控件类型必须匹配。
///
/// 这是配置期的错误，不是数据期的。放行的后果是原始毫秒时间戳被当字符串发给飞书，
/// 报回来 1390001（控件参数错误）——排查方向指向飞书而不是指向这份配置。
fn check_converter_matches(widget: &WidgetMap) -> Result<(), ConvertError> {
    let kind = widget.widget_type.as_str();
    let expected = match kind {
        "date" | "dateInterval" => Converter::Date,
        "radio" | "radioV2" | "checkbox" | "checkboxV2" => Converter::Option,
        _ => Converter::Direct,
    };
    if widget.converter == expected {
        return Ok(());
    }
    Err(ConvertError::ConverterMismatch {
        widget_id: widget.widget_id.clone(),
        widget_type: widget.widget_type.clone(),
        converter: match widget.converter {
            Converter::Direct => "direct",
            Converter::Date => "date",
            Converter::Option => "option",
        },
    })
}

/// 文本/数字类的直取。
fn assemble_direct(widget: &WidgetMap, cell: &Value) -> Result<Value, ConvertError> {
    match widget.widget_type.as_str() {
        // 数字与金额取 JSON 数字，**不能**是字符串——传字符串会被 1390001 拒掉。
        "number" | "amount" => number_from_cell(widget, cell),
        // 电话：多维表格存成文本，拆出区号与号码。
        "telephone" => assemble_telephone(widget, cell),
        // 联系人与部门：多维表格的人员字段给 open_id 数组。
        "contact" => Ok(json!({ "open_ids": open_ids_from_cell(widget, cell)? })),
        "department" => Ok(Value::Array(
            open_ids_from_cell(widget, cell)?
                .into_iter()
                .map(|id| json!({ "open_id": id }))
                .collect(),
        )),
        // 其余文本类（input / textarea）直取字符串。
        _ => Ok(Value::String(text_from_cell(cell))),
    }
}

/// 日期：毫秒时间戳 → 带偏移量的 RFC3339。
///
/// 单点日期与日期区间的单元格形态不同（前者是数字，后者是 `{start, end}`），
/// 所以先按控件类型分流，再各自解析——顺序反了会让区间单元格先撞上
/// 「期望毫秒时间戳」。
fn assemble_date(
    widget: &WidgetMap,
    cell: &Value,
    offset: FixedOffset,
) -> Result<Value, ConvertError> {
    if widget.widget_type == "dateInterval" {
        let start = cell.get("start").and_then(Value::as_i64);
        let end = cell.get("end").and_then(Value::as_i64);
        let (Some(start), Some(end)) = (start, end) else {
            return Err(ConvertError::BadValue {
                widget_id: widget.widget_id.clone(),
                reason: format!("日期区间字段期望 {{start, end}} 两个毫秒时间戳，实际是 {cell}"),
            });
        };
        let days = (end - start) as f64 / 86_400_000.0;
        return Ok(json!({
            "start": format_rfc3339(start, offset),
            "end": format_rfc3339(end, offset),
            "interval": days,
        }));
    }

    let millis = cell.as_i64().ok_or_else(|| ConvertError::BadValue {
        widget_id: widget.widget_id.clone(),
        reason: format!("日期字段期望毫秒时间戳，实际是 {cell}"),
    })?;
    Ok(Value::String(format_rfc3339(millis, offset)))
}

/// 单选/多选：多维表格文案 → 审批控件 option value。
fn assemble_option(widget: &WidgetMap, cell: &Value) -> Result<Value, ConvertError> {
    let labels = labels_from_cell(cell);
    let mut values = Vec::with_capacity(labels.len());
    for label in labels {
        let mapped = widget
            .option_map
            .get(&label)
            .ok_or_else(|| ConvertError::UnmappedOption {
                widget_id: widget.widget_id.clone(),
                label: label.clone(),
            })?;
        values.push(Value::String(mapped.clone()));
    }

    // 单选控件传**单个字符串**，多选传数组——两者形态不同，混用会被拒。
    if matches!(widget.widget_type.as_str(), "radio" | "radioV2") {
        match values.len() {
            0 => Ok(Value::Null),
            1 => Ok(values.remove(0)),
            _ => Err(ConvertError::BadValue {
                widget_id: widget.widget_id.clone(),
                reason: format!("单选控件只能有一个选项，实际 {} 个", values.len()),
            }),
        }
    } else {
        Ok(Value::Array(values))
    }
}

/// 电话：从文本里拆区号与号码。
///
/// 多维表格的电话字段是纯文本，形态不固定（可能带 `+86`、可能带空格或横线）。
/// 拆不出来时**报错**而不是猜——猜错会打到一个错误的号码上。
fn assemble_telephone(widget: &WidgetMap, cell: &Value) -> Result<Value, ConvertError> {
    let raw = text_from_cell(cell);
    let trimmed = raw.trim();
    let (country, national) = if let Some(rest) = trimmed.strip_prefix('+') {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        // 区号 1~3 位（`+86` / `+1` / `+852`）。
        if digits.is_empty() || digits.len() > 3 {
            return Err(ConvertError::BadValue {
                widget_id: widget.widget_id.clone(),
                reason: format!("电话「{trimmed}」的区号无法识别"),
            });
        }
        let national: String = rest[digits.len()..]
            .chars()
            .filter(|c| c.is_ascii_digit())
            .collect();
        (format!("+{digits}"), national)
    } else {
        (
            "+86".to_string(),
            trimmed.chars().filter(|c| c.is_ascii_digit()).collect(),
        )
    };
    if national.is_empty() {
        return Err(ConvertError::BadValue {
            widget_id: widget.widget_id.clone(),
            reason: format!("电话「{trimmed}」没有号码部分"),
        });
    }
    Ok(json!({ "countryCode": country, "nationalNumber": national }))
}

fn number_from_cell(widget: &WidgetMap, cell: &Value) -> Result<Value, ConvertError> {
    if cell.is_number() {
        return Ok(cell.clone());
    }
    // 多维表格的数字偶尔以字符串形态出现（公式字段、文本化的数字）。
    if let Some(text) = cell.as_str() {
        if let Ok(parsed) = text.trim().parse::<f64>() {
            return Ok(json!(parsed));
        }
    }
    Err(ConvertError::BadValue {
        widget_id: widget.widget_id.clone(),
        reason: format!("数字字段期望数值，实际是 {cell}"),
    })
}

/// 人员字段取 `open_id` 数组。
///
/// 多维表格的人员字段是对象数组（`[{"id": "ou_…"}]`），但也可能是裸字符串数组
/// 或逗号分隔的文本——历史数据与不同来源的写法不一，三种都认。
fn open_ids_from_cell(widget: &WidgetMap, cell: &Value) -> Result<Vec<String>, ConvertError> {
    match cell {
        Value::Array(items) => {
            let mut ids = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::Object(map) => {
                        if let Some(id) = map.get("id").and_then(Value::as_str) {
                            ids.push(id.to_string());
                        }
                    }
                    Value::String(id) => ids.push(id.clone()),
                    _ => {}
                }
            }
            Ok(ids)
        }
        Value::String(text) => Ok(text
            .split([',', '，'])
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect()),
        _ => Err(ConvertError::BadValue {
            widget_id: widget.widget_id.clone(),
            reason: format!("人员字段期望人员数组，实际是 {cell}"),
        }),
    }
}

/// 从单元格里取出选项文案列表。
fn labels_from_cell(cell: &Value) -> Vec<String> {
    match cell {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                // 多维表格的选项单元格常是 `[{"text": "已批准"}]`。
                Value::Object(map) => map
                    .get("text")
                    .or_else(|| map.get("name"))
                    .or_else(|| map.get("value"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                Value::String(text) => Some(text.clone()),
                _ => None,
            })
            .collect(),
        Value::String(text) => vec![text.clone()],
        _ => Vec::new(),
    }
}

/// 多维表格单元格取文本。多态单元格（`[{"text": …}]`）取第一个 text。
fn text_from_cell(cell: &Value) -> String {
    match cell {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Array(items) => items
            .iter()
            .map(text_from_cell)
            .find(|text| !text.is_empty())
            .unwrap_or_default(),
        Value::Object(map) => map
            .get("text")
            .or_else(|| map.get("name"))
            .map(text_from_cell)
            .unwrap_or_default(),
        Value::Null => String::new(),
    }
}

/// 空值判定。多维表格的「空」有多种形态：null、空串、空数组、
/// 以及只有空文本的多态单元格（`[{"text": ""}]`）。
fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => items.iter().all(is_empty),
        // 多态单元格 `{"text": ""}` 的 map 本身非空，但**内容**是空的。
        // 只看 `map.is_empty()` 会把它判成有值，于是必填校验放行、飞书侧报
        // 1390001——错误指向飞书而不是指向这份数据。
        Value::Object(map) => {
            if map.is_empty() {
                return true;
            }
            // 只认承载内容的键；其他键（如附件 token）不参与空判定。
            let content_keys = ["text", "name", "value"];
            content_keys
                .iter()
                .filter_map(|key| map.get(*key))
                .all(is_empty)
                && map
                    .iter()
                    .filter(|(key, _)| !content_keys.contains(&key.as_str()))
                    .all(|(_, nested)| is_empty(nested))
        }
        _ => false,
    }
}

/// 毫秒时间戳 + 固定偏移 → RFC3339（`2019-10-01T08:12:01+08:00`）。
///
/// 手工换算而不是引 `chrono`：仓库现有的时间戳处理（`Timestamp` 字段、
/// `available_at` 等）都用 `i64` 毫秒，引入一个带时区数据库的日期库只为这一处
/// 换算不划算。
fn format_rfc3339(millis: i64, offset: FixedOffset) -> String {
    let shifted = millis + i64::from(offset.seconds) * 1_000;
    let total_seconds = shifted.div_euclid(1_000);
    let days = total_seconds.div_euclid(86_400);
    let seconds_of_day = total_seconds.rem_euclid(86_400);

    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;

    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{}",
        offset.render()
    )
}

/// 由「自 1970-01-01 起的天数」求公历年月日。
///
/// Howard Hinnant 的 `civil_from_days` 算法：把 3 月当作一年的起点，消掉闰年
/// 的特殊情形，再做两次整数修正。用整数运算，不涉及时区数据库。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    // 把 3 月当 1 月、2 月当 14 月，闰年就落在年末，不再需要单独分支。
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// 创建实例 API 不支持的控件类型。
///
/// 清单来源：官方《审批实例表单控件参数》的「审批实例 API 不支持的控件」表。
/// 含这些控件的审批定义**不能仅通过 API 提单**，必须在配置保存时就拦下。
pub(crate) fn is_unsupported_widget_type(widget_type: &str) -> bool {
    matches!(
        widget_type,
        // 「说明」控件：发起时不可编辑。
        "text"
            // 「引用多维表格」。
            | "mutableGroup"
            // 「收款账户」。
            | "account"
            // 「流水号」。
            | "serialNumber"
            // 控件组：出差 / 录用 / 转正 / 补卡 / 调岗 / 离职。
            | "tripGroup"
            | "apaascorehrOnboardingGroup"
            | "apaascorehrRegularateGroup"
            | "remedyGroupV2"
            | "apaascorehrJobAdjustGroup"
            | "apaascorehrOffboardingGroup"
    )
}

/// 需要人工提供凭据、本模块不自动准备的控件类型。
pub(crate) fn needs_manual_value(widget_type: &str) -> bool {
    matches!(
        widget_type,
        "address" | "connect" | "attachmentV2" | "attachment" | "image" | "imageV2" | "document"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shanghai() -> FixedOffset {
        FixedOffset::from_seconds(8 * 3600).unwrap_or_else(|e| panic!("{e}"))
    }

    fn widget(id: &str, kind: &str) -> WidgetMap {
        WidgetMap {
            widget_id: id.to_string(),
            widget_type: kind.to_string(),
            required: false,
            bitable_field: "fld_src".to_string(),
            // 默认转换器按控件类型取，避免每个用例都要手写一遍；
            // 专门测「不匹配」的用例再显式覆盖它。
            converter: expected_converter(kind),
            option_map: BTreeMap::new(),
            currency: None,
        }
    }

    /// 与生产代码同一张对应关系，供测试构造「匹配」的映射。
    fn expected_converter(kind: &str) -> Converter {
        match kind {
            "date" | "dateInterval" => Converter::Date,
            "radio" | "radioV2" | "checkbox" | "checkboxV2" => Converter::Option,
            _ => Converter::Direct,
        }
    }

    fn cells(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    fn render(widgets: &[WidgetMap], cell_map: &Map<String, Value>) -> Result<Vec<Value>, String> {
        let input = FormInput {
            widgets,
            cells: cell_map,
            timezone_offset: shanghai(),
        };
        let form = build_form(&input).map_err(|error| error.to_string())?;
        serde_json::from_str::<Value>(&form)
            .map_err(|error| error.to_string())?
            .as_array()
            .cloned()
            .ok_or_else(|| "form 必须是数组".to_string())
    }

    /// 取出组装失败的原因文案。
    ///
    /// 不用 `.unwrap_err()`：clippy 的 `unwrap_used` 是 deny，且它把「取错误值」
    /// 的变体一并归入——`#[cfg(test)]` 也不例外（仓库既有纪律）。
    fn render_error(widgets: &[WidgetMap], cell_map: &Map<String, Value>) -> String {
        match render(widgets, cell_map) {
            Ok(rendered) => panic!("期望组装失败，实际成功：{rendered:?}"),
            Err(error) => error,
        }
    }

    #[test]
    fn form_is_a_compressed_json_array_string() {
        // instance/create.md:30 要求 form 是「JSON 数组，压缩转义为字符串」。
        let widgets = [widget("w1", "input")];
        let input = FormInput {
            widgets: &widgets,
            cells: &cells(&[("fld_src", json!("hello"))]),
            timezone_offset: shanghai(),
        };
        let form = build_form(&input).unwrap_or_else(|e| panic!("{e}"));
        assert!(form.starts_with('['), "form 是数组字符串：{form}");
        let parsed: Value = serde_json::from_str(&form).unwrap_or_else(|e| panic!("{e}"));
        assert!(parsed.is_array());
    }

    #[test]
    fn optional_widget_without_value_is_omitted_entirely() {
        // official：不传必填控件时「该控件的完整 JSON 参数均不能传入」——
        // 一旦传入控件 JSON 就必须设 value，否则报错。
        let widgets = [widget("w1", "input")];
        let rendered = render(&widgets, &cells(&[])).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            rendered.is_empty(),
            "可选控件无值时应整个省略：{rendered:?}"
        );
    }

    #[test]
    fn missing_required_names_the_widget() {
        let mut required = widget("widget_missing", "input");
        required.required = true;
        let error = render_error(&[required], &cells(&[]));
        assert!(
            error.contains("widget_missing"),
            "错误必须能定位到控件 id：{error}"
        );
    }

    #[test]
    fn whitespace_only_cell_counts_as_missing() {
        let mut required = widget("w1", "input");
        required.required = true;
        let error = render_error(&[required], &cells(&[("fld_src", json!("   "))]));
        assert!(error.contains("缺少必填"), "{error}");
    }

    #[test]
    fn number_is_json_number_not_string() {
        // 传字符串会被 1390001 拒掉。
        let rendered = render(&[widget("w1", "number")], &cells(&[("fld_src", json!(42))]))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(rendered[0]["value"], json!(42));
        assert!(rendered[0]["value"].is_number());
    }

    #[test]
    fn amount_carries_currency_as_sibling() {
        let mut amount = widget("w1", "amount");
        amount.currency = Some("CNY".to_string());
        let rendered = render(&[amount], &cells(&[("fld_src", json!(12.5))]))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(rendered[0]["value"], json!(12.5));
        assert_eq!(rendered[0]["currency"], json!("CNY"));
    }

    #[test]
    fn date_becomes_rfc3339_with_offset() {
        // 2026-09-28T00:00:00+08:00 == 1790524800000 ms
        let rendered = render(
            &[widget("w1", "date")],
            &cells(&[("fld_src", json!(1_790_524_800_000_i64))]),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(rendered[0]["value"], json!("2026-09-28T00:00:00+08:00"));
    }

    #[test]
    fn date_with_negative_offset_renders_correctly() {
        // 偏移换算必须对负数也成立（div_euclid/rem_euclid，不是 / 和 %）。
        // 同一个瞬时 1790524800000 ms 在 -05:00 下是前一天 11:00。
        let input = FormInput {
            widgets: &[widget("w1", "date")],
            cells: &cells(&[("fld_src", json!(1_790_524_800_000_i64))]),
            timezone_offset: FixedOffset::from_seconds(-5 * 3600).unwrap_or_else(|e| panic!("{e}")),
        };
        let form = build_form(&input).unwrap_or_else(|e| panic!("{e}"));
        let parsed: Value = serde_json::from_str(&form).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(parsed[0]["value"], json!("2026-09-27T11:00:00-05:00"));
    }

    #[test]
    fn date_interval_produces_start_end_and_days() {
        // 日期区间控件的单元格是 {start, end}（两个毫秒时间戳）。
        let rendered = render(
            &[widget("w1", "dateInterval")],
            &cells(&[(
                "fld_src",
                json!({"start": 1_790_524_800_000_i64, "end": 1_790_611_200_000_i64}),
            )]),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            rendered[0]["value"]["start"],
            json!("2026-09-28T00:00:00+08:00")
        );
        assert_eq!(
            rendered[0]["value"]["end"],
            json!("2026-09-29T00:00:00+08:00")
        );
        assert_eq!(rendered[0]["value"]["interval"], json!(1.0));
    }

    #[test]
    fn converter_type_mismatch_is_rejected() {
        // 给 date 控件配 direct 转换器 → 原始毫秒时间戳会被当字符串发出去，
        // 飞书报 1390001，排查方向指向飞书而不是这份配置。必须在这里拦下。
        let mut mismatched = widget("w1", "date");
        mismatched.converter = Converter::Direct;
        let error = render_error(
            &[mismatched],
            &cells(&[("fld_src", json!(1_790_524_800_000_i64))]),
        );
        assert!(error.contains("不匹配"), "{error}");

        // 反向：给文本控件配 date 转换器。
        let mut wrong_way = widget("w2", "input");
        wrong_way.converter = Converter::Date;
        let error = render_error(&[wrong_way], &cells(&[("fld_src", json!("文本"))]));
        assert!(error.contains("不匹配"), "{error}");
    }

    #[test]
    fn epoch_and_pre_epoch_dates_are_correct() {
        let offset = FixedOffset::from_seconds(0).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(format_rfc3339(0, offset), "1970-01-01T00:00:00+00:00");
        assert_eq!(format_rfc3339(-1_000, offset), "1969-12-31T23:59:59+00:00");
    }

    #[test]
    fn leap_day_is_handled() {
        // 2024-02-29T00:00:00Z == 1709164800000
        let offset = FixedOffset::from_seconds(0).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            format_rfc3339(1_709_164_800_000, offset),
            "2024-02-29T00:00:00+00:00"
        );
    }

    #[test]
    fn radio_uses_mapped_value_not_label() {
        // 选项文案与审批控件 option value 不一定同名，必须走映射。
        let mut radio = widget("w1", "radioV2");
        radio.converter = Converter::Option;
        radio
            .option_map
            .insert("已批准".to_string(), "k2b8mkx0-h71x5gl1234-1".to_string());
        let rendered = render(
            &[radio],
            &cells(&[("fld_src", json!([{"text": "已批准"}]))]),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(rendered[0]["value"], json!("k2b8mkx0-h71x5gl1234-1"));
    }

    #[test]
    fn radio_is_a_single_string_not_array() {
        let mut radio = widget("w1", "radio");
        radio.converter = Converter::Option;
        radio.option_map.insert("A".to_string(), "va".to_string());
        let rendered =
            render(&[radio], &cells(&[("fld_src", json!("A"))])).unwrap_or_else(|e| panic!("{e}"));
        assert!(rendered[0]["value"].is_string(), "单选传单个字符串");
    }

    #[test]
    fn checkbox_is_an_array_of_mapped_values() {
        let mut checkbox = widget("w1", "checkboxV2");
        checkbox.converter = Converter::Option;
        checkbox
            .option_map
            .insert("A".to_string(), "va".to_string());
        checkbox
            .option_map
            .insert("B".to_string(), "vb".to_string());
        let rendered = render(&[checkbox], &cells(&[("fld_src", json!(["A", "B"]))]))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(rendered[0]["value"], json!(["va", "vb"]));
    }

    #[test]
    fn unmapped_option_names_the_label() {
        let mut radio = widget("w1", "radioV2");
        radio.converter = Converter::Option;
        let error = render_error(&[radio], &cells(&[("fld_src", json!("没配过的选项"))]));
        assert!(error.contains("没配过的选项"), "{error}");
    }

    #[test]
    fn contact_writes_open_ids() {
        let rendered = render(
            &[widget("w1", "contact")],
            &cells(&[("fld_src", json!([{"id": "ou_abc"}]))]),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(rendered[0]["value"], json!({ "open_ids": ["ou_abc"] }));
    }

    #[test]
    fn department_wraps_open_ids_in_object_array() {
        let rendered = render(
            &[widget("w1", "department")],
            &cells(&[("fld_src", json!([{"id": "od_xyz"}]))]),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(rendered[0]["value"], json!([{ "open_id": "od_xyz" }]));
    }

    #[test]
    fn telephone_splits_country_code() {
        let rendered = render(
            &[widget("w1", "telephone")],
            &cells(&[("fld_src", json!("+86 131-2222-2222"))]),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            rendered[0]["value"],
            json!({ "countryCode": "+86", "nationalNumber": "13122222222" })
        );
    }

    #[test]
    fn telephone_without_country_defaults_to_86() {
        let rendered = render(
            &[widget("w1", "telephone")],
            &cells(&[("fld_src", json!("13122222222"))]),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(rendered[0]["value"]["countryCode"], json!("+86"));
    }

    #[test]
    fn unsupported_widget_type_is_rejected_even_with_a_value() {
        // 含不支持控件的定义提不出去，早点报错好让用户改定义而不是反复补数据。
        let mut trip = widget("w1", "tripGroup");
        trip.required = false;
        let error = render_error(&[trip], &cells(&[("fld_src", json!("anything"))]));
        assert!(error.contains("不支持通过 API 提单"), "{error}");
    }

    #[test]
    fn every_officially_unsupported_widget_type_is_recognised() {
        // 官方清单逐条钉住，避免漏一个就静默放过。
        for kind in [
            "text",
            "mutableGroup",
            "account",
            "serialNumber",
            "tripGroup",
            "apaascorehrOnboardingGroup",
            "apaascorehrRegularateGroup",
            "remedyGroupV2",
            "apaascorehrJobAdjustGroup",
            "apaascorehrOffboardingGroup",
        ] {
            assert!(is_unsupported_widget_type(kind), "{kind} 应被认作不支持");
        }
        for kind in [
            "input",
            "textarea",
            "number",
            "date",
            "radioV2",
            "checkboxV2",
            "contact",
        ] {
            assert!(!is_unsupported_widget_type(kind), "{kind} 应被支持");
        }
    }

    #[test]
    fn unknown_converter_fails_closed() {
        // 静默退化成 Direct 会把日期或选项值原样送出，表现为飞书侧格式错误，
        // 排查方向完全跑偏。
        let error = match Converter::parse("typo") {
            Ok(value) => panic!("未知转换器应收敛失败，实际 {value:?}"),
            Err(error) => error,
        };
        assert_eq!(error, ConvertError::UnknownConverter("typo".to_string()));
    }

    #[test]
    fn converter_round_trips_known_names() {
        for (text, expected) in [
            ("direct", Converter::Direct),
            ("date", Converter::Date),
            ("option", Converter::Option),
        ] {
            assert_eq!(
                Converter::parse(text).unwrap_or_else(|e| panic!("{e}")),
                expected
            );
        }
    }

    #[test]
    fn offset_beyond_eighteen_hours_is_rejected() {
        assert!(FixedOffset::from_seconds(19 * 3600).is_err());
        assert!(FixedOffset::from_seconds(-19 * 3600).is_err());
    }

    #[test]
    fn unknown_timezone_is_rejected() {
        assert!(FixedOffset::from_iana("Mars/Olympus").is_err());
        assert_eq!(
            FixedOffset::from_iana("Asia/Shanghai").ok(),
            FixedOffset::from_seconds(8 * 3600).ok()
        );
    }

    #[test]
    fn empty_and_null_cells_are_treated_as_missing() {
        for empty in [json!(null), json!(""), json!([]), json!([{"text": ""}])] {
            let mut required = widget("w1", "input");
            required.required = true;
            assert!(
                render(&[required], &cells(&[("fld_src", empty.clone())])).is_err(),
                "{empty} 应被判为空"
            );
        }
    }
}

//! 多维表格记录拉取：URL 组装、分页收敛、单元格取值。
//!
//! # 接口选择
//!
//! 走官方《列出记录》`GET /open-apis/bitable/v1/apps/:app_token/tables/:table_id/records`。
//!
//! **该接口在官方文档首行已被标注为「历史接口，不推荐使用」**，替代品是《查询记录》
//! `POST .../records/search`。本次仍选它，理由是设计文档与任务清单（T4-3）都按
//! 「列出记录」的契约写好了字段口径，且两个接口的入参形态确有差异需要单独适配：
//!
//! | | 列出记录（本模块） | 查询记录 |
//! |---|---|---|
//! | `field_names` | **单个 JSON 数组字符串** | 真正的 `string[]` |
//! | 数字/进度/评分/货币 | **字符串**（`"100"` / `"0.66"`） | number |
//!
//! 迁移到《查询记录》时，这两处是必须一起改的地方：只改 URL 会让 `field_names`
//! 与数值取值同时失效，而且是静默失效（不报错、只是取空）。
//!
//! # 分页
//!
//! `page_size` 上限 **500**（默认 20）。设计文档里写的「100 上限」是审批后台选项数
//! 的限制，与分页无关——照那个数字写会把每页压到 100 行，白白多翻 5 倍页数。
//!
//! 取 500 是权衡的结果：它把翻页次数压到最少、降低触发频控的概率；但官方对
//! `1254030 TooLargeResponse` 的缓解建议恰恰是「适当降低 page_size」。本次不做自适应
//! 回退，而是把 `1254030` 归为**不可重试**（确定性失败），并在错误文案里直接给出
//! 「降低 page_size」的指引（见 `outbound::fatal_hint`）。

#![allow(dead_code)] // 出站能力先落地；消费者（拉取 worker）在后续批次接入。

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Context};
use serde::Deserialize;

use super::outbound::{
    send_with_retry, FailureKind, OutboundFailure, OutboundMethod, OutboundRequest,
    OutboundResponse, OutboundTransport, Sleeper, PULL_REQUEST_TIMEOUT_SECS, PULL_RETRY,
};
use super::tenant_token::{TenantTokenProvider, FEISHU_OPEN_BASE};

/// 每页行数，官方上限 500。
pub(crate) const PAGE_SIZE: u32 = 500;

/// 列出字段的分页上限（官方默认 20、最大 100），与记录不同。
const PAGE_SIZE_FIELDS: u32 = 100;

/// 单轮最多翻页数。防「`page_token` 不推进」导致的死循环。
pub(crate) const MAX_PAGES: u32 = 40;

/// 路径段长度上限。
const MAX_PATH_SEGMENT_LEN: usize = 128;

// ---------------------------------------------------------------------------
// URL 与查询参数
// ---------------------------------------------------------------------------

/// 校验一个要拼进 **URL 路径段** 的值。
///
/// `app_token` / `table_id` / `view_id` 来自数据源配置，直接采信会让 `../` 之类的
/// 输入把请求打到别的路径上。白名单只放 ASCII 字母数字与 `-` `_`——飞书的
/// base token（`bascnCMII2ORej2RItqpZZUNMIe`）与 table id（`tblxI2tWaxP5dG7p`）都落在这个集合内。
///
/// **`page_token` 刻意不做这个校验**：它是服务端下发的不透明游标（base64，含
/// `=` `+` `/`），白名单会把合法游标判非法。它只作为查询参数值出现，由传输层编码。
pub(crate) fn validate_path_segment(name: &str, value: &str) -> anyhow::Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= MAX_PATH_SEGMENT_LEN,
        "{name} 长度必须在 1..={MAX_PATH_SEGMENT_LEN} 字节"
    );
    ensure!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
        "{name} 只能包含 ASCII 字母、数字、连字符与下划线"
    );
    Ok(())
}

/// 组装记录列表 URL。
///
/// **先在函数内部校验两个路径段再拼**——把校验放在调用方是这类代码最常见的漏洞：
/// 组装函数自己看起来无害，于是新增的调用点忘了校验，而它恰恰是最危险的地方。
pub(crate) fn records_url(app_token: &str, table_id: &str) -> anyhow::Result<String> {
    validate_path_segment("bitable_base_token", app_token)?;
    validate_path_segment("bitable_table_id", table_id)?;
    Ok(format!(
        "{FEISHU_OPEN_BASE}/open-apis/bitable/v1/apps/{app_token}/tables/{table_id}/records"
    ))
}

/// 组装字段列表 URL。用于 `field_id → 精确字段名` 的映射。
pub(crate) fn fields_url(app_token: &str, table_id: &str) -> anyhow::Result<String> {
    validate_path_segment("bitable_base_token", app_token)?;
    validate_path_segment("bitable_table_id", table_id)?;
    Ok(format!(
        "{FEISHU_OPEN_BASE}/open-apis/bitable/v1/apps/{app_token}/tables/{table_id}/fields"
    ))
}

/// 组装列出视图 URL。
///
/// 官方《列出视图》：`GET /open-apis/bitable/v1/apps/:app_token/tables/:table_id/views`。
pub(crate) fn views_url(app_token: &str, table_id: &str) -> anyhow::Result<String> {
    validate_path_segment("bitable_base_token", app_token)?;
    validate_path_segment("bitable_table_id", table_id)?;
    Ok(format!(
        "{FEISHU_OPEN_BASE}/open-apis/bitable/v1/apps/{app_token}/tables/{table_id}/views"
    ))
}

/// 组装列出数据表 URL。
///
/// 官方《列出数据表》：`GET /open-apis/bitable/v1/apps/:app_token/tables`。
/// `app_token` 进路径段，必须先过 [`validate_path_segment`]。
pub(crate) fn tables_url(app_token: &str) -> anyhow::Result<String> {
    validate_path_segment("bitable_base_token", app_token)?;
    Ok(format!(
        "{FEISHU_OPEN_BASE}/open-apis/bitable/v1/apps/{app_token}/tables"
    ))
}

/// 组装列出记录的查询参数。
///
/// `field_names` 是**一个 JSON 数组字符串**，不是重复参数：写成
/// `queries([("field_names", "a"), ("field_names", "b")])` 会稳定吃
/// `1254024 InvalidFieldNames`。
///
/// `field_names` 里的名字必须是**精确字段名**（不是 field_id）——见
/// [`resolve_field_name`]。
pub(crate) fn list_records_query(
    field_names: &[String],
    view_id: Option<&str>,
    page_token: Option<&str>,
    page_size: u32,
) -> anyhow::Result<Vec<(String, String)>> {
    ensure!(page_size > 0, "page_size 必须为正数");
    // **不要发 offset**：官方该接口的查询参数表里没有它（只有 page_token/page_size），
    // 多发一个未声明参数属于自找 400。
    let mut query = vec![
        ("page_size".to_string(), page_size.to_string()),
        // 多行文本保持字符串返回（默认即 false）。置 true 会变成 `[{text,type}]`，
        // 对「取单值列当选项文案」这个用途只会增加解析分支。
        ("text_field_as_array".to_string(), "false".to_string()),
    ];
    if !field_names.is_empty() {
        query.push((
            "field_names".to_string(),
            serde_json::to_string(field_names).context("序列化 field_names 失败")?,
        ));
    }
    if let Some(view_id) = view_id {
        validate_path_segment("bitable_view_id", view_id)?;
        query.push(("view_id".to_string(), view_id.trim().to_string()));
    }
    if let Some(page_token) = page_token {
        // 原样透传，不校验形状（见 validate_path_segment 的说明）。
        query.push(("page_token".to_string(), page_token.to_string()));
    }
    Ok(query)
}

/// 组装「每页 100 条」的列表查询参数。
///
/// 服务于所有 `page_size` 上限为 100 的列表端点：列出字段、列出数据表、列出视图。
pub(crate) fn list_fields_query(page_token: Option<&str>) -> Vec<(String, String)> {
    let mut query = vec![("page_size".to_string(), PAGE_SIZE_FIELDS.to_string())];
    if let Some(page_token) = page_token {
        query.push(("page_token".to_string(), page_token.to_string()));
    }
    query
}

// ---------------------------------------------------------------------------
// 响应 DTO
// ---------------------------------------------------------------------------

/// 除「换取 token」外所有接口的信封（凭证接口是扁平的，见 `tenant_token`）。
///
/// `data` 刻意是 `serde_json::Value` 而不是 `Option<T>`：泛型版本会让
/// `#[serde(default)]` 在派生实现上引入 `T: Default` 约束（而 `ListRecordsData`
/// 并不实现 `Default`，也不该实现——一个「默认空快照」正是我们要拒绝的形态）。
/// 两步解析（先信封、再 `from_value`）既绕开这个约束，也把「code=0 但 data 缺席」
/// 这件事显式暴露出来。
#[derive(Debug, Deserialize)]
pub(crate) struct FeishuApiEnvelope {
    pub(crate) code: i32,
    #[serde(default)]
    pub(crate) msg: String,
    #[serde(default)]
    pub(crate) data: Option<serde_json::Value>,
}

/// 一条记录。
///
/// 没有 `Eq`：`fields` 里的 `serde_json::Value` 可能含 `f64`（数字类列），
/// 因此只到 `PartialEq` 为止。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct RecordItem {
    #[serde(default)]
    pub(crate) record_id: String,
    /// `map<string, union>`：官方把字段值定义为联合类型（单选=string、多选=array<string>、
    /// 数字/进度/评分/货币=string、日期=毫秒整数、公式=`[{text,type}]`、人员=对象数组……），
    /// **无法用单一 struct 表达**。强行 typed 会在取数列是数字类时反序列化失败。
    #[serde(default)]
    pub(crate) fields: BTreeMap<String, serde_json::Value>,
}

/// 列出记录的 `data`。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ListRecordsData {
    #[serde(default)]
    pub(crate) has_more: bool,
    /// `has_more=false` 时该字段**根本不出现**——写成 `String` 会反序列化失败。
    #[serde(default)]
    pub(crate) page_token: Option<String>,
    /// 总记录数。用于收敛断言；官方只承诺「总记录数」，不承诺等于你将收到的行数之和。
    #[serde(default)]
    pub(crate) total: Option<i64>,
    #[serde(default)]
    pub(crate) items: Vec<RecordItem>,
}

/// 列出数据表的 `data`。
///
/// 与 [`ListFieldsData`] 同样的理由带分页字段：一张 Base 的表数可以超过默认
/// `page_size`，少了分页就会把「还有第 2 页」静默吞掉。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct BitableTablesData {
    #[serde(default)]
    pub(crate) has_more: bool,
    #[serde(default)]
    pub(crate) page_token: Option<String>,
    #[serde(default)]
    pub(crate) total: Option<i64>,
    #[serde(default)]
    pub(crate) items: Vec<BitableTableItem>,
}

/// 一张数据表的标识信息。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct BitableTableItem {
    #[serde(default)]
    pub(crate) table_id: String,
    #[serde(default)]
    pub(crate) name: String,
}

/// 列出视图的 `data`。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct BitableViewsData {
    #[serde(default)]
    pub(crate) has_more: bool,
    #[serde(default)]
    pub(crate) page_token: Option<String>,
    #[serde(default)]
    pub(crate) total: Option<i64>,
    #[serde(default)]
    pub(crate) items: Vec<BitableViewItem>,
}

/// 一个视图的标识信息。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct BitableViewItem {
    #[serde(default)]
    pub(crate) view_id: String,
    #[serde(default)]
    pub(crate) view_name: String,
    /// `grid` 表格 / `kanban` 看板 / `gallery` 画册 / `gantt` 甘特 / `form` 表单。
    #[serde(default)]
    pub(crate) view_type: String,
}

/// 列出字段的 `data`。
///
/// **必须带分页字段**：字段接口默认 `page_size=20`，字段数超过 20 的表第 2 页起
/// 看不到。少了它，`resolve_field_name` 会对一个真实存在的 field_id 报「找不到」，
/// 把配置错误伪装成字段不存在。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ListFieldsData {
    #[serde(default)]
    pub(crate) has_more: bool,
    #[serde(default)]
    pub(crate) page_token: Option<String>,
    #[serde(default)]
    pub(crate) total: Option<i64>,
    #[serde(default)]
    pub(crate) items: Vec<FieldItem>,
}

/// 一个字段的标识信息。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct FieldItem {
    #[serde(default)]
    pub(crate) field_id: String,
    #[serde(default)]
    pub(crate) field_name: String,
    /// 字段类型码。官方《列出字段》的可选值：1 文本 / 2 数字 / 3 单选 / 4 多选 /
    /// 5 日期 / 7 复选框 / 11 人员 / 13 电话号码 / 15 超链接 / 17 附件 / 18 关联 /
    /// 20 公式 / 21 双向关联 / 22 地理位置 / 23 群组 / 1001~1005 自动字段。
    ///
    /// **这一位是消除「单选取值形态」歧义的唯一可靠依据**：只看单元格的 JSON
    /// 无法区分「多选但只选了一项」（`["A"]`）与「单选」（官方是 `"A"`）。
    #[serde(default, rename = "type")]
    pub(crate) field_type: Option<i32>,
    /// 字段 UI 类型（`SingleSelect` / `MultiSelect` / `Text` …）。
    #[serde(default)]
    pub(crate) ui_type: Option<String>,
    /// 字段属性。**单选/多选的选项就在这里**（`property.options[].name`）。
    ///
    /// 官方《列出字段》的响应里，选项**不在字段顶层**而嵌在 `property` 下——
    /// 没有这一位时按名匹配会拿到空选项，症状是「单选控件的选项一个都派生不出来」，
    /// 而列本身明明有选项。
    #[serde(default)]
    pub(crate) property: Option<FieldProperty>,
}

/// 字段属性。只取本模块用得到的一项。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct FieldProperty {
    /// 选项列表；单选/多选之外的字段类型没有这一项。
    #[serde(default)]
    pub(crate) options: Vec<PropertyOption>,
}

/// 一个字段选项。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PropertyOption {
    #[serde(default)]
    pub(crate) name: String,
}

impl FieldItem {
    /// 该字段的选项名列表（非选项类字段返回空）。
    pub(crate) fn option_names(&self) -> Vec<String> {
        self.property
            .as_ref()
            .map(|property| {
                property
                    .options
                    .iter()
                    .map(|option| option.name.trim().to_string())
                    .filter(|name| !name.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// 官方《多维表格记录数据结构》里值形态为**单值**的字段类型。
///
/// 取数列必须落在这些类型上。多值类型（多选 `4`、人员 `11`、附件 `17`、关联
/// `18`/`21`、群组 `23`）与无文本类型（复选框 `7`、地理位置 `22`）一律拒绝——
/// 详见 [`check_coordinate_field`]。
pub(crate) fn is_single_value_field_type(field_type: i32) -> bool {
    // 1 文本（含条码）/ 2 数字（含进度、货币、评分）/ 3 单选 / 5 日期 /
    // 13 电话号码 / 15 超链接 / 20 公式 / 1005 自动编号：这些在记录接口里
    // 都是**单个** string 或 number。
    matches!(field_type, 1 | 2 | 3 | 5 | 13 | 15 | 20 | 1005)
}

/// 校验取数列的字段类型是否可用，返回可读的类型名。
///
/// 这一步把「取数列选错类型」从**运行期的静默产出空标签**变成**明确的配置错误**：
/// 只从单元格值看不出类型（`["A"]` 既可能是多选选了一项，也可能是单选被归一化成
/// 了数组），而字段元数据是权威的。
pub(crate) fn check_coordinate_field(field: &FieldItem) -> anyhow::Result<String> {
    let label = field
        .ui_type
        .clone()
        .unwrap_or_else(|| format!("type={:?}", field.field_type));
    let Some(field_type) = field.field_type else {
        // 老响应或缺字段时不拦：宁可放行也不要因为拿不到元数据就拒绝一个本来可用的配置。
        return Ok(label);
    };
    ensure!(
        is_single_value_field_type(field_type),
        "字段「{}」的类型是 {}（type={field_type}），不是单值字段：\
         取数列必须指向文本、数字、单选、日期、电话、超链接或公式列。\
         多选/人员/附件/关联/群组/复选框/地理位置的单元格不是一个可用的选项值",
        field.field_name,
        label
    );
    Ok(label)
}

/// 把 `field_id` 映射成**精确字段名**。
///
/// `field_names` 要的是字段名而不是 field_id（官方 `1254024 InvalidFieldNames`
/// 的排查建议就是「调列出字段接口获取字段名称」）。而仓库里登记的是 field_id，
/// 因此这一步不可省——直接送 field_id 会稳定吃 `1254024`。
///
/// 三种失败都要报出来，一种都不能静默吞：
/// ① 找不到该 field_id；② 名字为空；③ **同名不唯一**——官方按名字匹配，重名列
/// 会取到不确定的那一列（实测同一张表里存在两列同名但 field_id 不同）。
pub(crate) fn resolve_field_name(fields: &[FieldItem], field_id: &str) -> anyhow::Result<String> {
    let matched = fields
        .iter()
        .find(|field| field.field_id == field_id)
        .map(|field| field.field_name.trim().to_string())
        .filter(|name| !name.is_empty())
        .with_context(|| {
            format!("多维表格里找不到 field_id {field_id} 对应的字段（或该字段名为空）")
        })?;

    let duplicates = fields
        .iter()
        .filter(|field| field.field_name.trim() == matched)
        .count();
    ensure!(
        duplicates == 1,
        "字段名「{matched}」在表内不唯一（{duplicates} 列同名）：\
         接口按名字匹配会取到不确定的列，请改用唯一列名"
    );
    Ok(matched)
}

/// 把一组 `field_id` 批量映射成**当前**的精确字段名。
///
/// 返回 `(field_id, 当前字段名)`，**顺序与输入一致**——日志、快照摘要与
/// `field_names` 都按这个顺序拼，顺序飘了对照价值就没了。
///
/// 与 [`resolve_field_name`] 的三点不同，都是表级拉取逼出来的：
///
/// 1. **批量**：一次快照要解析整张表勾选的所有列，逐列调用会重复扫全表元数据。
/// 2. **点名缺失**：缺哪个就报哪个，而不是在第一个缺失处停下——运维一次就能看全
///    要修的东西，不必「修一个跑一轮」。
/// 3. **按 id 稳定**：字段改名后仍解析得出当前名字。这正是身份存 `field_id`
///    而不是 `field_name` 的全部意义。
pub(crate) fn resolve_field_names(
    fields: &[FieldItem],
    field_ids: &[String],
) -> anyhow::Result<Vec<(String, String)>> {
    let name_of = |field_id: &str| -> Option<String> {
        fields
            .iter()
            .find(|field| field.field_id == field_id)
            .map(|field| field.field_name.trim().to_string())
            .filter(|name| !name.is_empty())
    };

    let missing: Vec<&str> = field_ids
        .iter()
        .map(String::as_str)
        .filter(|field_id| name_of(field_id).is_none())
        .collect();
    ensure!(
        missing.is_empty(),
        "多维表格里找不到这些字段（可能已被删除，或该字段名为空）：{}",
        missing.join("、")
    );

    let resolved: Vec<(String, String)> = field_ids
        .iter()
        .filter_map(|field_id| name_of(field_id).map(|name| (field_id.clone(), name)))
        .collect();

    // 重名必须拦在拉取之前：`field_names` 是**按名字匹配**的，表里有两列同名时
    // 接口会取到不确定的那一列。单列版已经有这条，批量版不能漏。
    for (_, name) in &resolved {
        let in_table = fields
            .iter()
            .filter(|field| field.field_name.trim() == name)
            .count();
        ensure!(
            in_table == 1,
            "字段名「{name}」在表内不唯一（{in_table} 列同名）：\
             接口按名字匹配会取到不确定的列，请改用唯一列名"
        );
    }
    Ok(resolved)
}

/// 拉一次字段元数据，把一组 `field_id` 解析成当前名字。
///
/// 表级拉取的第一步：一次快照要覆盖整张表勾选的所有列，所以名字在同一次
/// 元数据里解析完，不做逐列往返。
///
/// 缺失一律归为 `Fatal`：这是**配置错误**（列被删了），退避重试不会自愈。
pub(crate) async fn resolve_current_field_names(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    coordinates: &BitableCoordinates,
    field_ids: &[String],
) -> Result<Vec<(String, String)>, OutboundFailure> {
    let fields = list_all_fields(transport, sleeper, tokens, coordinates).await?;
    resolve_field_names(&fields, field_ids).map_err(|error| OutboundFailure {
        kind: FailureKind::Fatal { code: 0 },
        message: error.to_string(),
    })
}

// ---------------------------------------------------------------------------
// 单元格取值
// ---------------------------------------------------------------------------

/// 单元格取值的三态结果。
///
/// 把「合法空值」与「**不支持的列类型**」分开是刻意的：人员、群组、地理位置是
/// **不含 `text` 键**的对象，若都返回「空」，取数列选到这几类列时会静默产出空标签
/// （不报错、不告警），后续再叠加「读成功且为空即停用补集」就会把该列的选项集
/// 整体停用。这种情况必须能被上层看见并拒绝。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CellValue {
    /// 取到了文本。
    Text(String),
    /// 单元格确实为空（缺列、`null`、空串、空数组）。
    Empty,
    /// 列类型不支持取值（多值列、人员、群组、地理位置、复选框等）。
    Unsupported,
}

/// 从单元格值里取一个用于展示的标签。
///
/// **单选字段同时接受裸字符串与长度 1 的数组**。两种形态都有实证：
/// 官方《列出记录》响应示例写的是 `"单选": "选项1"`（裸字符串），而本仓库
/// 用 lark-cli 抓的真实数据是 `"费用大类/Main Exp Cat*": ["股东借款"]`（数组，
/// 且该字段定义为 `multiple: false`），其 manifest 也把 `physical_type` 标为
/// `array<string>`。既然两个方向都有证据，就两个都吃——写错方向是**静默丢数据**
/// （选项取空 → 飞书控件显示为空），代价远大于多一个分支。
pub(crate) fn cell_label(value: &serde_json::Value) -> CellValue {
    match value {
        serde_json::Value::String(text) => classify_text(text),
        serde_json::Value::Number(number) => classify_text(&number.to_string()),
        serde_json::Value::Null => CellValue::Empty,
        serde_json::Value::Array(items) => match items.as_slice() {
            [] => CellValue::Empty,
            // 长度 1：单值列。这正是「单选用数组返回」的那种形态。
            [single] => match union_text(single) {
                Some(text) => classify_text(&text),
                None => CellValue::Unsupported,
            },
            // 长度 ≥2：多值列。取数列必须指向单值列，报出来而不是随手取第一个
            // ——静默取第一个会让「汇率（多选）」这种列悄悄产出一个看起来正常的选项集。
            _ => CellValue::Unsupported,
        },
        serde_json::Value::Object(_) => match union_text(value) {
            Some(text) => classify_text(&text),
            // 人员 / 群组 / 地理位置：对象里没有 `text` 键
            None => CellValue::Unsupported,
        },
        serde_json::Value::Bool(_) => CellValue::Unsupported,
    }
}

/// 从 union 里抠文本：字符串直取，数字字符串化，`{text,type}` 形态取 `text`。
fn union_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::Object(map) => map
            .get("text")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

fn classify_text(text: &str) -> CellValue {
    if text.trim().is_empty() {
        CellValue::Empty
    } else {
        CellValue::Text(text.to_string())
    }
}

// ---------------------------------------------------------------------------
// 分页状态机
// ---------------------------------------------------------------------------

/// 分页状态机 + 收敛断言。
///
/// **成功判据不是 `fetched != 0`**：那会把「第 2 页 429 失败」当成成功，从而把补集
/// 停用建立在半份快照上（把还有效的选项批量停用）。真正的判据是「`page_token` 耗尽
/// 且累计行数不少于首屏 `total`」。
#[derive(Debug)]
pub(crate) struct PaginationState {
    accumulated: usize,
    first_total: Option<i64>,
    has_more: bool,
    pages: u32,
    max_pages: u32,
    seen_tokens: BTreeSet<String>,
}

impl PaginationState {
    pub(crate) fn new(max_pages: u32) -> Self {
        Self {
            accumulated: 0,
            first_total: None,
            has_more: false,
            pages: 0,
            max_pages,
            seen_tokens: BTreeSet::new(),
        }
    }

    /// 累计行数与首屏总数（供调用方判断「读成功但为空」这种歧义形态）。
    pub(crate) fn accumulated(&self) -> usize {
        self.accumulated
    }

    pub(crate) fn first_total(&self) -> Option<i64> {
        self.first_total
    }

    /// 吞下一屏，返回下一页游标（`None` 表示已耗尽）。
    ///
    /// 与具体载荷解耦（只吃计数与游标），因此记录与字段两种列表可以共用同一套收敛判据。
    pub(crate) fn accept(
        &mut self,
        has_more: bool,
        page_token: Option<String>,
        total: Option<i64>,
        item_count: usize,
    ) -> anyhow::Result<Option<String>> {
        self.pages += 1;
        ensure!(
            self.pages <= self.max_pages,
            "翻页数超过上限 {}：疑似 page_token 不推进",
            self.max_pages
        );
        self.accumulated += item_count;
        self.has_more = has_more;
        if self.first_total.is_none() {
            self.first_total = total;
        }
        if !has_more {
            return Ok(None);
        }
        // has_more=true 却不给游标：继续翻页只能重复请求同一页，必须硬失败。
        let cursor = page_token
            .filter(|token| !token.is_empty())
            .context("飞书返回 has_more=true 但没有 page_token，无法继续翻页")?;
        // 重复游标同样会死循环；而且 has_more 恒为 true 时 `assert_converged` 永远
        // 执行不到——循环会一直打网络，直到被飞书频控拦下为止。
        ensure!(
            self.seen_tokens.insert(cursor.clone()),
            "飞书返回了重复的 page_token，分页不会收敛（已翻 {} 页）",
            self.pages
        );
        Ok(Some(cursor))
    }

    /// 整轮是否收敛。
    ///
    /// 判据：`page_token` 耗尽 + `total` 存在且非负 + **累计行数 ≥ 首屏 total**。
    ///
    /// 用 `≥` 而不是 `==` 是刻意的：官方只承诺 `total` 是「总记录数」，没有承诺它
    /// 等于你将分页收到的行数之和。翻页途中表被并发编辑会让 `==` 恒假，于是
    /// 「连续失败次数」永不清零、持续告警——把一个 fail-closed 的守卫变成噪声源。
    /// 要拦的方向是**截断**（累计 < total），不等式同样拦得住。
    pub(crate) fn assert_converged(&self) -> anyhow::Result<()> {
        ensure!(!self.has_more, "分页未耗尽：最后一屏仍返回 has_more=true");
        let total = self
            .first_total
            .context("首屏未返回 total，无法证明快照完整")?;
        ensure!(total >= 0, "飞书返回的 total 为负数: {total}");
        ensure!(
            self.accumulated as i64 >= total,
            "累计行数 {} 少于首屏 total {total}：这是不完整快照",
            self.accumulated
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 拉取
// ---------------------------------------------------------------------------

/// 一轮拉取的结果。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RecordsSnapshot {
    pub(crate) items: Vec<RecordItem>,
    /// 首屏返回的总记录数。
    ///
    /// 单独暴露是给调用方一个**拒绝空集**的机会：多维表格开启高级权限而调用身份
    /// 不在授权群内时，官方明示「可能出现调用成功但返回数据为空」——此时
    /// `code=0`、`total=0`、`items=[]`，收敛断言会通过。若不看这个信号就执行
    /// 补集停用，会把该数据源 100% 已启用的选项静默停掉。
    pub(crate) total: i64,
}

/// 拉取一页并做业务码检查。
/// **唯一的重试入口**（GET 形态）：带 body 的写请求用 [`send_json`]。
async fn fetch_page<T: for<'de> Deserialize<'de>>(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    url: &str,
    query: Vec<(String, String)>,
) -> Result<T, OutboundFailure> {
    send_json(
        transport,
        sleeper,
        tokens,
        OutboundMethod::Get,
        url,
        query,
        None,
        PULL_REQUEST_TIMEOUT_SECS,
    )
    .await
}

/// 带 JSON body 的出站请求 + 信封解码。
///
/// 与 `fetch_page` 共用「token 失效 → 清缓存强制刷新 → 重试本页」的补救，
/// 只是多支持方法与 body 两维——写路径（`records/search`、`records/batch_update`）
/// 需要 POST + body，而 GET 用的 `fetch_page` 保持原样不动。
#[allow(clippy::too_many_arguments)]
async fn send_json<T: for<'de> Deserialize<'de>>(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    method: OutboundMethod,
    url: &str,
    query: Vec<(String, String)>,
    body: Option<serde_json::Value>,
    timeout_secs: u64,
) -> Result<T, OutboundFailure> {
    // 每页最多允许**一次**「token 失效 → 清缓存强制刷新 → 重试本页」。
    // 不做递归：第二次仍失效说明不是缓存陈旧。
    let mut token_refresh_used = false;
    loop {
        let token = tokens
            .tenant_access_token()
            .await
            .map_err(|error| OutboundFailure {
                kind: FailureKind::Fatal { code: 0 },
                message: error.to_string(),
            })?;

        let request = OutboundRequest {
            method,
            url: url.to_string(),
            query: query.clone(),
            bearer_token: Some(token),
            json_body: body.clone(),
            raw_body: None,
            binary_limit: None,
            timeout_secs: Some(timeout_secs),
            // 幂等：审批派发路径上的写是同值覆盖（回写同一记录的同一编号），
            // search 是只读。真正的幂等保证在创建审批实例那一步（uuid），
            // 不在这里；这里标 true 是为了让 429/5xx 能进退避重试——
            // `disposition` 在 `idempotent == false` 时对 Retry 类失败直接 Give。
            idempotent: true,
        };

        match send_with_retry(transport, sleeper, &request, PULL_RETRY).await {
            Ok(response) => return decode_envelope(response),
            Err(failure) => {
                if failure.kind == FailureKind::TokenExpired && !token_refresh_used {
                    token_refresh_used = true;
                    tracing::warn!("飞书 tenant_access_token 失效，清缓存并强制刷新一次");
                    // 刷新失败就直接冒泡：再试一次也只是拿同一个坏凭证。
                    tokens
                        .invalidate_and_refresh()
                        .await
                        .map_err(|error| OutboundFailure {
                            kind: FailureKind::Fatal { code: 0 },
                            message: format!("tenant_access_token 失效后强制刷新失败: {error}"),
                        })?;
                    continue;
                }
                return Err(failure);
            }
        }
    }
}

/// 解析信封并取出 `data`。
fn decode_envelope<T: for<'de> Deserialize<'de>>(
    response: OutboundResponse,
) -> Result<T, OutboundFailure> {
    let envelope: FeishuApiEnvelope =
        serde_json::from_str(&response.body).map_err(|error| OutboundFailure {
            kind: FailureKind::Fatal { code: 0 },
            message: format!(
                "解析飞书响应失败: {error}；原文: {}",
                super::outbound::summarize(response.status, &response.body)
            ),
        })?;
    // 走到这里说明 `outbound::classify` 已判成功，即 code == 0；
    // 但 `data` 仍可能缺席——那是我们对契约的理解错了，要报出来而不是当空集。
    let data = envelope.data.ok_or_else(|| OutboundFailure {
        kind: FailureKind::Fatal {
            code: envelope.code,
        },
        message: format!("飞书返回 code=0 但缺少 data（msg={}）", envelope.msg),
    })?;
    serde_json::from_value(data).map_err(|error| OutboundFailure {
        kind: FailureKind::Fatal { code: 0 },
        message: format!("解析飞书 data 失败: {error}"),
    })
}

/// 一个数据源在多维表格里的坐标。
///
/// 打成一个结构体而不是散成多个参数：三个值总是**一起**来自同一行数据源，
/// 且两个路径段必须一起通过 [`validate_path_segment`]；拆开传容易拼出
/// 「A 源的 app_token + B 源的 table_id」这种不会报错、只会读到错表的组合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BitableCoordinates {
    pub(crate) app_token: String,
    pub(crate) table_id: String,
    pub(crate) view_id: Option<String>,
}

/// 只取**第一页**记录，不做收敛断言。
///
/// 给诊断路径用：它要回答的是「凭证能不能注入、列名能不能解析、值长什么样」，
/// 而不是「快照完不完整」——因此**故意不套** [`PaginationState`] 的收敛判据，
/// 也不按 `MAX_PAGES` 翻页。走了收敛判据反而会在一张多页表上直接失败，
/// 把「探测成功」误报成「拉取失败」。
pub(crate) async fn first_records_page(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    coordinates: &BitableCoordinates,
    field_names: &[String],
    page_size: u32,
) -> Result<ListRecordsData, OutboundFailure> {
    let url = records_url(&coordinates.app_token, &coordinates.table_id).map_err(|error| {
        OutboundFailure {
            kind: FailureKind::Fatal { code: 0 },
            message: error.to_string(),
        }
    })?;
    let query = list_records_query(field_names, coordinates.view_id.as_deref(), None, page_size)
        .map_err(|error| OutboundFailure {
            kind: FailureKind::Fatal { code: 0 },
            message: error.to_string(),
        })?;
    fetch_page(transport, sleeper, tokens, &url, query).await
}

/// 拉取全量记录。
///
/// 成功即返回**已收敛**的快照；任何一页失败或未收敛都会返回 `Err`，
/// 绝不返回「部分行 + 看起来成功」。
pub(crate) async fn list_all_records(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    coordinates: &BitableCoordinates,
    field_names: &[String],
    max_pages: u32,
) -> Result<RecordsSnapshot, OutboundFailure> {
    let url = records_url(&coordinates.app_token, &coordinates.table_id).map_err(|error| {
        OutboundFailure {
            kind: FailureKind::Fatal { code: 0 },
            message: error.to_string(),
        }
    })?;
    let view_id = coordinates.view_id.as_deref();

    let mut state = PaginationState::new(max_pages);
    let mut cursor: Option<String> = None;
    let mut items: Vec<RecordItem> = Vec::new();

    loop {
        let query = list_records_query(field_names, view_id, cursor.as_deref(), PAGE_SIZE)
            .map_err(|error| OutboundFailure {
                kind: FailureKind::Fatal { code: 0 },
                message: error.to_string(),
            })?;

        let page: ListRecordsData = fetch_page(transport, sleeper, tokens, &url, query).await?;

        let next = state
            .accept(
                page.has_more,
                page.page_token.clone(),
                page.total,
                page.items.len(),
            )
            .map_err(|error| OutboundFailure {
                kind: FailureKind::Fatal { code: 0 },
                message: error.to_string(),
            })?;

        items.extend(page.items);

        match next {
            Some(cursor_value) => cursor = Some(cursor_value),
            None => break,
        }
    }

    state.assert_converged().map_err(|error| OutboundFailure {
        kind: FailureKind::Fatal { code: 0 },
        message: error.to_string(),
    })?;

    Ok(RecordsSnapshot {
        items,
        total: state.first_total().unwrap_or_default(),
    })
}

/// 拉取某张数据表下的全部视图。
///
/// 供配置向导第二步用。**注意视图的职责**：它决定**拉取哪些行**，不决定能勾哪些字段
/// （实测「列出字段」的 `view_id` 参数不生效）。UI 必须把这件事讲对。
pub(crate) async fn list_all_views(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    app_token: &str,
    table_id: &str,
) -> Result<Vec<BitableViewItem>, OutboundFailure> {
    let url = views_url(app_token, table_id).map_err(|error| OutboundFailure {
        kind: FailureKind::Fatal { code: 0 },
        message: error.to_string(),
    })?;

    let mut state = PaginationState::new(MAX_PAGES);
    let mut cursor: Option<String> = None;
    let mut items: Vec<BitableViewItem> = Vec::new();

    loop {
        let query = list_fields_query(cursor.as_deref());
        let page: BitableViewsData = fetch_page(transport, sleeper, tokens, &url, query).await?;

        let next = state
            .accept(
                page.has_more,
                page.page_token.clone(),
                page.total,
                page.items.len(),
            )
            .map_err(|error| OutboundFailure {
                kind: FailureKind::Fatal { code: 0 },
                message: error.to_string(),
            })?;

        items.extend(page.items);
        match next {
            Some(cursor_value) => cursor = Some(cursor_value),
            None => break,
        }
    }

    state.assert_converged().map_err(|error| OutboundFailure {
        kind: FailureKind::Fatal { code: 0 },
        message: error.to_string(),
    })?;
    Ok(items)
}

/// 拉取某张 Base 下的全部数据表。
///
/// 供配置向导第一步用：运维只填 `app_token`，表列表由这里取回让他在界面上选。
pub(crate) async fn list_all_tables(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    app_token: &str,
) -> Result<Vec<BitableTableItem>, OutboundFailure> {
    let url = tables_url(app_token).map_err(|error| OutboundFailure {
        kind: FailureKind::Fatal { code: 0 },
        message: error.to_string(),
    })?;

    let mut state = PaginationState::new(MAX_PAGES);
    let mut cursor: Option<String> = None;
    let mut items: Vec<BitableTableItem> = Vec::new();

    loop {
        let query = list_fields_query(cursor.as_deref());
        let page: BitableTablesData = fetch_page(transport, sleeper, tokens, &url, query).await?;

        let next = state
            .accept(
                page.has_more,
                page.page_token.clone(),
                page.total,
                page.items.len(),
            )
            .map_err(|error| OutboundFailure {
                kind: FailureKind::Fatal { code: 0 },
                message: error.to_string(),
            })?;

        items.extend(page.items);
        match next {
            Some(cursor_value) => cursor = Some(cursor_value),
            None => break,
        }
    }

    state.assert_converged().map_err(|error| OutboundFailure {
        kind: FailureKind::Fatal { code: 0 },
        message: error.to_string(),
    })?;
    Ok(items)
}

/// 拉取全量字段定义（用于 `field_id → 字段名` 映射）。
pub(crate) async fn list_all_fields(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    coordinates: &BitableCoordinates,
) -> Result<Vec<FieldItem>, OutboundFailure> {
    let url = fields_url(&coordinates.app_token, &coordinates.table_id).map_err(|error| {
        OutboundFailure {
            kind: FailureKind::Fatal { code: 0 },
            message: error.to_string(),
        }
    })?;

    let mut state = PaginationState::new(MAX_PAGES);
    let mut cursor: Option<String> = None;
    let mut items: Vec<FieldItem> = Vec::new();

    loop {
        let query = list_fields_query(cursor.as_deref());
        let page: ListFieldsData = fetch_page(transport, sleeper, tokens, &url, query).await?;

        let next = state
            .accept(
                page.has_more,
                page.page_token.clone(),
                page.total,
                page.items.len(),
            )
            .map_err(|error| OutboundFailure {
                kind: FailureKind::Fatal { code: 0 },
                message: error.to_string(),
            })?;

        items.extend(page.items);
        match next {
            Some(cursor_value) => cursor = Some(cursor_value),
            None => break,
        }
    }

    state.assert_converged().map_err(|error| OutboundFailure {
        kind: FailureKind::Fatal { code: 0 },
        message: error.to_string(),
    })?;
    Ok(items)
}

// ---------------------------------------------------------------------------
// 审批派发路径：查询记录（带筛选）与批量回写
// ---------------------------------------------------------------------------

/// 单次查询记录的行数上限。
///
/// 官方《查询记录》是 **500**，与《列出记录》的 `PAGE_SIZE` 同值但**出处不同**——
/// 两个接口的上限各自独立，将来一方调整不会自动带动另一方，所以不复用同一个常量。
pub(crate) const SEARCH_PAGE_SIZE: u32 = 500;

/// 批量回写的子批上限。
///
/// `batch_update` 单次上限是 **1000**，这里取 100 不是照抄上限，而是两条约束的
/// 交集：
///
/// 1. 批量写是**全有全无**语义（官方：「响应状态是全部成功或者失败，不存在部分
///    成功或失败的结果」），所以批越小、一条毒记录牵连的无辜记录越少；
/// 2. 官方对 `1254607` 的排查建议就是「降低批量请求的 page_size」。
///
/// 回写失败时的处置是**折半拆批**直到定位到具体记录，所以这个值同时是拆分的起点。
pub(crate) const BACKFILL_CHUNK: usize = 100;

/// 查询记录接口的 URL。
pub(crate) fn search_records_url(app_token: &str, table_id: &str) -> anyhow::Result<String> {
    validate_path_segment("app_token", app_token)?;
    validate_path_segment("table_id", table_id)?;
    Ok(format!(
        "{FEISHU_OPEN_BASE}/open-apis/bitable/v1/apps/{app_token}/tables/{table_id}/records/search"
    ))
}

/// 批量回写接口的 URL。
pub(crate) fn batch_update_records_url(app_token: &str, table_id: &str) -> anyhow::Result<String> {
    validate_path_segment("app_token", app_token)?;
    validate_path_segment("table_id", table_id)?;
    Ok(format!(
        "{FEISHU_OPEN_BASE}/open-apis/bitable/v1/apps/{app_token}/tables/{table_id}/records/batch_update"
    ))
}

/// 一条待回写的记录：记录 id + 要写的单元格。
pub(crate) type BackfillRow = (String, BTreeMap<String, serde_json::Value>);

/// 批量回写记录的单元格。
///
/// **全有全无**：任一记录触发字段转换类错误（如文本字段收到超长串），整批以非 0
/// code 返回且**没有任何 per-record 失败列表**可读。所以调用方必须能容忍「整批
/// 零条落库」并据此重试或拆批——本函数只如实返回失败，不做拆分（拆分在编排层，
/// 因为那里才知道哪些记录属于同一批）。
pub(crate) async fn batch_update_records(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    coordinates: &BitableCoordinates,
    rows: &[BackfillRow],
) -> Result<(), OutboundFailure> {
    let url = batch_update_records_url(&coordinates.app_token, &coordinates.table_id).map_err(
        |error| OutboundFailure {
            kind: FailureKind::Fatal { code: 0 },
            message: error.to_string(),
        },
    )?;

    let records: Vec<serde_json::Value> = rows
        .iter()
        .map(|(record_id, fields)| {
            serde_json::json!({
                "record_id": record_id,
                "fields": fields,
            })
        })
        .collect();
    let body = serde_json::json!({ "records": records });

    // 用 `serde_json::Value` 接响应而不是定义结构体：这个接口的响应体形态对本调用
    // 者没有信息量（成功侧 `records` 一律相同），只有成败与错误码有用——而那由
    // `classify` 在信封层判掉了。
    let _: serde_json::Value = send_json(
        transport,
        sleeper,
        tokens,
        OutboundMethod::Post,
        &url,
        Vec::new(),
        Some(body),
        PULL_REQUEST_TIMEOUT_SECS,
    )
    .await?;
    Ok(())
}

/// 查询记录的 `data`。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SearchRecordsData {
    #[serde(default)]
    pub(crate) items: Vec<RecordItem>,
    #[serde(default)]
    pub(crate) has_more: bool,
    /// `has_more=false` 时**根本不出现**——写成 `String` 会反序列化失败。
    #[serde(default)]
    pub(crate) page_token: Option<String>,
    #[serde(default)]
    pub(crate) total: Option<i64>,
}

/// 按条件查询记录（单页）。
///
/// 与《列出记录》的差异（迁移时必须一起改，见本文件头部说明）：`field_names` 是
/// 真正的 `string[]`（不是 JSON 数组字符串），数字类单元格是 number（不是字符串）。
/// 本函数只发一页、不做收敛断言——调用方按 `record_id` 过滤时结果本就是一行。
pub(crate) async fn search_records(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    coordinates: &BitableCoordinates,
    body: serde_json::Value,
    page_token: Option<&str>,
) -> Result<SearchRecordsData, OutboundFailure> {
    let url =
        search_records_url(&coordinates.app_token, &coordinates.table_id).map_err(|error| {
            OutboundFailure {
                kind: FailureKind::Fatal { code: 0 },
                message: error.to_string(),
            }
        })?;
    // `page_token` / `page_size` 是**查询参数**（不是请求体字段）——《查询记录》的
    // 请求体只有 `filter` / `sort` / `field_names` / `automatic_fields` 那几项。
    let mut query = vec![("page_size".to_string(), SEARCH_PAGE_SIZE.to_string())];
    if let Some(page_token) = page_token.filter(|token| !token.is_empty()) {
        query.push(("page_token".to_string(), page_token.to_string()));
    }
    send_json(
        transport,
        sleeper,
        tokens,
        OutboundMethod::Post,
        &url,
        query,
        Some(body),
        PULL_REQUEST_TIMEOUT_SECS,
    )
    .await
}

/// 翻页取**全部**命中的记录。
///
/// # 为什么要收敛断言之外还给上限
///
/// `max_pages` 是**防失控**而不是分页参数：筛选条件写错（比如过滤条件没生效）时，
/// 一页页拉全表会把这一轮无限拖长，而 worker 是单线程的——它卡住就等于整个队列停摆。
/// 到上限即报错，让「条件没生效」以显式失败出现，而不是以「跑了十分钟」出现。
pub(crate) async fn search_all_records(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    coordinates: &BitableCoordinates,
    body: serde_json::Value,
    max_pages: u32,
) -> Result<Vec<RecordItem>, OutboundFailure> {
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..max_pages {
        let page = search_records(
            transport,
            sleeper,
            tokens,
            coordinates,
            body.clone(),
            cursor.as_deref(),
        )
        .await?;
        items.extend(page.items);
        if !page.has_more {
            return Ok(items);
        }
        match page.page_token.filter(|token| !token.is_empty()) {
            Some(token) => cursor = Some(token),
            // `has_more=true` 却不给 `page_token`：继续翻会**原样重取第一页**
            // （死循环 + 重复行），所以这里必须失败而不是硬翻。
            None => {
                return Err(OutboundFailure {
                    kind: FailureKind::Fatal { code: 0 },
                    message: "查询记录返回 has_more=true 却没有 page_token".to_string(),
                })
            }
        }
    }
    Err(OutboundFailure {
        kind: FailureKind::Fatal { code: 0 },
        message: format!("查询记录翻页超过 {max_pages} 页仍未收敛"),
    })
}

/// 「某列为空」的筛选体。
///
/// `isEmpty` 的 `value` 必须是**空数组**（官方字段目标值说明：operator 为
/// `isEmpty`/`isNotEmpty` 时需填空值 `[]`）——传 null 或不传都会吃 `1254018`。
pub(crate) fn empty_field_filter(field_name: &str) -> serde_json::Value {
    serde_json::json!({
        "filter": {
            "conjunction": "and",
            "conditions": [
                { "field_name": field_name, "operator": "isEmpty", "value": [] }
            ]
        },
        // 系统字段（创建人/创建时间…）没有 field_id，取回来会在重映射那步报错。
        "automatic_fields": false,
    })
}

/// 按 `record_id` 列表取记录（`records/batch_get`，单次最多 100 条）。
///
/// # 为什么不是「查询记录 + filter」
///
/// 原实现用 `filter: {field_name: "record_id", operator: "is"}`，而 `record_id`
/// 是**响应里的系统字段、不是可过滤字段**——《记录筛选参数填写说明》的
/// `field_name` 结构里只有普通字段（`record-filter-guide.md` 全文举的例是
/// 「字段1 / 职位 / 销售额」），把系统字段塞进去得不到一条按 id 的定位。
/// `records/batch_get` 是官方给的正解：入参就是 `record_ids[]`。
///
/// 顺带比原来的写法省一次「拉多页 + 本地过滤」的往返：这里一次请求就拿到目标行。
pub(crate) async fn get_records_by_ids(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    coordinates: &BitableCoordinates,
    record_ids: &[String],
) -> Result<Vec<RecordItem>, OutboundFailure> {
    ensure_ids(record_ids)?;

    let url = format!(
        "{}/batch_get",
        records_url(&coordinates.app_token, &coordinates.table_id).map_err(|error| {
            OutboundFailure {
                kind: FailureKind::Fatal { code: 0 },
                message: error.to_string(),
            }
        })?,
    );
    // **不投影**。《批量获取记录》的请求体只声明了 `record_ids` / `user_id_type` /
    // `with_shared_url` / `automatic_fields` 四个参数——`field_names` 只出现在它的
    // **错误表**里（`1254024 InvalidFieldNames`），没有进请求参数表。
    //
    // 拿不准时宁可不投：传一个对方不认的参数，轻则被忽略、重则整批 400，而症状会
    // 指向「字段名不匹配」——与真正的原因（参数不存在）完全无关。少投影的代价只是
    // 多返回几列，而 `RecordItem.fields` 是 `BTreeMap<String, Value>`，任何形态都吃得下。
    //
    // `automatic_fields: false` 必须留着：它挡掉 `created_by` / `created_time` 这类
    // **没有 field_id 的系统字段**，它们进了 `fields` 之后 [`rekey_cells_by_field_id`]
    // 会因为找不到对应列而报错。
    let body = serde_json::json!({
        "record_ids": record_ids,
        "automatic_fields": false,
    });

    let data: BatchGetRecordsData = send_json(
        transport,
        sleeper,
        tokens,
        OutboundMethod::Post,
        &url,
        Vec::new(),
        Some(body),
        PULL_REQUEST_TIMEOUT_SECS,
    )
    .await?;

    // 高级权限拒绝与「不存在」都显式报错，而不是静默返回空——空集会让调用方
    // 误判成「记录已删」，随后把任务置终态，而记录其实还在。
    if !data.forbidden_record_ids.is_empty() || !data.absent_record_ids.is_empty() {
        return Err(OutboundFailure {
            kind: FailureKind::Fatal { code: 0 },
            message: format!(
                "取记录被拒或记录不存在：forbidden={:?} absent={:?}",
                data.forbidden_record_ids, data.absent_record_ids
            ),
        });
    }
    Ok(data.records)
}

/// 把官方响应的 `fields`（**按字段名**作键）重映射成按 `field_id` 作键。
///
/// # 为什么这一步必须存在
///
/// 官方《批量获取记录》的示例是 `{"fields": {"单选": "选项1"}}`——键是**列名**。
/// 而本域的配置存的是 `field_id`，`applicant_open_id` 与 `build_form` 也按 id 取值。
///
/// 两边对不上的症状是**每条记录都「缺少申请人」**：不报错、不告警，只是永远停在
/// 「数据不完整」的等待态——因为按 id 去查一个按名作键的 map 永远查不到。
/// 这类静默失败只在真实跑一条时才暴露，所以判据在这里就写死。
///
/// # 列名可以变，id 不能
///
/// 重映射按**当前**字段名解析：用户改列名后 dispatch 仍然工作（配置里的 id 不变，
/// 每次现场解析）。这正是「存 id、按名匹配」这条纪律的运行期收益。
///
/// # 找不到的键一律报错，不静默保留
///
/// 静默保留一个原键，后果是域里按 id 取不到 → 又变成「缺少申请人」——把这次修复
/// 挪到别处而不是消除它。宁可在这里失败并点名是哪个键。
pub(crate) fn rekey_cells_by_field_id(
    fields: &[FieldItem],
    cells: &BTreeMap<String, serde_json::Value>,
) -> anyhow::Result<serde_json::Map<String, serde_json::Value>> {
    let mut out = serde_json::Map::new();
    let mut unknown: Vec<&str> = Vec::new();
    for (key, value) in cells {
        // 已经是 id 的直接收下：个别接口两种键都给，防御性写法没有代价。
        if fields.iter().any(|field| field.field_id == key.as_str()) {
            out.insert(key.clone(), value.clone());
            continue;
        }
        // 列名不唯一时 `resolve_field_name` 会点名报错——比取到不确定的一列强。
        match fields
            .iter()
            .find(|field| field.field_name.trim() == key.trim())
        {
            Some(field) => {
                out.insert(field.field_id.clone(), value.clone());
            }
            None => unknown.push(key),
        }
    }

    ensure!(
        unknown.is_empty(),
        "响应里的这些键既不是字段 id 也不是已知列名：{}（它们多半是未投影的系统字段，         或该列已从表里删除）",
        unknown.join("、")
    );
    Ok(out)
}

/// `records/batch_get` 的入参校验：官方单次 1~100 条，且不允许空串 id。
fn ensure_ids(record_ids: &[String]) -> Result<(), OutboundFailure> {
    // 不用 `ensure!`：它返回 `anyhow::Error`，而本函数返回 `OutboundFailure`
    // （失败分类是出站层的语义，不该在这一层被折成 `Fatal{code:0}` 以外的任何东西）。
    let reason = if record_ids.is_empty() {
        Some("record_ids 不能为空".to_string())
    } else if record_ids.len() > 100 {
        Some(format!(
            "record_ids 单次最多 100 条，实际 {}",
            record_ids.len()
        ))
    } else if record_ids.iter().any(|id| id.trim().is_empty()) {
        Some("record_ids 不得含空值".to_string())
    } else {
        None
    };
    match reason {
        Some(message) => Err(OutboundFailure {
            kind: FailureKind::Fatal { code: 0 },
            message,
        }),
        None => Ok(()),
    }
}

/// `records/batch_get` 的 `data`。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct BatchGetRecordsData {
    #[serde(default)]
    pub(crate) records: Vec<RecordItem>,
    /// 高级权限下被拒绝的记录 id。
    #[serde(default)]
    pub(crate) forbidden_record_ids: Vec<String>,
    /// 表里已经不存在的记录 id。
    #[serde(default)]
    pub(crate) absent_record_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- 审批派发：写路径 ----

    #[test]
    fn batch_update_url_targets_the_batch_update_endpoint() {
        let url = batch_update_records_url("appbcbWCzen6", "tblsRc9GRRX")
            .unwrap_or_else(|error| panic!("应可构造: {error}"));
        assert!(url.ends_with("/records/batch_update"), "{url}");
    }

    #[test]
    fn search_url_targets_the_search_endpoint() {
        let url = search_records_url("appbcbWCzen6", "tblsRc9GRRX")
            .unwrap_or_else(|error| panic!("应可构造: {error}"));
        assert!(url.ends_with("/records/search"), "{url}");
    }

    #[test]
    fn batch_update_rejects_unsafe_path_segments() {
        // 与既有 `validate_path_segment` 同一条防线：路径段里混进斜杠或查询串
        // 会让请求打到别的端点。
        assert!(batch_update_records_url("bad/token", "tbl").is_err());
        assert!(search_records_url("app", "bad?x=1").is_err());
    }

    #[test]
    fn backfill_chunk_supports_halving_to_a_single_record() {
        // 折半拆批是毒记录隔离的唯一手段，要能收敛到 1 条。取 2 的幂
        // （100 不是 2 的幂，但连续折半最终落在 1 或 2——只要能被反复整除到 ≤1 即可）。
        let mut size = BACKFILL_CHUNK;
        while size > 1 {
            size /= 2;
        }
        assert_eq!(size, 1, "折半必须能收敛到单条");
    }

    // ---- 列出数据表 ----

    #[test]
    fn tables_url_is_the_official_path() {
        // 官方《列出数据表》：GET /open-apis/bitable/v1/apps/:app_token/tables
        // 注意 `/open-apis` 这一段：既有的 records_url/fields_url 都带着它，
        // 漏掉会打到一个不存在的路径上。
        let url = tables_url("ZoCWb82JQaCCiAspCqbcUvlsnwg")
            .unwrap_or_else(|error| panic!("应可组装: {error}"));
        assert_eq!(
            url,
            format!(
                "{FEISHU_OPEN_BASE}/open-apis/bitable/v1/apps/ZoCWb82JQaCCiAspCqbcUvlsnwg/tables"
            )
        );
    }

    #[test]
    fn tables_url_rejects_path_traversal() {
        // app_token 来自用户输入；直接采信会让 `../` 把请求打到别的路径上
        assert!(tables_url("../evil").is_err());
        assert!(tables_url("").is_err());
    }

    #[test]
    fn tables_data_tolerates_a_missing_page_token() {
        // has_more 为 false 时官方不返回 page_token；写成 String 会反序列化失败
        let data: BitableTablesData = serde_json::from_str(
            r#"{"has_more":false,"total":1,"items":[{"table_id":"tblA","name":"公司往来付款"}]}"#,
        )
        .unwrap_or_else(|error| panic!("应可解析: {error}"));
        assert_eq!(data.items.len(), 1);
        assert_eq!(data.items[0].table_id, "tblA");
        assert_eq!(data.items[0].name, "公司往来付款");
        assert!(data.page_token.is_none());
    }

    // ---- 批量解析 field_id → 当前字段名 ----

    fn remote(field_id: &str, field_name: &str) -> FieldItem {
        FieldItem {
            field_id: field_id.to_string(),
            field_name: field_name.to_string(),
            field_type: Some(3),
            ui_type: Some("SingleSelect".to_string()),
            property: None,
        }
    }

    #[test]
    fn every_missing_field_id_is_named_not_just_the_first() {
        // 勾选的列被删了 → 必须一次看全要修的东西。只报第一个会让人修一轮跑一轮。
        let fields = vec![remote("fldA", "币种"), remote("fldB", "汇率")];
        let wanted = vec![
            "fldA".to_string(),
            "fldGONE".to_string(),
            "fldALSO_GONE".to_string(),
        ];
        let error = resolve_field_names(&fields, &wanted)
            .err()
            .unwrap_or_else(|| panic!("缺字段应报错"));
        let message = error.to_string();
        assert!(message.contains("fldGONE"), "实际: {message}");
        assert!(
            message.contains("fldALSO_GONE"),
            "要一次报全，实际: {message}"
        );
    }

    #[test]
    fn resolution_returns_the_current_name_so_a_rename_does_not_break_the_link() {
        // 存 field_id 的全部意义：字段改名后仍解析得出当前名字
        let fields = vec![remote("fld6DuK6tM", "币种/Currency（单选）（已改名）")];
        let resolved = resolve_field_names(&fields, &["fld6DuK6tM".to_string()])
            .unwrap_or_else(|error| panic!("应可解析: {error}"));
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].0, "fld6DuK6tM");
        assert_eq!(resolved[0].1, "币种/Currency（单选）（已改名）");
    }

    #[test]
    fn the_order_matches_the_input_order() {
        // 顺序稳定才有对照价值：日志、快照摘要、`field_names` 都按它拼
        let fields = vec![
            remote("fldB", "汇率"),
            remote("fldA", "币种"),
            remote("fldC", "费用类型"),
        ];
        let wanted = vec!["fldC".to_string(), "fldA".to_string(), "fldB".to_string()];
        let resolved = resolve_field_names(&fields, &wanted)
            .unwrap_or_else(|error| panic!("应可解析: {error}"));
        assert_eq!(
            resolved
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            vec!["fldC", "fldA", "fldB"]
        );
    }

    #[test]
    fn a_name_that_collides_with_another_column_is_rejected() {
        // `field_names` 是**按名字匹配**的：表里有两列同名就取到不确定的那一列。
        // 单列版 resolve_field_name 已经拦这条，批量版不能漏。
        let fields = vec![remote("fldA", "费用类型"), remote("fldB", "费用类型")];
        assert!(
            resolve_field_names(&fields, &["fldA".to_string()]).is_err(),
            "重名必须被拦在拉取之前"
        );
    }

    #[test]
    fn an_empty_request_yields_an_empty_result() {
        // 没勾任何列是合法输入（调用方另有非空校验），这里不该报错
        let resolved =
            resolve_field_names(&[], &[]).unwrap_or_else(|error| panic!("空输入不该报错: {error}"));
        assert!(resolved.is_empty());
    }

    #[test]
    fn a_blank_field_name_counts_as_missing() {
        // 名字被清空的列没法喂给 `field_names`，等同于不存在
        let fields = vec![remote("fldA", "   ")];
        assert!(resolve_field_names(&fields, &["fldA".to_string()]).is_err());
    }

    // ---- 列出视图 ----

    #[test]
    fn views_url_matches_the_official_path() {
        // 官方《列出视图》：GET /open-apis/bitable/v1/apps/:app_token/tables/:table_id/views
        let url = views_url("ZoCWb82JQaCCiAspCqbcUvlsnwg", "tblauuOafa4acvT3")
            .unwrap_or_else(|error| panic!("应可组装: {error}"));
        assert_eq!(
            url,
            format!(
                "{FEISHU_OPEN_BASE}/open-apis/bitable/v1/apps/ZoCWb82JQaCCiAspCqbcUvlsnwg/tables/tblauuOafa4acvT3/views"
            )
        );
    }

    #[test]
    fn views_url_rejects_path_traversal_in_either_segment() {
        assert!(views_url("../evil", "tblA").is_err());
        assert!(views_url("appA", "../evil").is_err());
    }

    #[test]
    fn fields_url_does_not_send_view_id() {
        // 【实测 2026-09-23】目标台账上带与不带 `view_id` 各调一次「列出字段」，
        // 两次返回**完全相同的 30 个字段与顺序**。视图只管「拉哪些行」，不管「有哪些列」。
        // 这条把「不要发它」钉住，免得有人照着官方参数字段表加回去。
        let url = fields_url("appA", "tblA").unwrap_or_else(|error| panic!("应可组装: {error}"));
        assert!(!url.contains("view_id"), "实际: {url}");
    }

    #[test]
    fn views_data_tolerates_a_missing_page_token() {
        let data: BitableViewsData = serde_json::from_str(
            r#"{"has_more":false,"total":1,"items":[{"view_id":"vewAEKSbvO","view_name":"表格 1","view_type":"grid"}]}"#,
        )
        .unwrap_or_else(|error| panic!("应可解析: {error}"));
        assert_eq!(data.items.len(), 1);
        assert_eq!(data.items[0].view_id, "vewAEKSbvO");
        assert_eq!(data.items[0].view_type, "grid");
        assert!(data.page_token.is_none());
    }

    // ---- 路径段与 URL ----

    #[test]
    fn accepts_realistic_tokens_and_ids() {
        for value in [
            "bascnCMII2ORej2RItqpZZUNMIe",
            "tblxI2tWaxP5dG7p",
            "vewqhz51lk",
        ] {
            assert!(
                validate_path_segment("x", value).is_ok(),
                "{value} 应被接受"
            );
        }
    }

    #[test]
    fn rejects_path_traversal_and_odd_shapes() {
        for value in [
            "../etc/passwd",
            "a/b",
            "a b",
            "a?b",
            "中文",
            "",
            "a\tb",
            "a:b",
        ] {
            assert!(
                validate_path_segment("x", value).is_err(),
                "{value:?} 必须被拒绝"
            );
        }
        assert!(validate_path_segment("x", &"a".repeat(MAX_PATH_SEGMENT_LEN + 1)).is_err());
    }

    #[test]
    fn records_url_validates_both_path_segments() {
        assert_eq!(
            records_url("bascnCMII2ORej2RItqpZZUNMIe", "tblxI2tWaxP5dG7p")
                .unwrap_or_else(|error| panic!("应可组装: {error}")),
            "https://open.feishu.cn/open-apis/bitable/v1/apps/bascnCMII2ORej2RItqpZZUNMIe/tables/tblxI2tWaxP5dG7p/records"
        );
        // 证明校验真的落在 URL 组装上，而不是只存在于一个没人调的函数里
        assert!(
            records_url("../x", "tblx").is_err(),
            "app_token 里的路径穿越必须被拦在组装函数内部"
        );
        assert!(records_url("bascn", "../x").is_err(), "table_id 同理");
    }

    // ---- 查询参数 ----

    #[test]
    fn field_names_is_a_single_json_array_value() {
        let query = list_records_query(
            &["字段1".to_string(), "字段2".to_string()],
            None,
            None,
            PAGE_SIZE,
        )
        .unwrap_or_else(|error| panic!("应可组装: {error}"));

        let names: Vec<&String> = query
            .iter()
            .filter(|(key, _)| key == "field_names")
            .map(|(_, value)| value)
            .collect();
        assert_eq!(names.len(), 1, "必须是**一个** JSON 数组值，不是重复参数");
        assert_eq!(names[0], r#"["字段1","字段2"]"#);
    }

    #[test]
    fn page_size_uses_the_documented_maximum() {
        let query = list_records_query(&[], None, None, PAGE_SIZE)
            .unwrap_or_else(|error| panic!("应可组装: {error}"));
        let page_size = query
            .iter()
            .find(|(key, _)| key == "page_size")
            .map(|(_, value)| value.clone())
            .unwrap_or_default();
        assert_eq!(page_size, "500", "官方上限是 500，不是审批后台的 100");
    }

    #[test]
    fn page_token_is_passed_through_without_shape_validation() {
        // 游标是 base64，含 = + /，白名单校验会把合法游标判非法
        let cursor = "recn0hoyXL+/==".to_string();
        let query = list_records_query(&[], None, Some(&cursor), PAGE_SIZE)
            .unwrap_or_else(|error| panic!("应可组装: {error}"));
        assert!(query
            .iter()
            .any(|(key, value)| key == "page_token" && value == &cursor));
    }

    #[test]
    fn view_id_is_validated_before_being_placed_in_the_query() {
        assert!(list_records_query(&[], Some("../x"), None, PAGE_SIZE).is_err());
        let query = list_records_query(&[], Some("vewqhz51lk"), None, PAGE_SIZE)
            .unwrap_or_else(|error| panic!("应可组装: {error}"));
        assert!(query
            .iter()
            .any(|(key, value)| key == "view_id" && value == "vewqhz51lk"));
    }

    #[test]
    fn zero_page_size_is_rejected() {
        assert!(list_records_query(&[], None, None, 0).is_err());
    }

    // ---- 字段名解析 ----

    /// 造一个字段项。默认 `type: 3`（单选）——名字解析类的用例不关心类型。
    fn field(field_id: &str, field_name: &str) -> FieldItem {
        FieldItem {
            field_id: field_id.to_string(),
            field_name: field_name.to_string(),
            field_type: Some(3),
            ui_type: Some("SingleSelect".to_string()),
            property: None,
        }
    }

    fn fields() -> Vec<FieldItem> {
        vec![
            field("fldA", "费用大类/Main Exp Cat*"),
            field("fldB", "费用类型/Fee Type*"),
        ]
    }

    #[test]
    fn resolves_field_id_to_exact_name() {
        assert_eq!(
            resolve_field_name(&fields(), "fldA")
                .unwrap_or_else(|error| panic!("应能解析: {error}")),
            "费用大类/Main Exp Cat*"
        );
    }

    #[test]
    fn missing_field_id_is_an_error_not_an_empty_name() {
        let error = resolve_field_name(&fields(), "fldNope")
            .err()
            .unwrap_or_else(|| panic!("找不到必须报错"));
        assert!(error.to_string().contains("fldNope"), "实际: {error}");
    }

    #[test]
    fn duplicate_field_names_are_rejected() {
        // 官方按名字匹配；重名列会取到不确定的那一列（实测同表确有两列同名不同 id）
        let duplicated = vec![field("fldA", "同名"), field("fldB", "同名")];
        let error = resolve_field_name(&duplicated, "fldA")
            .err()
            .unwrap_or_else(|| panic!("重名必须报错"));
        assert!(error.to_string().contains("不唯一"), "实际: {error}");
    }

    #[test]
    fn blank_field_name_is_rejected() {
        let blank = vec![field("fldA", "   ")];
        assert!(resolve_field_name(&blank, "fldA").is_err());
    }

    // ---- 取数列类型校验 ----

    fn field_of_type(field_type: i32, ui_type: Option<&str>) -> FieldItem {
        FieldItem {
            field_id: "fldX".to_string(),
            field_name: "取数列".to_string(),
            field_type: Some(field_type),
            ui_type: ui_type.map(str::to_string),
            property: None,
        }
    }

    #[test]
    fn single_value_field_types_are_accepted() {
        // 官方《列出字段》的类型码：1 文本 / 2 数字 / 3 单选 / 5 日期 /
        // 13 电话 / 15 超链接 / 20 公式 / 1005 自动编号
        for field_type in [1, 2, 3, 5, 13, 15, 20, 1005] {
            assert!(
                is_single_value_field_type(field_type),
                "type={field_type} 是单值字段，应被接受"
            );
            assert!(
                check_coordinate_field(&field_of_type(field_type, None)).is_ok(),
                "type={field_type} 的校验应通过"
            );
        }
    }

    #[test]
    fn multi_value_and_textless_field_types_are_rejected() {
        // 4 多选 / 7 复选框 / 11 人员 / 17 附件 / 18 关联 / 21 双向关联 /
        // 22 地理位置 / 23 群组 —— 单元格不是一个可用的选项值
        for field_type in [4, 7, 11, 17, 18, 21, 22, 23] {
            assert!(!is_single_value_field_type(field_type));
            let error = check_coordinate_field(&field_of_type(field_type, Some("MultiSelect")))
                .err()
                .unwrap_or_else(|| panic!("type={field_type} 必须被拒绝"));
            let text = error.to_string();
            assert!(
                text.contains("不是单值字段"),
                "报错要说清原因，实际: {text}"
            );
        }
    }

    #[test]
    fn missing_field_type_does_not_block_the_configuration() {
        // 拿不到元数据时宁可放行：让真实拉取去暴露问题，比在这里猜更准
        let unknown = FieldItem {
            field_id: "fldX".to_string(),
            field_name: "取数列".to_string(),
            field_type: None,
            ui_type: None,
            property: None,
        };
        assert!(check_coordinate_field(&unknown).is_ok());
    }

    #[test]
    fn field_type_check_reports_a_readable_name() {
        let single = field_of_type(3, Some("SingleSelect"));
        assert_eq!(
            check_coordinate_field(&single).unwrap_or_default(),
            "SingleSelect"
        );
        // 没有 ui_type 时退回类型码，不返回空串
        let bare = field_of_type(1, None);
        assert!(check_coordinate_field(&bare)
            .unwrap_or_default()
            .contains("1"));
    }

    // ---- 单元格取值 ----

    #[test]
    fn string_cell_is_text() {
        assert_eq!(cell_label(&json!("选项1")), CellValue::Text("选项1".into()));
    }

    #[test]
    fn single_select_returned_as_a_one_element_array_is_also_accepted() {
        // 官方示例写裸字符串，实测 lark-cli 抓到的真实数据是长度 1 的数组；
        // 两个方向都吃，写错方向是静默丢数据
        assert_eq!(
            cell_label(&json!(["股东借款"])),
            CellValue::Text("股东借款".into())
        );
    }

    #[test]
    fn multi_value_arrays_are_unsupported_not_silently_truncated() {
        // 取数列必须指向单值列；静默取第一个会让多选列悄悄产出看起来正常的选项集
        assert_eq!(cell_label(&json!(["A", "B"])), CellValue::Unsupported);
        assert_eq!(cell_label(&json!([])), CellValue::Empty);
    }

    #[test]
    fn numeric_cells_arrive_as_strings_on_the_list_endpoint() {
        // 官方《列出记录》示例：数字 "100"、货币 "1"、进度 "0.66"、评分 "3"
        for value in ["100", "0.66", "3"] {
            assert_eq!(cell_label(&json!(value)), CellValue::Text(value.into()));
        }
        // 若将来走《查询记录》，同样的列会以 number 出现——一并支持
        assert_eq!(cell_label(&json!(0.66)), CellValue::Text("0.66".into()));
        assert_eq!(cell_label(&json!(3)), CellValue::Text("3".into()));
    }

    #[test]
    fn formula_and_lookup_objects_yield_their_text() {
        assert_eq!(
            cell_label(&json!([{"text": "多行文本内容1", "type": "text"}])),
            CellValue::Text("多行文本内容1".into())
        );
        assert_eq!(
            cell_label(&json!({"text": "链接文案", "type": "text"})),
            CellValue::Text("链接文案".into())
        );
    }

    #[test]
    fn empty_values_are_empty_not_unsupported() {
        assert_eq!(cell_label(&serde_json::Value::Null), CellValue::Empty);
        assert_eq!(cell_label(&json!("")), CellValue::Empty);
        assert_eq!(cell_label(&json!("   ")), CellValue::Empty);
    }

    #[test]
    fn person_and_location_columns_are_unsupported_not_empty() {
        // 这几种对象**不含 text 键**；返回 Empty 会让「取数列选错类型」静默产出空标签
        let person = json!([{
            "avatar_url": "https://example.invalid/a.png",
            "email": "a@example.com",
            "en_name": "ZhangSan",
            "id": "ou_2910",
            "name": "张三"
        }]);
        assert_eq!(cell_label(&person), CellValue::Unsupported);

        let location = json!({
            "address": "东长安街",
            "cityname": "北京市",
            "name": "天安门广场"
        });
        assert_eq!(cell_label(&location), CellValue::Unsupported);

        assert_eq!(cell_label(&json!(true)), CellValue::Unsupported);
    }

    // ---- 分页 ----

    #[test]
    fn converges_across_two_pages() {
        let mut state = PaginationState::new(MAX_PAGES);
        let next = state
            .accept(true, Some("cursor-1".to_string()), Some(7), 5)
            .unwrap_or_else(|error| panic!("第一页应被接受: {error}"));
        assert_eq!(next.as_deref(), Some("cursor-1"));

        let end = state
            .accept(false, None, Some(7), 2)
            .unwrap_or_else(|error| panic!("第二页应被接受: {error}"));
        assert_eq!(end, None, "has_more=false 表示耗尽");
        assert!(state.assert_converged().is_ok());
        assert_eq!(state.accumulated(), 7);
    }

    #[test]
    fn missing_cursor_with_has_more_must_fail() {
        let mut state = PaginationState::new(MAX_PAGES);
        assert!(state.accept(true, None, Some(7), 5).is_err());
        assert!(state.accept(true, Some(String::new()), Some(7), 5).is_err());
    }

    #[test]
    fn duplicate_cursor_must_fail_instead_of_looping_forever() {
        let mut state = PaginationState::new(MAX_PAGES);
        state
            .accept(true, Some("same".to_string()), Some(9), 5)
            .unwrap_or_else(|error| panic!("首次应被接受: {error}"));
        let error = state
            .accept(true, Some("same".to_string()), Some(9), 5)
            .err()
            .unwrap_or_else(|| panic!("重复游标必须失败"));
        assert!(error.to_string().contains("重复"), "实际: {error}");
    }

    #[test]
    fn page_cap_stops_a_non_advancing_cursor() {
        let mut state = PaginationState::new(2);
        state
            .accept(true, Some("c1".to_string()), Some(100), 5)
            .unwrap_or_else(|error| panic!("第一页应被接受: {error}"));
        state
            .accept(true, Some("c2".to_string()), Some(100), 5)
            .unwrap_or_else(|error| panic!("第二页应被接受: {error}"));
        assert!(state
            .accept(true, Some("c3".to_string()), Some(100), 5)
            .is_err());
    }

    #[test]
    fn convergence_rejects_missing_negative_or_truncated_totals() {
        let mut missing = PaginationState::new(MAX_PAGES);
        missing
            .accept(false, None, None, 3)
            .unwrap_or_else(|error| panic!("应被接受: {error}"));
        assert!(missing.assert_converged().is_err(), "缺 total 无法证明完整");

        let mut negative = PaginationState::new(MAX_PAGES);
        negative
            .accept(false, None, Some(-1), 3)
            .unwrap_or_else(|error| panic!("应被接受: {error}"));
        assert!(
            negative.assert_converged().is_err(),
            "total 为负时 `as usize` 会变成极大数，必须显式拒绝"
        );

        let mut truncated = PaginationState::new(MAX_PAGES);
        truncated
            .accept(false, None, Some(10), 4)
            .unwrap_or_else(|error| panic!("应被接受: {error}"));
        assert!(
            truncated.assert_converged().is_err(),
            "累计少于 total 是不完整快照"
        );
    }

    #[test]
    fn convergence_tolerates_rows_added_during_paging() {
        // 官方未承诺 total == 收到的行数之和；翻页途中并发新增会让 `==` 恒假，
        // 把守卫变成持续告警的噪声源。累计 ≥ total 即可。
        let mut state = PaginationState::new(MAX_PAGES);
        state
            .accept(false, None, Some(3), 5)
            .unwrap_or_else(|error| panic!("应被接受: {error}"));
        assert!(state.assert_converged().is_ok());
    }

    // ---- DTO 形状 ----

    #[test]
    fn records_data_parses_when_page_token_is_absent() {
        // has_more=false 时官方不返回 page_token；写成 String 会反序列化失败
        let data: ListRecordsData = serde_json::from_str(
            r#"{"has_more":false,"total":1,"items":[{"record_id":"rec1","fields":{"单选":"选项1"}}]}"#,
        )
        .unwrap_or_else(|error| panic!("应可解析: {error}"));
        assert!(!data.has_more);
        assert_eq!(data.page_token, None);
        assert_eq!(data.total, Some(1));
        assert_eq!(data.items.len(), 1);
        assert_eq!(
            data.items[0].fields.get("单选"),
            Some(&json!("选项1")),
            "fields 是 map<string,union>，按 Value 取值"
        );
    }

    #[test]
    fn fields_data_parses_with_paging_fields() {
        let data: ListFieldsData = serde_json::from_str(
            r#"{"has_more":true,"page_token":"c1","total":30,"items":[{"field_id":"fldA","field_name":"名称"}]}"#,
        )
        .unwrap_or_else(|error| panic!("应可解析: {error}"));
        assert!(data.has_more);
        assert_eq!(data.page_token.as_deref(), Some("c1"));
        assert_eq!(data.items[0].field_name, "名称");
    }

    #[test]
    fn envelope_parses_with_data() {
        let envelope: FeishuApiEnvelope = serde_json::from_str(
            r#"{"code":0,"msg":"success","data":{"has_more":false,"total":0}}"#,
        )
        .unwrap_or_else(|error| panic!("应可解析: {error}"));
        assert_eq!(envelope.code, 0);
        assert!(envelope.data.is_some());
    }
}

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

/// 组装列出字段的查询参数。
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
async fn fetch_page<T: for<'de> Deserialize<'de>>(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    url: &str,
    query: Vec<(String, String)>,
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
            method: OutboundMethod::Get,
            url: url.to_string(),
            query: query.clone(),
            bearer_token: Some(token),
            json_body: None,
            timeout_secs: Some(PULL_REQUEST_TIMEOUT_SECS),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

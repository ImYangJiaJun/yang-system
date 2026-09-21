//! 飞书审批「关联外部选项」接口的请求与响应契约。
//!
//! 键名逐字对齐飞书官方文档，**不经过任何框架包络**：顶层是 `{code, msg, data}`，
//! `data.result` 在未配置 Key 时是对象、配置 Key 后是 base64 字符串。
//!
//! # 临时豁免
//!
//! `#![allow(dead_code)]` 是**临时**的：本文件的类型要被
//! `option/actions/approval_options.rs`（外部选项端点）消费，而该端点尚未落地。
//! 端点提交时**必须删除这一行**——它不是长期豁免，只是为了让中间提交也能过
//! `clippy -D warnings`。 `dead_code` 在端点落地后会自然消失。
#![allow(dead_code)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use yang_base::definition::{ParamInput, Params};
use yang_base::BaseError;

/// 外部选项接口的请求体。
///
/// 字段名与飞书文档逐字一致。**刻意不设 `deny_unknown_fields`**：飞书将来新增字段
/// 不应打挂我们，而该 DTO 没有任何内部字段可供注入——在这里拒绝未知字段只有可用性
/// 风险，没有安全收益。
///
/// `decode` 用 `ParamInput` 的默认实现（把请求体当 JSON 反序列化）。这是刻意的：
/// `params!` 宏表达不了 `linkage_params` 这个 Map（它只支持标量类 builder），
/// 而手写 `decode` 反而要处理 trait 默认实现上 `where Self: DeserializeOwned`
/// 子句的坑。默认实现正好就是我们要的语义。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct FeishuOptionsRequest {
    /// 内部 ID。飞书文档推荐改用 `employee_id`；两者都空表示期望返回全部数据。
    #[serde(default)]
    pub(crate) user_id: Option<String>,
    /// 用户的 user_id；发起审批时是发起人。
    #[serde(default)]
    pub(crate) employee_id: Option<String>,
    /// 用于校验请求来源是否合法的自定义取值（文档中唯一标为必填的请求参数）。
    pub(crate) token: String,
    /// 联动选项参数。v1 收到即忽略，仅在数据模型上预留。
    #[serde(default)]
    pub(crate) linkage_params: Option<BTreeMap<String, String>>,
    /// 分页标记；不传或为空表示从第一页开始。
    #[serde(default)]
    pub(crate) page_token: Option<String>,
    /// 搜索关键词。
    #[serde(default)]
    pub(crate) query: Option<String>,
    /// 语言环境：`zh_cn` / `en_us` / `ja_jp`。
    #[serde(default)]
    pub(crate) locale: Option<String>,
}

impl ParamInput for FeishuOptionsRequest {
    fn params() -> Params {
        // 该 DTO 只当传输层契约用，参数元数据是空集合（与 list_permissions.rs 同形）。
        Params::new()
    }
}

/// 未配置 Key 时 `data.result` 的内容。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FeishuResultBody {
    /// 选项列表。
    pub(crate) options: Vec<FeishuOption>,
    /// 国际化文案；**必须至少有一项**，否则飞书侧控件显示为空。
    #[serde(rename = "i18nResources")]
    pub(crate) i18n_resources: Vec<FeishuI18nResource>,
    /// 是否有下一页。
    #[serde(rename = "hasMore")]
    pub(crate) has_more: bool,
    /// 下一页游标；仅当 `has_more` 为 true 时输出。
    #[serde(rename = "nextPageToken", skip_serializing_if = "Option::is_none")]
    pub(crate) next_page_token: Option<String>,
}

impl FeishuResultBody {
    /// 空结果集；`i18nResources` 的「至少一种语言」由构造方保证。
    pub(crate) fn empty() -> Self {
        Self {
            options: Vec::new(),
            i18n_resources: Vec::new(),
            has_more: false,
            next_page_token: None,
        }
    }
}

/// 单个选项（飞书文档中的 `externalData`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FeishuOption {
    /// 选项唯一标识，全局唯一且固定。
    pub(crate) id: String,
    /// 用于到 `i18nResources.texts` 中匹配显示文案的键。
    pub(crate) value: String,
    /// 是否为默认选项。
    #[serde(rename = "isDefault", skip_serializing_if = "Option::is_none")]
    pub(crate) is_default: Option<bool>,
}

/// 国际化文案（飞书文档中的 `i18nResource`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FeishuI18nResource {
    /// `zh_cn` / `en_us` / `ja_jp`。
    pub(crate) locale: String,
    /// 是否为默认语言。
    #[serde(rename = "isDefault")]
    pub(crate) is_default: bool,
    /// 键为 `@i18n@<option_id>`，值为该语言下的文案。
    pub(crate) texts: BTreeMap<String, String>,
}

/// `data.result`：未配置 Key 时是对象，配置 Key 后是 base64 密文字符串。
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub(crate) enum FeishuResult {
    /// 明文结果。
    Plain(FeishuResultBody),
    /// 加密结果：整体 base64（`IV ‖ 密文`）。
    Encrypted(String),
}

/// `data` 包装：把 `result` 嵌在 `data` 下，与文档示例一致。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FeishuData {
    /// 请求结果的内容。
    pub(crate) result: FeishuResult,
}

/// 外部选项接口的响应包络：`{code, msg, data}`。
///
/// 这是本服务唯一不使用框架 `ApiResponse` 包络的响应类型——飞书契约的顶层键是
/// `msg` 而不是 `message`，且框架不允许改键名。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FeishuEnvelope {
    /// 错误码，非 0 表示失败。
    pub(crate) code: i32,
    /// 返回码的描述。
    pub(crate) msg: String,
    /// 返回业务信息。失败时为 `null`。
    pub(crate) data: Option<FeishuData>,
}

impl FeishuEnvelope {
    /// 成功（明文）。
    pub(crate) fn ok(body: FeishuResultBody) -> Self {
        Self {
            code: 0,
            msg: "success!".to_string(),
            data: Some(FeishuData {
                result: FeishuResult::Plain(body),
            }),
        }
    }

    /// 成功（加密）。
    pub(crate) fn encrypted(cipher_base64: String) -> Self {
        Self {
            code: 0,
            msg: "success!".to_string(),
            data: Some(FeishuData {
                result: FeishuResult::Encrypted(cipher_base64),
            }),
        }
    }

    /// 业务失败。飞书只依据 `code` 判定成败。
    pub(crate) fn fail(code: i32, msg: impl Into<String>) -> Self {
        Self {
            code,
            msg: msg.into(),
            data: None,
        }
    }

    /// 序列化为响应体文本，供 `ResponseBody::raw` 使用。
    pub(crate) fn to_json(&self) -> Result<String, BaseError> {
        serde_json::to_string(self)
            .map_err(|error| BaseError::JsonSerializeFailed(error.to_string()))
    }

    /// 序列化为 [`Value`]，供需要结构化访问的调用方使用。
    pub(crate) fn to_value(&self) -> Result<Value, BaseError> {
        serde_json::to_value(self)
            .map_err(|error| BaseError::JsonSerializeFailed(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 序列化响应包络为文本。
    fn envelope_json(envelope: &FeishuEnvelope) -> String {
        envelope
            .to_json()
            .unwrap_or_else(|error| panic!("包络应可序列化: {error}"))
    }

    /// 把文本解析成 JSON 值。
    fn parse(text: &str) -> serde_json::Value {
        serde_json::from_str(text).unwrap_or_else(|error| panic!("应是合法 JSON: {error}"))
    }

    /// 序列化任意可序列化值为文本。
    fn to_text<T: Serialize>(value: &T) -> String {
        serde_json::to_string(value).unwrap_or_else(|error| panic!("应可序列化: {error}"))
    }

    /// 以请求体构造并解码飞书请求 DTO。
    fn decode_request(body: serde_json::Value) -> Result<FeishuOptionsRequest, BaseError> {
        let mut request = yang_base::action::Request::new(body);
        FeishuOptionsRequest::decode(&mut request)
    }

    #[test]
    fn envelope_uses_msg_key_not_message() {
        // 飞书契约的顶层键是 msg；框架的 message 在这里必须不出现
        let text = envelope_json(&FeishuEnvelope::ok(FeishuResultBody::empty()));
        assert!(text.contains(r#""msg":"success!""#), "实际: {text}");
        assert!(
            !text.contains(r#""message""#),
            "不得出现框架的 message 键: {text}"
        );
    }

    #[test]
    fn plain_result_is_nested_under_data_result() {
        // 形状必须是 {"code":0,"msg":"...","data":{"result":{...}}}
        let value = parse(&envelope_json(&FeishuEnvelope::ok(
            FeishuResultBody::empty(),
        )));
        assert_eq!(value["code"], json!(0));
        assert!(
            value["data"]["result"].is_object(),
            "result 应为对象: {value}"
        );
        assert!(value["data"]["result"]["options"].is_array());
        assert!(value["data"]["result"]["i18nResources"].is_array());
    }

    #[test]
    fn encrypted_result_is_a_string() {
        // 配置 Key 后 result 是 base64 字符串，不是对象
        let value = parse(&envelope_json(&FeishuEnvelope::encrypted(
            "dEs0TQ==".to_string(),
        )));
        assert_eq!(value["data"]["result"], json!("dEs0TQ=="));
    }

    #[test]
    fn failure_envelope_has_null_data_and_nonzero_code() {
        let value = parse(&envelope_json(&FeishuEnvelope::fail(
            40102,
            "token 校验失败",
        )));
        assert_ne!(value["code"], json!(0));
        assert_eq!(value["msg"], json!("token 校验失败"));
        assert_eq!(value["data"], serde_json::Value::Null);
    }

    #[test]
    fn option_uses_camel_case_is_default() {
        // externalData 的字段是 isDefault（camelCase）
        let text = to_text(&FeishuOption {
            id: "dept_sales".to_string(),
            value: "@i18n@dept_sales".to_string(),
            is_default: Some(true),
        });
        assert!(text.contains(r#""isDefault":true"#), "实际: {text}");
    }

    #[test]
    fn option_omits_is_default_when_absent() {
        // 非默认选项不必输出 isDefault，避免与文档示例产生无谓差异
        let text = to_text(&FeishuOption {
            id: "dept_rd".to_string(),
            value: "@i18n@dept_rd".to_string(),
            is_default: None,
        });
        assert!(!text.contains("isDefault"), "实际: {text}");
    }

    #[test]
    fn i18n_resource_uses_camel_case_is_default() {
        let mut texts = BTreeMap::new();
        texts.insert("@i18n@dept_sales".to_string(), "销售部".to_string());
        let text = to_text(&FeishuI18nResource {
            locale: "zh_cn".to_string(),
            is_default: true,
            texts,
        });
        assert!(text.contains(r#""isDefault":true"#), "实际: {text}");
        assert!(text.contains(r#""locale":"zh_cn""#), "实际: {text}");
    }

    #[test]
    fn result_body_omits_next_page_token_when_absent() {
        // hasMore 为 false 时不返回 nextPageToken
        let text = to_text(&FeishuResultBody::empty());
        assert!(!text.contains("nextPageToken"), "实际: {text}");
        assert!(text.contains(r#""hasMore":false"#), "实际: {text}");
        assert!(text.contains(r#""i18nResources":[]"#), "实际: {text}");
    }

    #[test]
    fn request_decodes_all_documented_fields() {
        // 请求字段逐字对齐飞书
        let decoded = match decode_request(json!({
            "user_id": "123",
            "employee_id": "abc",
            "token": "t0ken",
            "linkage_params": {"key1": "value1"},
            "page_token": "cursor",
            "query": "北京",
            "locale": "zh_cn"
        })) {
            Ok(decoded) => decoded,
            Err(error) => panic!("应可解码: {error}"),
        };
        assert_eq!(decoded.user_id.as_deref(), Some("123"));
        assert_eq!(decoded.employee_id.as_deref(), Some("abc"));
        assert_eq!(decoded.token, "t0ken");
        assert_eq!(
            decoded
                .linkage_params
                .as_ref()
                .and_then(|map| map.get("key1"))
                .map(String::as_str),
            Some("value1")
        );
        assert_eq!(decoded.page_token.as_deref(), Some("cursor"));
        assert_eq!(decoded.query.as_deref(), Some("北京"));
        assert_eq!(decoded.locale.as_deref(), Some("zh_cn"));
    }

    #[test]
    fn request_accepts_a_body_with_only_the_required_field() {
        // 其余字段全部可选：只带 token 也必须能解码
        assert!(
            decode_request(json!({"token": "t0ken"})).is_ok(),
            "只带 token 应可解码"
        );
    }

    #[test]
    fn request_tolerates_fields_feishu_may_add_later() {
        // 刻意不设 deny_unknown_fields：飞书将来新增字段不该打挂我们
        assert!(
            decode_request(json!({
                "employee_id": "abc",
                "token": "t0ken",
                "future_field_added_by_feishu": "ignored"
            }))
            .is_ok(),
            "未知字段必须被容忍"
        );
    }

    #[test]
    fn request_requires_token() {
        // token 是文档里唯一被标为「是」的请求参数
        assert!(
            decode_request(json!({"employee_id": "abc"})).is_err(),
            "缺少 token 必须失败"
        );
    }
}

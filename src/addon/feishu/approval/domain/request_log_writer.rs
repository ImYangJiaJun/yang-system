//! 派发请求记录：outcome 四桶归类与记录行组装。
//!
//! 纯逻辑层，不碰任何表——归类与组装都可离线单测；落库（独立连接自动提交）
//! 由 `actions/dispatch.rs` 的 `write_request_log` 执行，写失败只降级日志，
//! 绝不影响派发结果（设计 §3 的写入点）。

use yang_base::table::Record;

use crate::addon::feishu::domain::approval_dispatch::DispatchResult;

/// outcome 四桶，与 `request_log_table.rs` 的 Radio 选项逐字对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// 单条创建成功（Backfilled）。
    Succeeded,
    /// 单条必填缺失、本轮未处理（Waiting）。
    Waiting,
    /// 批量受理（accepted）。
    Accepted,
    /// 失败（参数校验 / 配置 / 凭证 / 业务失败）。
    Failed,
}

impl Outcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Waiting => "waiting",
            Self::Accepted => "accepted",
            Self::Failed => "failed",
        }
    }
}

/// 请求体未带 `requested_by`（或 trim 后为空）时落库的请求人。
pub(crate) const DEFAULT_REQUESTED_BY: &str = "feishu-workflow";

/// 请求人归一：trim 后空串按缺失处理，缺省记 `feishu-workflow`。
pub(crate) fn normalized_requested_by(input: Option<&str>) -> String {
    input
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| DEFAULT_REQUESTED_BY.to_string())
}

/// 单条处理结果 → 结果四桶（设计 §4.2）。
///
/// `Backfilled` → succeeded；`Waiting` → waiting；
/// `Terminal` 与 `Retryable` → failed（可重试失败也算失败——它**没有**被受理，
/// 批量语义里只有 `accepted` 一个受理桶，见设计 §4.2）。
pub(crate) fn outcome_for(result: &DispatchResult) -> Outcome {
    match result {
        DispatchResult::Backfilled { .. } => Outcome::Succeeded,
        DispatchResult::Waiting { .. } => Outcome::Waiting,
        DispatchResult::Terminal { .. } | DispatchResult::Retryable { .. } => Outcome::Failed,
    }
}

/// 一次请求的记录内容：从请求与结果抽取的纯值，经 [`RequestLog::into_record`]
/// 折成落库行。
pub(crate) struct RequestLog {
    pub(crate) requested_by: String,
    pub(crate) base_token: String,
    pub(crate) table_id: String,
    /// 配置存在或建成后关联；校验失败与配置未建成（40401/35600）的请求为空。
    pub(crate) config_id: Option<i64>,
    pub(crate) record_id: Option<String>,
    /// `DispatchInput` 的 serde_json 序列化原文。
    pub(crate) request_body: String,
    pub(crate) outcome: Outcome,
    /// 结果说明（复用响应 message；失败时为可行动原因）。
    pub(crate) message: String,
    pub(crate) serial_number: Option<String>,
    /// 返回信封 `data` 的 JSON 原文；无 data（失败出口）时为 `None`（落 NULL）。
    pub(crate) response_body: Option<String>,
}

impl RequestLog {
    /// 组装成落库 Record，键与 `request_log_table.rs` 的字段一一对应。
    ///
    /// 可空列缺省即不插键（落 NULL）；`created_at` 由表的 `created_at` 时间戳
    /// 注入，`id` 自增。
    pub(crate) fn into_record(self) -> Record {
        let mut row = Record::new();
        row.insert("requested_by", serde_json::json!(self.requested_by));
        row.insert("base_token", serde_json::json!(self.base_token));
        row.insert("table_id", serde_json::json!(self.table_id));
        if let Some(config_id) = self.config_id {
            row.insert("config_id", serde_json::json!(config_id));
        }
        if let Some(record_id) = self.record_id {
            row.insert("record_id", serde_json::json!(record_id));
        }
        row.insert("request_body", serde_json::json!(self.request_body));
        row.insert("outcome", serde_json::json!(self.outcome.as_str()));
        row.insert("message", serde_json::json!(self.message));
        if let Some(serial_number) = self.serial_number {
            row.insert("serial_number", serde_json::json!(serial_number));
        }
        if let Some(response_body) = self.response_body {
            row.insert("response_body", serde_json::json!(response_body));
        }
        row
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    /// 四桶映射的每一行：一个 `DispatchResult` 变体 → 一个桶 + 落库文本。
    #[test]
    fn outcome_mapping_covers_all_four_buckets() {
        let cases = [
            (
                DispatchResult::Backfilled {
                    serial_number: "202609280001".to_string(),
                },
                Outcome::Succeeded,
                "succeeded",
            ),
            (
                DispatchResult::Waiting {
                    reason: "缺少申请人".to_string(),
                },
                Outcome::Waiting,
                "waiting",
            ),
            (
                DispatchResult::Terminal {
                    message: "必填列缺失".to_string(),
                },
                Outcome::Failed,
                "failed",
            ),
            (
                DispatchResult::Retryable {
                    message: "接口限流".to_string(),
                },
                Outcome::Failed,
                "failed",
            ),
        ];
        for (result, expected, text) in cases {
            assert_eq!(outcome_for(&result), expected);
            assert_eq!(expected.as_str(), text);
        }
    }

    /// 空 requested_by（未带 / 空白 / trim 后空）一律落 `feishu-workflow`。
    #[test]
    fn empty_requested_by_falls_back_to_feishu_workflow() {
        assert_eq!(normalized_requested_by(None), DEFAULT_REQUESTED_BY);
        assert_eq!(normalized_requested_by(Some("")), DEFAULT_REQUESTED_BY);
        assert_eq!(normalized_requested_by(Some("   ")), DEFAULT_REQUESTED_BY);
        // 非空则 trim 后原样保留（列上限已在校验层挡住）。
        assert_eq!(normalized_requested_by(Some("  张三  ")), "张三");
    }

    /// 全部可弃字段都满时的组装：每个键都在，值与原样一致。
    #[test]
    fn record_assembly_carries_every_column() {
        let log = RequestLog {
            requested_by: "张三".to_string(),
            base_token: "appbcbWCzen6".to_string(),
            table_id: "tblsRc9GRRX".to_string(),
            config_id: Some(7),
            record_id: Some("rec001".to_string()),
            request_body: r#"{"base_token":"appbcbWCzen6"}"#.to_string(),
            outcome: Outcome::Succeeded,
            message: "已创建审批实例，编号 202609280001".to_string(),
            serial_number: Some("202609280001".to_string()),
            response_body: Some(r#"{"accepted":true}"#.to_string()),
        };
        let map = log.into_record().into_map();
        assert_eq!(map.get("requested_by"), Some(&json!("张三")));
        assert_eq!(map.get("base_token"), Some(&json!("appbcbWCzen6")));
        assert_eq!(map.get("table_id"), Some(&json!("tblsRc9GRRX")));
        assert_eq!(map.get("config_id"), Some(&json!(7)));
        assert_eq!(map.get("record_id"), Some(&json!("rec001")));
        assert_eq!(
            map.get("request_body"),
            Some(&json!(r#"{"base_token":"appbcbWCzen6"}"#))
        );
        assert_eq!(map.get("outcome"), Some(&json!("succeeded")));
        assert_eq!(
            map.get("message"),
            Some(&json!("已创建审批实例，编号 202609280001"))
        );
        assert_eq!(map.get("serial_number"), Some(&json!("202609280001")));
        assert_eq!(
            map.get("response_body"),
            Some(&json!(r#"{"accepted":true}"#))
        );
        assert_eq!(map.len(), 10, "满配行恰好 10 个键");
    }

    /// 可空列缺省（失败出口的形态）时不插键——落 NULL 而不是空串。
    #[test]
    fn optional_columns_are_omitted_when_absent() {
        let log = RequestLog {
            requested_by: DEFAULT_REQUESTED_BY.to_string(),
            base_token: "appbcbWCzen6".to_string(),
            table_id: "tblsRc9GRRX".to_string(),
            config_id: None,
            record_id: None,
            request_body: r#"{}"#.to_string(),
            outcome: Outcome::Failed,
            message: "参数无效 [base_token]: 不能为空".to_string(),
            serial_number: None,
            response_body: None,
        };
        let map = log.into_record().into_map();
        for absent in ["config_id", "record_id", "serial_number", "response_body"] {
            assert!(!map.contains_key(absent), "{absent} 缺省时不应插键");
        }
        assert_eq!(map.get("outcome"), Some(&json!("failed")));
        // 五个必填列 + requested_by（恒有值，缺省落 feishu-workflow）。
        assert_eq!(map.len(), 6);
    }

    /// 组装键必须都在表声明上——`into_record` 不碰仓库，schema_anchor 的静态
    /// 归属解析不到它（已在 `schema_anchor` 的宽松档 `expected` 记账），
    /// 用表声明原地把「键 ⊆ 声明列 + 必填列都在」钉住，补上那道静态缺口。
    #[test]
    fn record_keys_are_declared_on_the_request_log_table() {
        let spec = crate::addon::feishu::approval::domain::request_log_table::table_spec()
            .unwrap_or_else(|error| panic!("表声明应有效: {error}"));
        let log = RequestLog {
            requested_by: "张三".to_string(),
            base_token: "appbcbWCzen6".to_string(),
            table_id: "tblsRc9GRRX".to_string(),
            config_id: Some(1),
            record_id: Some("rec001".to_string()),
            request_body: r#"{}"#.to_string(),
            outcome: Outcome::Waiting,
            message: "本轮未处理".to_string(),
            serial_number: Some("202609280001".to_string()),
            response_body: Some(r#"{}"#.to_string()),
        };
        let map = log.into_record().into_map();
        for key in map.keys() {
            assert!(
                spec.fields.iter().any(|field| field.name.as_str() == key),
                "键 {key} 未在表声明上"
            );
        }
        // 必填列（表声明 require=true 的那五个）都必须被组装出来。
        for required in [
            "base_token",
            "table_id",
            "request_body",
            "outcome",
            "message",
        ] {
            assert!(
                map.contains_key(required),
                "必填列 {required} 必须出现在记录行里"
            );
        }
    }
}

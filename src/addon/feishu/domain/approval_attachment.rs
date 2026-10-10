//! 多维表格附件搬运到审批系统；源 URL 不作为请求目标。
use super::approval_convert::{ConvertError, WidgetMap};
use super::bitable::BitableCoordinates;
use super::outbound::{
    send_with_retry, FailureKind, OutboundFailure, OutboundMethod, OutboundRequest,
    OutboundResponse, OutboundTransport, Sleeper, PULL_RETRY,
};
use super::tenant_token::TenantTokenProvider;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::time::Duration;

const MAX_ATTACHMENT_BYTES: usize = 50 * 1024 * 1024;
const UPLOAD_URL: &str = "https://www.feishu.cn/approval/openapi/v2/file/upload";

#[derive(Debug, Deserialize)]
pub(crate) struct Attachment {
    file_token: String,
    name: String,
    size: u64,
}

pub(crate) fn attachments(
    widget: &WidgetMap,
    cells: &Map<String, Value>,
) -> Result<Vec<Attachment>, ConvertError> {
    let bad = || ConvertError::BadValue {
        widget_id: widget.widget_id.clone(),
        reason: "附件必须是多维表格附件数组，含合法 file_token、文件名和 1 字节至 50 MiB 的 size"
            .to_string(),
    };
    let Some(value) = cells.get(&widget.bitable_field) else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let files: Vec<Attachment> = serde_json::from_value(value.clone()).map_err(|_| bad())?;
    for file in &files {
        if file.file_token.is_empty()
            || file.file_token.len() > 128
            || !file.file_token.bytes().all(|c| c.is_ascii_alphanumeric())
            || file.name.len() > 1024
            || file
                .name
                .chars()
                .any(|c| c.is_control() || matches!(c, '"' | '\\' | '/'))
            || !file
                .name
                .rsplit_once('.')
                .is_some_and(|(stem, ext)| !stem.trim().is_empty() && !ext.trim().is_empty())
            || file.size == 0
            || file.size > MAX_ATTACHMENT_BYTES as u64
        {
            return Err(bad());
        }
    }
    Ok(files)
}

/// 先生成校验用占位 code；完整 build_form 校验通过后才能产生外部副作用。
pub(crate) fn preflight(
    widgets: &[WidgetMap],
    cells: &Map<String, Value>,
) -> Result<BTreeMap<String, Vec<String>>, ConvertError> {
    let mut codes = BTreeMap::new();
    for widget in widgets.iter().filter(|w| w.widget_type == "attachmentV2") {
        let files = attachments(widget, cells)?;
        if !files.is_empty() {
            codes.insert(
                widget.widget_id.clone(),
                files.iter().map(|_| "preflight".to_string()).collect(),
            );
        }
    }
    Ok(codes)
}

#[cfg(test)]
mod tests {
    use super::super::approval_convert::Converter;
    use super::*;

    #[test]
    fn attachment_metadata_rejects_invalid_tokens_names_sizes_and_shapes() {
        let widget = WidgetMap {
            widget_id: "a".to_string(),
            widget_type: "attachmentV2".to_string(),
            bitable_field: "f".to_string(),
            required: true,
            converter: Converter::Direct,
            option_map: BTreeMap::new(),
            currency: None,
        };
        let valid = json!({"file_token":"Token1", "name":"凭证.pdf", "size":1});
        for (key, value) in [
            ("file_token", json!("")),
            ("file_token", json!("a/../b")),
            ("file_token", json!("a".repeat(129))),
            ("file_token", json!("中文")),
            ("name", json!("plain")),
            ("name", json!(".pdf")),
            ("name", json!("a.")),
            ("name", json!("a\r\nx.pdf")),
            ("name", json!("a\".pdf")),
            ("name", json!("a/b.pdf")),
            ("name", json!("a\\b.pdf")),
            ("name", json!(format!("{}.pdf", "a".repeat(1024)))),
            ("size", json!(0)),
            ("size", json!(MAX_ATTACHMENT_BYTES + 1)),
        ] {
            let mut file = valid.clone();
            file[key] = value;
            assert!(
                attachments(&widget, &Map::from_iter([("f".to_string(), json!([file]))])).is_err(),
                "{key}"
            );
        }
        for value in [json!("Token1"), json!({"file_token":"Token1"}), json!([{}])] {
            assert!(attachments(&widget, &Map::from_iter([("f".to_string(), value)])).is_err());
        }
        for size in [1, MAX_ATTACHMENT_BYTES] {
            let mut file = valid.clone();
            file["size"] = json!(size);
            assert_eq!(
                attachments(&widget, &Map::from_iter([("f".to_string(), json!([file]))]))
                    .unwrap_or_else(|e| panic!("{e}"))
                    .len(),
                1
            );
        }
        assert!(attachments(&widget, &Map::new())
            .unwrap_or_else(|e| panic!("{e}"))
            .is_empty());
        assert!(
            attachments(&widget, &Map::from_iter([("f".to_string(), Value::Null)]))
                .unwrap_or_else(|e| panic!("{e}"))
                .is_empty()
        );
        for value in [Value::Null, json!([])] {
            assert!(preflight(
                std::slice::from_ref(&widget),
                &Map::from_iter([("f".to_string(), value)])
            )
            .unwrap_or_else(|e| panic!("{e}"))
            .is_empty());
        }
    }
}

fn multipart(name: &str, bytes: &[u8]) -> (String, Vec<u8>) {
    let boundary = format!("feishu-{}", uuid::Uuid::new_v4());
    let mut body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"name\"\r\n\r\n{name}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"type\"\r\n\r\nattachment\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"content\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n").into_bytes();
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

async fn send(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    mut request: OutboundRequest,
) -> Result<OutboundResponse, OutboundFailure> {
    request.bearer_token = Some(
        tokens
            .tenant_access_token()
            .await
            .map_err(|_| retry("附件调用无法取得应用 token"))?,
    );
    for refresh in [false, true] {
        match send_with_retry(transport, sleeper, &request, PULL_RETRY).await {
            Err(failure) if failure.kind == FailureKind::TokenExpired && !refresh => {
                request.bearer_token = Some(
                    tokens
                        .invalidate_and_refresh()
                        .await
                        .map_err(|_| retry("附件调用刷新应用 token 失败"))?,
                );
            }
            result => return result,
        }
    }
    Err(retry("附件调用 token 失效"))
}

fn retry(message: &str) -> OutboundFailure {
    OutboundFailure {
        kind: FailureKind::Retry {
            retry_after_seconds: None,
        },
        message: message.to_string(),
    }
}

pub(crate) async fn transfer(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    coordinates: &BitableCoordinates,
    record_id: &str,
    widgets: &[WidgetMap],
    cells: &Map<String, Value>,
) -> Result<BTreeMap<String, Vec<String>>, OutboundFailure> {
    let mut codes = BTreeMap::new();
    for widget in widgets.iter().filter(|w| w.widget_type == "attachmentV2") {
        let files = attachments(widget, cells).map_err(|e| retry(&e.to_string()))?;
        let mut uploaded = Vec::new();
        for file in files {
            // 官方下载配额 5 QPS；单条内串行，跨请求仍以服务端 429 退避兜底。
            sleeper.sleep(Duration::from_millis(200)).await;
            let extra = json!({"bitablePerm":{"tableId":coordinates.table_id,"attachments":{&widget.bitable_field:{record_id:[&file.file_token]}}}}).to_string();
            let response = send(
                transport,
                sleeper,
                tokens,
                OutboundRequest {
                    method: OutboundMethod::Get,
                    url: format!(
                        "https://open.feishu.cn/open-apis/drive/v1/medias/{}/download",
                        file.file_token
                    ),
                    query: vec![("extra".to_string(), extra)],
                    bearer_token: None,
                    json_body: None,
                    raw_body: None,
                    binary_limit: Some(MAX_ATTACHMENT_BYTES),
                    timeout_secs: Some(120),
                    idempotent: true,
                },
            )
            .await?;
            let bytes = response
                .bytes
                .filter(|b| response.status == 200 && b.len() as u64 == file.size)
                .ok_or_else(|| retry("附件下载不是完整二进制文件或大小与记录不一致"))?;
            let response = send(
                transport,
                sleeper,
                tokens,
                OutboundRequest {
                    method: OutboundMethod::Post,
                    url: UPLOAD_URL.to_string(),
                    query: Vec::new(),
                    bearer_token: None,
                    json_body: None,
                    raw_body: Some(multipart(&file.name, &bytes)),
                    binary_limit: None,
                    timeout_secs: Some(120),
                    // 上传没有幂等键，超时交给任务下一轮重试；审批实例仍由 UUID 去重。
                    idempotent: false,
                },
            )
            .await?;
            let value: Value = serde_json::from_str(&response.body)
                .map_err(|_| retry("审批附件上传响应不是 JSON"))?;
            let code = value
                .get("data")
                .and_then(|d| d.get("code"))
                .and_then(Value::as_str)
                .filter(|c| {
                    value.get("code").and_then(Value::as_i64) == Some(0) && !c.trim().is_empty()
                })
                .ok_or_else(|| retry("审批附件上传响应缺少成功标记或 file code"))?;
            uploaded.push(code.to_string());
        }
        if !uploaded.is_empty() {
            codes.insert(widget.widget_id.clone(), uploaded);
        }
    }
    Ok(codes)
}

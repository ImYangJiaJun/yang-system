//! 审批签名 URL → 多维表附件 `file_token`。
//!
//! 审批实例回读的附件是 12 小时签名 URL，不能直接写进多维表附件列；必须先
//! 下载二进制，再调 `drive/v1/medias/upload_all` 换取 `file_token`。

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use super::super::approval_export::form::AttachmentRef;
use super::super::outbound::{
    send_with_retry, FailureKind, OutboundMethod, OutboundRequest, OutboundResponse,
    OutboundTransport, Sleeper, PULL_RETRY,
};
use super::super::tenant_token::TenantTokenProvider;
use super::error::SyncError;
use super::plan::AttachmentPreparer;
use super::value::PreparedAttachment;

const UPLOAD_URL: &str = "https://open.feishu.cn/open-apis/drive/v1/medias/upload_all";
const DOWNLOAD_TIMEOUT_SECS: u64 = 120;
const UPLOAD_TIMEOUT_SECS: u64 = 120;
/// 多维表 `upload_all` 的单文件上限；超过要走分片上传。
pub(crate) const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;

/// 真实上传器。
pub(crate) struct BitableAttachmentUploader<'a> {
    pub(crate) transport: &'a dyn OutboundTransport,
    pub(crate) sleeper: &'a dyn Sleeper,
    pub(crate) tokens: &'a TenantTokenProvider,
    pub(crate) base_token: &'a str,
    pub(crate) max_bytes: usize,
}

#[async_trait]
impl AttachmentPreparer for BitableAttachmentUploader<'_> {
    async fn prepare(
        &self,
        widget_id: &str,
        widget_type: &str,
        files: &[AttachmentRef],
    ) -> Result<Vec<PreparedAttachment>, SyncError> {
        if self.base_token.trim().is_empty() {
            return Err(SyncError::AttachmentUnavailable {
                widget_id: widget_id.to_string(),
            });
        }
        let parent_type = if matches!(widget_type, "image" | "imageV2") {
            "bitable_image"
        } else {
            "bitable_file"
        };
        let mut prepared = Vec::with_capacity(files.len());
        for file in files {
            if file.url.trim().is_empty() {
                return Err(SyncError::AttachmentUnavailable {
                    widget_id: widget_id.to_string(),
                });
            }
            if file.size.is_some_and(|size| size > self.max_bytes as i64) {
                return Err(SyncError::AttachmentTooLarge {
                    widget_id: widget_id.to_string(),
                    size: file.size.unwrap_or_default(),
                });
            }
            // 官方下载配额 5 QPS；单条内串行，服务端 429 仍由重试兜底。
            self.sleeper.sleep(Duration::from_millis(200)).await;
            let request = OutboundRequest {
                method: OutboundMethod::Get,
                url: file.url.clone(),
                query: Vec::new(),
                bearer_token: None,
                json_body: None,
                raw_body: None,
                binary_limit: Some(self.max_bytes + 1),
                timeout_secs: Some(DOWNLOAD_TIMEOUT_SECS),
                idempotent: true,
            };
            let response = send_with_retry(self.transport, self.sleeper, &request, PULL_RETRY)
                .await
                .map_err(|_| SyncError::AttachmentUnavailable {
                    widget_id: widget_id.to_string(),
                })?;
            let bytes = response
                .bytes
                .filter(|bytes| response.status == 200 && !bytes.is_empty())
                .ok_or(SyncError::AttachmentUnavailable {
                    widget_id: widget_id.to_string(),
                })?;
            if bytes.len() > self.max_bytes {
                return Err(SyncError::AttachmentTooLarge {
                    widget_id: widget_id.to_string(),
                    size: bytes.len() as i64,
                });
            }
            let (content_type, body) = multipart(
                &safe_filename(&file.name),
                &bytes,
                parent_type,
                self.base_token,
            );
            let response = self.upload(widget_id, content_type, body).await?;
            let parsed: Value = serde_json::from_str(&response.body).map_err(|_| {
                SyncError::AttachmentUnavailable {
                    widget_id: widget_id.to_string(),
                }
            })?;
            let code = parsed.get("code").and_then(Value::as_i64).unwrap_or(-1);
            let file_token = parsed
                .get("data")
                .and_then(|data| data.get("file_token"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .ok_or(SyncError::AttachmentUnavailable {
                    widget_id: widget_id.to_string(),
                })?;
            if code != 0 {
                return Err(SyncError::AttachmentUnavailable {
                    widget_id: widget_id.to_string(),
                });
            }
            prepared.push(PreparedAttachment {
                file_token: file_token.to_string(),
                name: file.name.clone(),
                size: Some(bytes.len() as i64),
            });
        }
        Ok(prepared)
    }
}

impl BitableAttachmentUploader<'_> {
    async fn upload(
        &self,
        widget_id: &str,
        content_type: String,
        body: Vec<u8>,
    ) -> Result<OutboundResponse, SyncError> {
        let mut request = OutboundRequest {
            method: OutboundMethod::Post,
            url: UPLOAD_URL.to_string(),
            query: Vec::new(),
            bearer_token: None,
            json_body: None,
            raw_body: Some((content_type, body)),
            binary_limit: None,
            timeout_secs: Some(UPLOAD_TIMEOUT_SECS),
            // 上传没有幂等键；重试可能产生孤儿文件（无删除 API），但不影响
            // 台账正确性，比「一次 429 就失败」更可接受。
            idempotent: true,
        };
        let mut token = self.tokens.tenant_access_token().await.map_err(|_| {
            SyncError::AttachmentUnavailable {
                widget_id: widget_id.to_string(),
            }
        })?;
        for refresh in [false, true] {
            request.bearer_token = Some(token.clone());
            match send_with_retry(self.transport, self.sleeper, &request, PULL_RETRY).await {
                Err(failure) if failure.kind == FailureKind::TokenExpired && !refresh => {
                    token = self.tokens.invalidate_and_refresh().await.map_err(|_| {
                        SyncError::AttachmentUnavailable {
                            widget_id: widget_id.to_string(),
                        }
                    })?;
                }
                Ok(response) => return Ok(response),
                Err(_) => {
                    return Err(SyncError::AttachmentUnavailable {
                        widget_id: widget_id.to_string(),
                    })
                }
            }
        }
        Err(SyncError::AttachmentUnavailable {
            widget_id: widget_id.to_string(),
        })
    }
}

fn safe_filename(name: &str) -> String {
    let trimmed = name.trim();
    let trimmed = if trimmed.is_empty() {
        "附件"
    } else {
        trimmed
    };
    trimmed
        .chars()
        .map(|character| match character {
            '"' | '\r' | '\n' => '_',
            other => other,
        })
        .collect()
}

fn multipart(
    file_name: &str,
    bytes: &[u8],
    parent_type: &str,
    base_token: &str,
) -> (String, Vec<u8>) {
    let boundary = format!("----yang-system-{}", uuid::Uuid::new_v4().simple());
    let mut body = Vec::new();
    let mut field = |name: &str, value: &str| {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    };
    field("file_name", file_name);
    field("parent_type", parent_type);
    field("parent_node", base_token);
    field("size", &bytes.len().to_string());
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{Arc, Mutex};

    use crate::addon::feishu::domain::tenant_token::{FeishuCredentials, TenantTokenCache};
    use yang_base::BaseError;

    use super::*;

    struct FakeCache {
        token: Mutex<Option<String>>,
    }

    impl FakeCache {
        fn with(token: &str) -> Self {
            Self {
                token: Mutex::new(Some(token.to_string())),
            }
        }
    }

    #[async_trait]
    impl TenantTokenCache for FakeCache {
        async fn get(&self) -> anyhow::Result<Option<String>> {
            Ok(self.token.lock().ok().and_then(|token| token.clone()))
        }

        async fn put(&self, token: &str, _ttl_seconds: i64) -> anyhow::Result<()> {
            if let Ok(mut stored) = self.token.lock() {
                *stored = Some(token.to_string());
            }
            Ok(())
        }

        async fn invalidate(&self) -> anyhow::Result<()> {
            if let Ok(mut stored) = self.token.lock() {
                *stored = None;
            }
            Ok(())
        }

        async fn acquire_lock(&self, _owner: &str, _ttl_seconds: i64) -> anyhow::Result<bool> {
            Ok(true)
        }

        async fn release_lock(&self, _owner: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct ScriptedTransport {
        responses: Mutex<VecDeque<Result<OutboundResponse, BaseError>>>,
        requests: Mutex<Vec<OutboundRequest>>,
    }

    impl ScriptedTransport {
        fn new(responses: Vec<Result<OutboundResponse, BaseError>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<OutboundRequest> {
            self.requests
                .lock()
                .map(|requests| requests.clone())
                .unwrap_or_default()
        }
    }

    #[async_trait]
    impl OutboundTransport for ScriptedTransport {
        async fn send(&self, request: OutboundRequest) -> Result<OutboundResponse, BaseError> {
            if let Ok(mut requests) = self.requests.lock() {
                requests.push(request);
            }
            let mut responses = self
                .responses
                .lock()
                .map_err(|_| BaseError::ParamInvalid("test".into(), "锁中毒".into()))?;
            responses
                .pop_front()
                .unwrap_or_else(|| Err(BaseError::ParamInvalid("test".into(), "脚本已耗尽".into())))
        }
    }

    struct NoopSleeper;

    #[async_trait]
    impl Sleeper for NoopSleeper {
        async fn sleep(&self, _duration: Duration) {}
    }

    fn response(status: u16, body: &str, bytes: Option<Vec<u8>>) -> OutboundResponse {
        OutboundResponse {
            status,
            body: body.to_string(),
            bytes,
            headers: BTreeMap::new(),
        }
    }

    fn upload_ok(token: &str) -> OutboundResponse {
        response(
            200,
            &format!("{{\"code\":0,\"msg\":\"ok\",\"data\":{{\"file_token\":\"{token}\"}}}}"),
            None,
        )
    }

    fn make_uploader<'a>(
        transport: &'a ScriptedTransport,
        tokens: &'a TenantTokenProvider,
        sleeper: &'a NoopSleeper,
    ) -> BitableAttachmentUploader<'a> {
        BitableAttachmentUploader {
            transport,
            sleeper,
            tokens,
            base_token: "appBase",
            max_bytes: MAX_UPLOAD_BYTES,
        }
    }

    fn make_tokens(
        transport: &Arc<ScriptedTransport>,
        sleeper: &Arc<NoopSleeper>,
    ) -> TenantTokenProvider {
        TenantTokenProvider::new(
            Arc::new(FakeCache::with("tenant-token")),
            Arc::clone(transport) as Arc<dyn OutboundTransport>,
            Arc::clone(sleeper) as Arc<dyn Sleeper>,
            FeishuCredentials {
                app_id: "cli_test".into(),
                app_secret: "secret".into(),
            },
            "test",
        )
        .unwrap_or_else(|error| panic!("{error}"))
    }

    #[tokio::test]
    async fn downloads_then_uploads_with_base_token_and_parent_type() {
        for (widget_type, parent_type) in [
            ("attachmentV2", "bitable_file"),
            ("imageV2", "bitable_image"),
        ] {
            let transport = Arc::new(ScriptedTransport::new(vec![
                Ok(response(200, "", Some(b"pdf-bytes".to_vec()))),
                Ok(upload_ok("file-token-1")),
            ]));
            let sleeper = Arc::new(NoopSleeper);
            let tokens = make_tokens(&transport, &sleeper);
            let uploader = make_uploader(&transport, &tokens, &sleeper);
            let files = [AttachmentRef {
                name: "凭证.pdf".into(),
                url: "https://example.test/signed".into(),
                size: Some(9),
            }];
            let prepared = uploader
                .prepare("w1", widget_type, &files)
                .await
                .unwrap_or_else(|error| panic!("{error}"));
            assert_eq!(prepared.len(), 1);
            assert_eq!(prepared[0].file_token, "file-token-1");
            assert_eq!(prepared[0].size, Some(9));
            let requests = transport.requests();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0].method, OutboundMethod::Get);
            assert_eq!(requests[0].url, "https://example.test/signed");
            assert!(requests[0].bearer_token.is_none());
            assert_eq!(requests[0].binary_limit, Some(MAX_UPLOAD_BYTES + 1));
            assert_eq!(requests[1].method, OutboundMethod::Post);
            assert_eq!(requests[1].url, UPLOAD_URL);
            assert_eq!(requests[1].bearer_token.as_deref(), Some("tenant-token"));
            let (content_type, body) = requests[1]
                .raw_body
                .as_ref()
                .unwrap_or_else(|| panic!("必须有 multipart body"));
            assert!(content_type.starts_with("multipart/form-data; boundary="));
            let body = String::from_utf8_lossy(body);
            assert!(body.contains("name=\"file_name\""));
            assert!(body.contains("凭证.pdf"));
            assert!(body.contains(&format!("name=\"parent_type\"\r\n\r\n{parent_type}")));
            assert!(body.contains("name=\"parent_node\"\r\n\r\nappBase"));
            assert!(body.contains("filename=\"凭证.pdf\""));
            assert!(body.contains("pdf-bytes"));
        }
    }

    #[tokio::test]
    async fn rejects_oversize_empty_url_and_bad_upload_response() {
        let transport = Arc::new(ScriptedTransport::new(Vec::new()));
        let sleeper = Arc::new(NoopSleeper);
        let tokens = make_tokens(&transport, &sleeper);
        let uploader = make_uploader(&transport, &tokens, &sleeper);

        let too_large = [AttachmentRef {
            name: "big.pdf".into(),
            url: "https://example.test/big".into(),
            size: Some((MAX_UPLOAD_BYTES + 1) as i64),
        }];
        assert!(matches!(
            uploader.prepare("w1", "attachmentV2", &too_large).await,
            Err(SyncError::AttachmentTooLarge { .. })
        ));
        assert!(transport.requests().is_empty(), "超限不应发起下载");

        let empty = [AttachmentRef {
            name: "a.pdf".into(),
            url: " ".into(),
            size: None,
        }];
        assert!(matches!(
            uploader.prepare("w1", "attachmentV2", &empty).await,
            Err(SyncError::AttachmentUnavailable { .. })
        ));

        let transport = Arc::new(ScriptedTransport::new(vec![
            Ok(response(200, "", Some(b"x".to_vec()))),
            Ok(response(200, "{\"code\":0,\"data\":{}}", None)),
        ]));
        let sleeper = Arc::new(NoopSleeper);
        let tokens = make_tokens(&transport, &sleeper);
        let uploader = make_uploader(&transport, &tokens, &sleeper);
        let files = [AttachmentRef {
            name: "a.pdf".into(),
            url: "https://example.test/a".into(),
            size: Some(1),
        }];
        assert!(matches!(
            uploader.prepare("w1", "attachmentV2", &files).await,
            Err(SyncError::AttachmentUnavailable { .. })
        ));
    }

    #[tokio::test]
    async fn retries_download_and_upload_on_429_then_500() {
        let mut headers = BTreeMap::new();
        headers.insert("x-ogw-ratelimit-reset".to_string(), "0".to_string());
        let transport = Arc::new(ScriptedTransport::new(vec![
            Ok(OutboundResponse {
                status: 429,
                body: "{\"code\":99991400,\"msg\":\"rate limited\"}".into(),
                bytes: None,
                headers,
            }),
            Ok(response(200, "", Some(b"x".to_vec()))),
            Ok(response(500, "{\"code\":1254290,\"msg\":\"busy\"}", None)),
            Ok(upload_ok("tok")),
        ]));
        let sleeper = Arc::new(NoopSleeper);
        let tokens = make_tokens(&transport, &sleeper);
        let uploader = make_uploader(&transport, &tokens, &sleeper);
        let files = [AttachmentRef {
            name: "a.pdf".into(),
            url: "https://example.test/a".into(),
            size: Some(1),
        }];
        let prepared = uploader
            .prepare("w1", "attachmentV2", &files)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(prepared[0].file_token, "tok");
        assert_eq!(transport.requests().len(), 4, "两次重试各多一次");
    }

    #[test]
    fn filename_header_injection_is_neutralized() {
        let safe = safe_filename("a\"b\r\nc.pdf");
        assert!(!safe.contains('"'));
        assert!(!safe.contains('\r'));
        assert!(!safe.contains('\n'));
    }
}

use super::AUDIT_EVENT_SCHEMA_VERSION;
use anyhow::{bail, ensure, Context};
use rand_core::{OsRng, RngCore};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};
use yang_base::action::RequestId;

const EVENT_ID_BYTES: usize = 16;
const MAX_ENTITY_KIND_BYTES: usize = 64;
const MAX_ENTITY_ID_CHARS: usize = 128;
const MAX_ACTION_BYTES: usize = 128;
const MAX_SUMMARY_FIELDS: usize = 64;
const MAX_SUMMARY_BYTES: usize = 16 * 1024;
const MAX_SUMMARY_STRING_CHARS: usize = 1_024;
const MAX_SUMMARY_ARRAY_ITEMS: usize = 64;
const SENSITIVE_FIELD_MARKERS: [&str; 8] = [
    "password",
    "secret",
    "token",
    "nonce",
    "credential",
    "authorization",
    "cookie",
    "hash",
];

/// 审计 actor；用户 ID 或稳定的系统主体标识，不能匿名。
#[derive(Clone, PartialEq, Eq)]
pub struct AuditActor {
    kind: &'static str,
    id: String,
}

impl AuditActor {
    pub fn user(user_id: i64) -> anyhow::Result<Self> {
        ensure!(user_id > 0, "审计用户 actor ID 必须为正数");
        Ok(Self {
            kind: "user",
            id: user_id.to_string(),
        })
    }

    pub fn system(id: impl Into<String>) -> anyhow::Result<Self> {
        let id = id.into();
        validate_entity_id("审计系统 actor", &id)?;
        Ok(Self { kind: "system", id })
    }

    pub fn kind(&self) -> &'static str {
        self.kind
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

impl fmt::Debug for AuditActor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuditActor")
            .field("kind", &self.kind)
            .field("id", &self.id)
            .finish()
    }
}

/// 审计 subject/target 的稳定类型与标识。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntity {
    kind: String,
    id: String,
}

impl AuditEntity {
    pub fn new(kind: impl Into<String>, id: impl Into<String>) -> anyhow::Result<Self> {
        let kind = kind.into();
        let id = id.into();
        validate_identifier("审计实体类型", &kind, MAX_ENTITY_KIND_BYTES)?;
        validate_entity_id("审计实体 ID", &id)?;
        Ok(Self { kind, id })
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

/// 只允许白名单字段组成的审计前后状态摘要。
///
/// 摘要值限制为标量或标量数组，拒绝任意嵌套对象、敏感字段名和无界内容。
#[derive(Clone, PartialEq)]
pub struct AuditSummary {
    fields: BTreeMap<String, Value>,
}

impl AuditSummary {
    pub fn try_from_fields<I, K>(fields: I) -> anyhow::Result<Self>
    where
        I: IntoIterator<Item = (K, Value)>,
        K: Into<String>,
    {
        let mut normalized_fields = BTreeMap::new();
        for (key, value) in fields {
            let key = key.into();
            ensure!(
                normalized_fields.insert(key.clone(), value).is_none(),
                "审计摘要字段不能重复: {key}"
            );
        }
        let fields = normalized_fields;
        ensure!(
            !fields.is_empty() && fields.len() <= MAX_SUMMARY_FIELDS,
            "审计摘要字段数必须在 1..={MAX_SUMMARY_FIELDS} 范围内"
        );
        for (key, value) in &fields {
            validate_summary_key(key)?;
            validate_summary_value(key, value)?;
        }
        let encoded = serde_json::to_vec(&fields).context("序列化审计摘要失败")?;
        ensure!(
            encoded.len() <= MAX_SUMMARY_BYTES,
            "审计摘要不能超过 {MAX_SUMMARY_BYTES} 字节"
        );
        Ok(Self { fields })
    }

    pub fn as_json(&self) -> Value {
        Value::Object(
            self.fields
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        )
    }

    pub fn field_names(&self) -> impl Iterator<Item = &str> {
        self.fields.keys().map(String::as_str)
    }
}

impl fmt::Debug for AuditSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuditSummary")
            .field("field_names", &self.fields.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditResult {
    Succeeded,
    Denied,
    Failed,
}

impl AuditResult {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Denied => "denied",
            Self::Failed => "failed",
        }
    }
}

/// 一次审计事实共享的可信请求上下文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEventContext {
    actor: AuditActor,
    tenant_id: Option<i64>,
    request_id: RequestId,
}

impl AuditEventContext {
    pub fn new(
        actor: AuditActor,
        tenant_id: Option<i64>,
        request_id: RequestId,
    ) -> anyhow::Result<Self> {
        if let Some(tenant_id) = tenant_id {
            ensure!(tenant_id > 0, "审计 tenant_id 必须为正数");
        }
        ensure!(request_id.as_u128() != 0, "审计 request_id 不能为零");
        Ok(Self {
            actor,
            tenant_id,
            request_id,
        })
    }

    pub fn actor(&self) -> &AuditActor {
        &self.actor
    }

    pub const fn tenant_id(&self) -> Option<i64> {
        self.tenant_id
    }

    pub const fn request_id(&self) -> RequestId {
        self.request_id
    }
}

/// 创建后不可修改的审计事件。
#[derive(Clone, PartialEq)]
pub struct AuditEvent {
    event_id: String,
    schema_version: u16,
    occurred_at: i64,
    context: AuditEventContext,
    action: String,
    subject: Option<AuditEntity>,
    target: AuditEntity,
    result: AuditResult,
    before_summary: Option<AuditSummary>,
    after_summary: Option<AuditSummary>,
}

impl AuditEvent {
    pub fn new(
        context: AuditEventContext,
        action: impl Into<String>,
        subject: Option<AuditEntity>,
        target: AuditEntity,
        result: AuditResult,
        before_summary: Option<AuditSummary>,
        after_summary: Option<AuditSummary>,
    ) -> anyhow::Result<Self> {
        let action = action.into();
        validate_identifier("审计 action", &action, MAX_ACTION_BYTES)?;
        if result == AuditResult::Succeeded {
            ensure!(
                before_summary.is_some() || after_summary.is_some(),
                "成功审计事件必须包含 before_summary 或 after_summary"
            );
        }
        let occurred_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("系统时间早于 Unix epoch，无法创建审计事件")?
            .as_secs();
        let occurred_at =
            i64::try_from(occurred_at).context("审计事件时间超出 MySQL BIGINT 范围")?;

        Ok(Self {
            event_id: generate_event_id()?,
            schema_version: AUDIT_EVENT_SCHEMA_VERSION,
            occurred_at,
            context,
            action,
            subject,
            target,
            result,
            before_summary,
            after_summary,
        })
    }

    pub fn event_id(&self) -> &str {
        &self.event_id
    }

    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    pub const fn occurred_at(&self) -> i64 {
        self.occurred_at
    }

    pub const fn context(&self) -> &AuditEventContext {
        &self.context
    }

    pub fn action(&self) -> &str {
        &self.action
    }

    pub fn subject(&self) -> Option<&AuditEntity> {
        self.subject.as_ref()
    }

    pub const fn target(&self) -> &AuditEntity {
        &self.target
    }

    pub const fn result(&self) -> AuditResult {
        self.result
    }

    pub fn before_summary(&self) -> Option<&AuditSummary> {
        self.before_summary.as_ref()
    }

    pub fn after_summary(&self) -> Option<&AuditSummary> {
        self.after_summary.as_ref()
    }
}

impl fmt::Debug for AuditEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuditEvent")
            .field("event_id", &self.event_id)
            .field("schema_version", &self.schema_version)
            .field("occurred_at", &self.occurred_at)
            .field("context", &self.context)
            .field("action", &self.action)
            .field("subject", &self.subject)
            .field("target", &self.target)
            .field("result", &self.result)
            .field(
                "before_fields",
                &self
                    .before_summary
                    .as_ref()
                    .map(|summary| summary.field_names().collect::<Vec<_>>()),
            )
            .field(
                "after_fields",
                &self
                    .after_summary
                    .as_ref()
                    .map(|summary| summary.field_names().collect::<Vec<_>>()),
            )
            .finish()
    }
}

fn generate_event_id() -> anyhow::Result<String> {
    let mut random = [0_u8; EVENT_ID_BYTES];
    OsRng
        .try_fill_bytes(&mut random)
        .map_err(|error| anyhow::anyhow!("生成审计 event_id 随机源失败: {error}"))?;
    let mut event_id = String::with_capacity(EVENT_ID_BYTES * 2);
    for byte in random {
        write!(&mut event_id, "{byte:02x}").context("编码审计 event_id 失败")?;
    }
    Ok(event_id)
}

fn validate_identifier(name: &str, value: &str, max_bytes: usize) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > max_bytes
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
    {
        bail!("{name} 必须是 1..={max_bytes} 字节的小写 ASCII 标识符");
    }
    Ok(())
}

fn validate_entity_id(name: &str, value: &str) -> anyhow::Result<()> {
    ensure!(
        !value.is_empty()
            && value.trim() == value
            && value.chars().count() <= MAX_ENTITY_ID_CHARS
            && !value.chars().any(char::is_control),
        "{name} 必须是 1..={MAX_ENTITY_ID_CHARS} 个无控制字符且无首尾空白的字符"
    );
    Ok(())
}

fn validate_summary_key(key: &str) -> anyhow::Result<()> {
    validate_identifier("审计摘要字段名", key, MAX_ENTITY_KIND_BYTES)?;
    let normalized = key.to_ascii_lowercase();
    if SENSITIVE_FIELD_MARKERS
        .iter()
        .any(|marker| normalized.contains(marker))
    {
        bail!("审计摘要字段 {key} 命中敏感字段拒绝规则");
    }
    Ok(())
}

fn validate_summary_value(key: &str, value: &Value) -> anyhow::Result<()> {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
        Value::String(value) => {
            ensure!(
                value.chars().count() <= MAX_SUMMARY_STRING_CHARS,
                "审计摘要字段 {key} 字符串超过 {MAX_SUMMARY_STRING_CHARS} 字符"
            );
            Ok(())
        }
        Value::Array(values) => {
            ensure!(
                values.len() <= MAX_SUMMARY_ARRAY_ITEMS,
                "审计摘要字段 {key} 数组超过 {MAX_SUMMARY_ARRAY_ITEMS} 项"
            );
            for value in values {
                match value {
                    Value::Null | Value::Bool(_) | Value::Number(_) => {}
                    Value::String(value) if value.chars().count() <= MAX_SUMMARY_STRING_CHARS => {}
                    Value::String(_) => {
                        bail!("审计摘要字段 {key} 的数组字符串超过 {MAX_SUMMARY_STRING_CHARS} 字符")
                    }
                    Value::Array(_) | Value::Object(_) => {
                        bail!("审计摘要字段 {key} 禁止嵌套数组或对象")
                    }
                }
            }
            Ok(())
        }
        Value::Object(_) => bail!("审计摘要字段 {key} 禁止嵌套对象"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_context() -> AuditEventContext {
        AuditEventContext::new(
            AuditActor::user(7).unwrap_or_else(|error| panic!("用户 actor 应有效: {error}")),
            Some(42),
            RequestId::from_u128(9),
        )
        .unwrap_or_else(|error| panic!("审计上下文应有效: {error}"))
    }

    #[test]
    fn event_is_bounded_correlated_and_debug_does_not_expose_summary_values() {
        let before = AuditSummary::try_from_fields([
            ("status", json!("active")),
            ("roles", json!(["member"])),
        ])
        .unwrap_or_else(|error| panic!("安全摘要应有效: {error}"));
        let after = AuditSummary::try_from_fields([
            ("status", json!("active")),
            ("roles", json!(["member", "org_admin"])),
        ])
        .unwrap_or_else(|error| panic!("安全摘要应有效: {error}"));
        let event = AuditEvent::new(
            valid_context(),
            "org.user.set_admin",
            Some(
                AuditEntity::new("user", "7")
                    .unwrap_or_else(|error| panic!("subject 应有效: {error}")),
            ),
            AuditEntity::new("org_membership", "81")
                .unwrap_or_else(|error| panic!("target 应有效: {error}")),
            AuditResult::Succeeded,
            Some(before),
            Some(after),
        )
        .unwrap_or_else(|error| panic!("审计事件应有效: {error}"));

        assert_eq!(event.event_id().len(), 32);
        assert!(event
            .event_id()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(event.schema_version(), 1);
        assert_eq!(event.context().tenant_id(), Some(42));
        assert_eq!(event.context().request_id().as_u128(), 9);
        assert_eq!(event.action(), "org.user.set_admin");
        assert!(event.occurred_at() > 0);
        let debug = format!("{event:?}");
        assert!(!debug.contains("org_admin"));
        assert!(debug.contains("roles"));
    }

    #[test]
    fn summaries_reject_sensitive_nested_and_unbounded_content() {
        for fields in [
            vec![("password_hash", json!("do-not-store"))],
            vec![("safe", json!({"nested": "value"}))],
            vec![("safe", json!("x".repeat(MAX_SUMMARY_STRING_CHARS + 1)))],
            vec![("same", json!(1)), ("same", json!(2))],
        ] {
            assert!(
                AuditSummary::try_from_fields(fields).is_err(),
                "敏感、嵌套或无界摘要必须被拒绝"
            );
        }
    }

    #[test]
    fn event_rejects_invalid_identity_context_and_empty_success_summary() {
        assert!(AuditActor::user(0).is_err());
        assert!(AuditActor::system(" system ").is_err());
        assert!(AuditEntity::new("Invalid", "7").is_err());
        assert!(AuditEventContext::new(
            AuditActor::user(7).unwrap_or_else(|error| panic!("用户 actor 应有效: {error}")),
            Some(0),
            RequestId::from_u128(9),
        )
        .is_err());
        assert!(AuditEvent::new(
            valid_context(),
            "account.user.register",
            None,
            AuditEntity::new("admin_account", "1")
                .unwrap_or_else(|error| panic!("target 应有效: {error}")),
            AuditResult::Succeeded,
            None,
            None,
        )
        .is_err());
    }
}

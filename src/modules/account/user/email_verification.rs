//! 公开注册邮箱验证码：摘要存储、独立限流与原子单次消费。

use super::rate_limit::client_ip_identity;
use crate::config::EmailVerificationSettings;
use crate::email::RegistrationEmailSenderHandle;
use hmac::{Hmac, Mac};
use rand_core::{OsRng, RngCore};
use schemars::JsonSchema;
use serde::Serialize;
use sha2::Sha256;
use yang_base::action::ActionContext;
use yang_base::BaseError;

const CODE_SPACE: u32 = 1_000_000;
const CODE_DIGITS: usize = 6;

const RESERVE_SEND_SCRIPT: &str = r#"
local cooldown_ttl = redis.call('TTL', KEYS[4])
if cooldown_ttl > 0 then
    return {2, cooldown_ttl}
end

local retry_after = 0
local exceeded = 0
for index = 1, 3 do
    local current = redis.call('INCR', KEYS[index])
    if current == 1 then
        redis.call('EXPIRE', KEYS[index], ARGV[1])
    end
    local ttl = redis.call('TTL', KEYS[index])
    if ttl < 1 then
        redis.call('EXPIRE', KEYS[index], ARGV[1])
        ttl = tonumber(ARGV[1])
    end
    if current > tonumber(ARGV[index + 1]) then
        exceeded = 1
        if ttl > retry_after then
            retry_after = ttl
        end
    end
end
if exceeded == 1 then
    return {1, retry_after}
end

redis.call('SET', KEYS[4], '1', 'EX', ARGV[5], 'NX')
return {0, tonumber(ARGV[5])}
"#;

const VERIFY_AND_CONSUME_SCRIPT: &str = r#"
local value = redis.call('GET', KEYS[1])
if not value then
    return 0
end
local separator = string.find(value, ':', 1, true)
if not separator then
    redis.call('DEL', KEYS[1])
    return 0
end
local expected = string.sub(value, 1, separator - 1)
local attempts = tonumber(string.sub(value, separator + 1))
if not attempts then
    redis.call('DEL', KEYS[1])
    return 0
end
if expected == ARGV[1] then
    redis.call('DEL', KEYS[1])
    return 1
end
attempts = attempts + 1
if attempts >= tonumber(ARGV[2]) then
    redis.call('DEL', KEYS[1])
    return -2
end
redis.call('SET', KEYS[1], expected .. ':' .. attempts, 'KEEPTTL')
return -1
"#;

const DELETE_IF_CURRENT_SCRIPT: &str = r#"
local value = redis.call('GET', KEYS[1])
if value and string.sub(value, 1, string.len(ARGV[1]) + 1) == ARGV[1] .. ':' then
    return redis.call('DEL', KEYS[1])
end
return 0
"#;

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(super) struct RegistrationEmailCodeAccepted {
    pub(super) accepted: bool,
    pub(super) expires_in: u64,
    pub(super) resend_after: u64,
}

pub(super) fn normalize_email(value: &str) -> Result<String, BaseError> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty()
        || normalized.len() > 254
        || !normalized.is_ascii()
        || normalized.parse::<lettre::Address>().is_err()
    {
        return Err(invalid_email());
    }
    let Some((local, domain)) = normalized.split_once('@') else {
        return Err(invalid_email());
    };
    let local_valid = !local.is_empty()
        && local.len() <= 64
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'%' | b'+' | b'-')
        });
    let labels = domain.split('.').collect::<Vec<_>>();
    let domain_valid = labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        && labels.last().is_some_and(|label| {
            label.len() >= 2 && label.bytes().all(|byte| byte.is_ascii_alphabetic())
        });
    if !local_valid || !domain_valid {
        return Err(invalid_email());
    }
    Ok(normalized)
}

pub(super) struct RegistrationEmailVerification<'a> {
    settings: &'a EmailVerificationSettings,
}

impl<'a> RegistrationEmailVerification<'a> {
    pub(super) fn from_context(ctx: &'a ActionContext) -> Result<Self, BaseError> {
        Ok(Self {
            settings: ctx.tools().config::<EmailVerificationSettings>()?,
        })
    }

    pub(super) async fn request(
        &self,
        ctx: &ActionContext,
        email: &str,
        deliver: bool,
    ) -> Result<RegistrationEmailCodeAccepted, BaseError> {
        let fingerprint = email_fingerprint(&self.settings.secret, email);
        let prefix = self.key_prefix();
        let keys = [
            format!(
                "{prefix}:send:ip:{}",
                client_ip_fingerprint(&self.settings.secret, ctx)
            ),
            format!("{prefix}:send:email:{fingerprint}"),
            format!("{prefix}:send:global"),
            format!("{prefix}:cooldown:{fingerprint}"),
        ];
        let args = [
            self.settings.send_window_seconds.to_string(),
            self.settings.send_ip_attempts.to_string(),
            self.settings.send_email_attempts.to_string(),
            self.settings.send_global_attempts.to_string(),
            self.settings.resend_cooldown_seconds.to_string(),
        ];
        let cache = ctx.tools().cache()?;
        let decision: (i64, i64) = cache
            .eval_script(&cache.script(RESERVE_SEND_SCRIPT), &keys, &args)
            .await?;
        if decision.0 != 0 {
            metrics::counter!(
                "yang_system_registration_email_total",
                "result" => if decision.0 == 2 { "cooldown" } else { "limited" }
            )
            .increment(1);
            return Err(BaseError::RateLimitExceeded {
                retry_after_seconds: u64::try_from(decision.1).unwrap_or(1).max(1),
            });
        }

        if !deliver {
            metrics::counter!("yang_system_registration_email_total", "result" => "suppressed")
                .increment(1);
            return Ok(self.accepted());
        }

        let code = generate_code();
        let digest = code_digest(&self.settings.secret, email, &code);
        let code_key = format!("{prefix}:code:{fingerprint}");
        cache
            .setex(
                code_key.clone(),
                i64::try_from(self.settings.ttl_seconds).map_err(|_| {
                    BaseError::ConfigError("邮箱验证码 TTL 超出 Redis 范围".to_string())
                })?,
                format!("{digest}:0"),
            )
            .await?;

        let sender = ctx.tools().extension::<RegistrationEmailSenderHandle>()?;
        if sender
            .send_registration_code(email, &code, self.settings.ttl_seconds)
            .await
            .is_err()
        {
            let _: i64 = cache
                .eval_script(
                    &cache.script(DELETE_IF_CURRENT_SCRIPT),
                    &[code_key],
                    &[digest],
                )
                .await?;
            metrics::counter!("yang_system_registration_email_total", "result" => "failed")
                .increment(1);
            return Err(BaseError::HttpRequestFailed("邮件服务暂不可用".to_string()));
        }
        metrics::counter!("yang_system_registration_email_total", "result" => "sent").increment(1);
        Ok(self.accepted())
    }

    pub(super) async fn consume(
        &self,
        ctx: &ActionContext,
        email: &str,
        code: &str,
    ) -> Result<(), BaseError> {
        if code.len() != CODE_DIGITS || !code.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid_code());
        }
        let fingerprint = email_fingerprint(&self.settings.secret, email);
        let key = format!("{}:code:{fingerprint}", self.key_prefix());
        let digest = code_digest(&self.settings.secret, email, code);
        let cache = ctx.tools().cache()?;
        let result: i64 = cache
            .eval_script(
                &cache.script(VERIFY_AND_CONSUME_SCRIPT),
                &[key],
                &[digest, self.settings.max_attempts.to_string()],
            )
            .await?;
        if result != 1 {
            metrics::counter!("yang_system_registration_email_verify_total", "result" => "denied")
                .increment(1);
            return Err(invalid_code());
        }
        metrics::counter!("yang_system_registration_email_verify_total", "result" => "consumed")
            .increment(1);
        Ok(())
    }

    fn key_prefix(&self) -> String {
        format!("yang-system:{}:registration-email", self.settings.namespace)
    }

    fn accepted(&self) -> RegistrationEmailCodeAccepted {
        RegistrationEmailCodeAccepted {
            accepted: true,
            expires_in: self.settings.ttl_seconds,
            resend_after: self.settings.resend_cooldown_seconds,
        }
    }
}

fn generate_code() -> String {
    let unbiased_upper_bound = u32::MAX - (u32::MAX % CODE_SPACE);
    let value = loop {
        let candidate = OsRng.next_u32();
        if candidate < unbiased_upper_bound {
            break candidate % CODE_SPACE;
        }
    };
    format!("{value:0CODE_DIGITS$}")
}

fn client_ip_fingerprint(secret: &str, ctx: &ActionContext) -> String {
    keyed_digest(
        secret,
        &[
            b"registration-email-ip-v1",
            client_ip_identity(ctx).as_bytes(),
        ],
    )
}

fn email_fingerprint(secret: &str, email: &str) -> String {
    keyed_digest(secret, &[b"registration-email-key-v1", email.as_bytes()])
}

fn code_digest(secret: &str, email: &str, code: &str) -> String {
    keyed_digest(
        secret,
        &[
            b"registration-email-code-v1",
            email.as_bytes(),
            code.as_bytes(),
        ],
    )
}

fn keyed_digest(secret: &str, parts: &[&[u8]]) -> String {
    let mut hasher = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .unwrap_or_else(|_| unreachable!("HMAC-SHA256 接受任意长度密钥"));
    for part in parts {
        hasher.update(&(part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    hasher
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn invalid_email() -> BaseError {
    BaseError::ParamInvalid("email".to_string(), "邮箱格式无效".to_string())
}

fn invalid_code() -> BaseError {
    BaseError::ParamInvalid(
        "email_code".to_string(),
        "邮箱验证码无效或已过期".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_is_canonical_ascii_and_rejects_ambiguous_shapes() {
        assert_eq!(
            normalize_email(" Alice.Tag+demo@Example.COM ")
                .unwrap_or_else(|error| panic!("合法邮箱应规范化: {error}")),
            "alice.tag+demo@example.com"
        );
        for invalid in [
            "alice@example",
            "alice@@example.com",
            ".alice@example.com",
            "alice..tag@example.com",
            "alice@-example.com",
            "用户@example.com",
        ] {
            assert!(normalize_email(invalid).is_err(), "应拒绝 {invalid:?}");
        }
    }

    #[test]
    fn generated_code_and_redis_material_do_not_expose_plaintext_identity() {
        let code = generate_code();
        assert_eq!(code.len(), CODE_DIGITS);
        assert!(code.bytes().all(|byte| byte.is_ascii_digit()));

        let email = "alice@example.com";
        let secret = "independent-email-verification-secret-32-bytes";
        let fingerprint = email_fingerprint(secret, email);
        let digest = code_digest(secret, email, &code);
        assert_eq!(fingerprint.len(), 64);
        assert_eq!(digest.len(), 64);
        assert!(!fingerprint.contains(email));
        assert!(!digest.contains(email));
        assert!(!digest.contains(&code));
    }
}

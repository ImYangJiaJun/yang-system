//! 飞书 SSO 的短时状态：登录 state、账号选择 pending token、MFA pending token。
//!
//! 与管理员 OAuth 的 `user_token` state 分开：这里的每条记录带 flow、nonce 摘要、
//! PKCE verifier 与（绑定流程的）user_id，生命周期都是 10 分钟。进程重启丢状态
//! 是失败闭合——用户重新点一次登录即可，不落库、不引 Redis。

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use base64::Engine;
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};

use yang_base::action::ActionContext;
use yang_base::BaseError;

use crate::addon::account::ExternalIdentityProfile;
use crate::addon::feishu::domain::user_token::TokenBundle;

/// 登录 state / 选择页 / MFA 的统一有效期。
const TTL_SECONDS: i64 = 600;
/// 内存表上限：防止反复打 `/authorize` 撑爆进程内存。
const MAX_ENTRIES: usize = 10_000;

pub(crate) const NONCE_COOKIE: &str = "feishu_login_nonce";
pub(crate) const NONCE_COOKIE_PATH: &str = "/api/v1/auth/feishu";

#[derive(Debug, Clone, Copy)]
pub(crate) enum StateFlow {
    Login,
    Bind { user_id: i64 },
}

#[derive(Debug, Clone)]
pub(crate) struct StateRecord {
    pub(crate) flow: StateFlow,
    pub(crate) nonce_hash: String,
    pub(crate) code_verifier: String,
    pub(crate) redirect: String,
    pub(crate) expires_at: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingChoice {
    pub(crate) profile: ExternalIdentityProfile,
    pub(crate) bundle: TokenBundle,
    pub(crate) redirect: String,
    pub(crate) nonce_hash: String,
    pub(crate) email_matches_existing: bool,
    pub(crate) expires_at: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingLogin {
    pub(crate) user_id: i64,
    pub(crate) redirect: String,
    pub(crate) nonce_hash: String,
    pub(crate) expires_at: i64,
}

static STATES: LazyLock<Mutex<HashMap<String, StateRecord>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static CHOICES: LazyLock<Mutex<HashMap<String, PendingChoice>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static LOGINS: LazyLock<Mutex<HashMap<String, PendingLogin>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn with_map<T, R>(
    map: &LazyLock<Mutex<HashMap<String, T>>>,
    action: impl FnOnce(&mut HashMap<String, T>) -> R,
) -> R {
    let mut guard = map.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    action(&mut guard)
}

fn mint<T: Clone + ExpiresAt>(
    map: &LazyLock<Mutex<HashMap<String, T>>>,
    value: T,
    now: i64,
) -> Result<String, BaseError> {
    let token = random_token();
    let mut inserted = false;
    with_map(map, |entries| {
        entries.retain(|_, record| record.expires_at() > now);
        if entries.len() >= MAX_ENTRIES {
            return;
        }
        entries.insert(token.clone(), value);
        inserted = true;
    });
    if !inserted {
        return Err(BaseError::RateLimitExceeded {
            retry_after_seconds: 1,
        });
    }
    Ok(token)
}

/// `StateRecord` / `PendingChoice` / `PendingLogin` 的过期时刻投影。
trait ExpiresAt {
    fn expires_at(&self) -> i64;
}

impl ExpiresAt for StateRecord {
    fn expires_at(&self) -> i64 {
        self.expires_at
    }
}

impl ExpiresAt for PendingChoice {
    fn expires_at(&self) -> i64 {
        self.expires_at
    }
}

impl ExpiresAt for PendingLogin {
    fn expires_at(&self) -> i64 {
        self.expires_at
    }
}

pub(crate) fn random_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub(crate) fn hash_nonce(nonce: &str) -> String {
    let digest = Sha256::digest(nonce.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

pub(crate) fn pkce_pair() -> (String, String) {
    let verifier = random_token();
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

pub(crate) fn mint_login_state(
    redirect: &str,
    nonce: &str,
    code_verifier: &str,
    now: i64,
) -> Result<String, BaseError> {
    mint(
        &STATES,
        StateRecord {
            flow: StateFlow::Login,
            nonce_hash: hash_nonce(nonce),
            code_verifier: code_verifier.to_string(),
            redirect: redirect.to_string(),
            expires_at: now + TTL_SECONDS,
        },
        now,
    )
}

pub(crate) fn mint_bind_state(
    user_id: i64,
    redirect: &str,
    nonce: &str,
    code_verifier: &str,
    now: i64,
) -> Result<String, BaseError> {
    mint(
        &STATES,
        StateRecord {
            flow: StateFlow::Bind { user_id },
            nonce_hash: hash_nonce(nonce),
            code_verifier: code_verifier.to_string(),
            redirect: redirect.to_string(),
            expires_at: now + TTL_SECONDS,
        },
        now,
    )
}

pub(crate) fn consume_state(state: &str, now: i64) -> Option<StateRecord> {
    let mut guard = STATES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.retain(|_, record| record.expires_at > now);
    guard.remove(state)
}

pub(crate) fn mint_pending_choice(
    profile: ExternalIdentityProfile,
    bundle: TokenBundle,
    redirect: &str,
    nonce: &str,
    email_matches_existing: bool,
    now: i64,
) -> Result<String, BaseError> {
    mint(
        &CHOICES,
        PendingChoice {
            profile,
            bundle,
            redirect: redirect.to_string(),
            nonce_hash: hash_nonce(nonce),
            email_matches_existing,
            expires_at: now + TTL_SECONDS,
        },
        now,
    )
}

pub(crate) fn peek_choice(token: &str, nonce: &str, now: i64) -> Option<PendingChoice> {
    let guard = CHOICES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .get(token)
        .filter(|record| record.expires_at > now && record.nonce_hash == hash_nonce(nonce))
        .cloned()
}

pub(crate) fn consume_choice(token: &str, nonce: &str, now: i64) -> Option<PendingChoice> {
    let mut guard = CHOICES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.retain(|_, record| record.expires_at > now);
    let matches = guard
        .get(token)
        .is_some_and(|record| record.nonce_hash == hash_nonce(nonce));
    if !matches {
        return None;
    }
    guard.remove(token)
}

pub(crate) fn mint_pending_login(
    user_id: i64,
    redirect: &str,
    nonce: &str,
    now: i64,
) -> Result<String, BaseError> {
    mint(
        &LOGINS,
        PendingLogin {
            user_id,
            redirect: redirect.to_string(),
            nonce_hash: hash_nonce(nonce),
            expires_at: now + TTL_SECONDS,
        },
        now,
    )
}

pub(crate) fn peek_pending_login(token: &str, nonce: &str, now: i64) -> Option<PendingLogin> {
    let guard = LOGINS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .get(token)
        .filter(|record| record.expires_at > now && record.nonce_hash == hash_nonce(nonce))
        .cloned()
}

pub(crate) fn consume_pending_login(token: &str, nonce: &str, now: i64) -> Option<PendingLogin> {
    let mut guard = LOGINS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.retain(|_, record| record.expires_at > now);
    let matches = guard
        .get(token)
        .is_some_and(|record| record.nonce_hash == hash_nonce(nonce));
    if !matches {
        return None;
    }
    guard.remove(token)
}

/// 从请求 Cookie 头取 nonce（不存在/空返回 None）。
pub(crate) fn nonce_from_request(ctx: &ActionContext) -> Option<String> {
    ctx.request.cookie().and_then(|header| {
        header.split(';').find_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            (name == NONCE_COOKIE && !value.is_empty()).then(|| value.to_string())
        })
    })
}

/// nonce Cookie：SameSite=Lax 是必须的——飞书回调是跨站顶层导航，Strict 不会带上。
pub(crate) fn nonce_cookie(nonce: &str, secure: bool) -> String {
    format!(
        "{NONCE_COOKIE}={nonce}; Path={NONCE_COOKIE_PATH}; HttpOnly; SameSite=Lax; Max-Age=600{}",
        if secure { "; Secure" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::feishu::domain::user_token::TokenBundle;

    fn bundle(now: i64) -> TokenBundle {
        TokenBundle {
            access_token: "a".to_string(),
            access_token_expires_at: now + 3600,
            refresh_token: "r".to_string(),
            refresh_token_expires_at: now + 7200,
            scope: None,
        }
    }

    #[test]
    fn pkce_challenge_is_s256_and_url_safe() {
        let (verifier, challenge) = pkce_pair();
        assert!(!verifier.is_empty());
        assert!(!challenge.is_empty());
        assert!(!verifier.contains('='));
        assert!(!challenge.contains('='));
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, expected);
    }

    #[test]
    fn login_state_is_one_time_and_nonce_is_hashed() {
        let now = 1_000_i64;
        let state = mint_login_state("/batch-approval", "nonce-1", "verifier", now)
            .unwrap_or_else(|error| panic!("state 应可签发: {error}"));
        let record = consume_state(&state, now).unwrap_or_else(|| panic!("state 应命中"));
        assert!(matches!(record.flow, StateFlow::Login));
        assert_eq!(record.redirect, "/batch-approval");
        assert_eq!(record.code_verifier, "verifier");
        assert_eq!(record.nonce_hash, hash_nonce("nonce-1"));
        assert!(consume_state(&state, now).is_none(), "state 必须一次性");
    }

    #[test]
    fn choice_peek_is_read_only_and_consume_checks_nonce() {
        let now = 2_000_i64;
        let profile = ExternalIdentityProfile::feishu(
            "tenant",
            "ou_1",
            None,
            Some("张三".to_string()),
            Some("z***@corp.com".to_string()),
            None,
        );
        let token = mint_pending_choice(
            profile,
            bundle(now),
            "/batch-approval",
            "nonce-2",
            true,
            now,
        )
        .unwrap_or_else(|error| panic!("choice 应可签发: {error}"));
        assert!(peek_choice(&token, "wrong", now).is_none());
        let pending = peek_choice(&token, "nonce-2", now).unwrap_or_else(|| panic!("应命中"));
        assert!(pending.email_matches_existing);
        assert_eq!(pending.redirect, "/batch-approval");
        assert!(
            peek_choice(&token, "nonce-2", now).is_some(),
            "peek 不得消费"
        );
        assert!(consume_choice(&token, "nonce-2", now).is_some());
        assert!(peek_choice(&token, "nonce-2", now).is_none());
    }

    #[test]
    fn expired_state_is_rejected() {
        let now = 3_000_i64;
        let state = mint_login_state("/", "nonce", "verifier", now)
            .unwrap_or_else(|error| panic!("state 应可签发: {error}"));
        assert!(consume_state(&state, now + TTL_SECONDS + 1).is_none());
    }
}

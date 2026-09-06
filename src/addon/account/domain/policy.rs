//! 用户领域不变量的唯一事实源。

use yang_base::BaseError;

pub(crate) const USERNAME_MIN_LENGTH: usize = 3;
pub(crate) const USERNAME_MAX_LENGTH: usize = 64;
pub(crate) const USERNAME_PATTERN: &str = "^[A-Za-z0-9_-]+$";
pub(crate) const PASSWORD_MIN_LENGTH: usize = 10;
pub(crate) const PASSWORD_MAX_LENGTH: usize = 128;

pub(crate) fn normalize_username(username: &str) -> Result<String, BaseError> {
    let normalized = username.trim().to_ascii_lowercase();
    let length = normalized.len();
    if !(USERNAME_MIN_LENGTH..=USERNAME_MAX_LENGTH).contains(&length) {
        return Err(BaseError::ParamInvalid(
            "username".to_string(),
            format!("长度必须在 {USERNAME_MIN_LENGTH}..={USERNAME_MAX_LENGTH} 之间"),
        ));
    }
    if !normalized
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(BaseError::ParamInvalid(
            "username".to_string(),
            "只允许 ASCII 字母、数字、下划线和连字符".to_string(),
        ));
    }
    Ok(normalized)
}

/// 内置常见弱口令名单（不引外部服务，覆盖撞库与默认口令的高频组合）。
///
/// 大小写不敏感匹配；名单刻意保持精简、可审查，避免把合法复杂口令误伤。
pub(crate) const WEAK_PASSWORDS: &[&str] = &[
    "password",
    "password1",
    "password123",
    "123456",
    "12345678",
    "123456789",
    "1234567890",
    "qwerty",
    "qwerty123",
    "abc123",
    "letmein",
    "admin",
    "admin123",
    "welcome",
    "welcome1",
    "monkey",
    "dragon",
    "football",
    "iloveyou",
    "654321",
    "111111",
    "000000",
];

pub(crate) fn validate_new_password(password: &str) -> Result<(), BaseError> {
    validate_password_field("new_password", password, None)
}

/// 注册/改密/重置共享的密码校验；`username` 非空时追加「禁止与用户名相同」规则。
pub(crate) fn validate_password_field(
    field: &str,
    password: &str,
    username: Option<&str>,
) -> Result<(), BaseError> {
    let length = password.chars().count();
    if !(PASSWORD_MIN_LENGTH..=PASSWORD_MAX_LENGTH).contains(&length) {
        return Err(BaseError::ParamInvalid(
            field.to_string(),
            format!("长度必须在 {PASSWORD_MIN_LENGTH}..={PASSWORD_MAX_LENGTH} 之间"),
        ));
    }
    let normalized = password.to_ascii_lowercase();
    if WEAK_PASSWORDS
        .iter()
        .any(|weak| weak.eq_ignore_ascii_case(&normalized))
    {
        return Err(BaseError::ParamInvalid(
            field.to_string(),
            "密码过于常见，请换用更复杂的口令".to_string(),
        ));
    }
    if let Some(username) = username {
        let username = username.trim().to_ascii_lowercase();
        if !username.is_empty() && normalized == username {
            return Err(BaseError::ParamInvalid(
                field.to_string(),
                "密码不能与用户名相同".to_string(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_policy_normalizes_and_rejects_unsupported_characters() {
        assert_eq!(
            normalize_username(" Alice-01 ")
                .unwrap_or_else(|error| panic!("用户名应合法: {error}")),
            "alice-01"
        );
        assert!(normalize_username("用户").is_err());
        assert!(normalize_username("ab").is_err());
    }

    #[test]
    fn password_policy_uses_character_count() {
        assert!(validate_new_password("correct-horse-battery").is_ok());
        assert!(validate_new_password("123456789").is_err());
        assert!(validate_new_password(&"x".repeat(PASSWORD_MAX_LENGTH + 1)).is_err());
    }

    #[test]
    fn change_password_reports_the_new_password_field() {
        assert!(matches!(
            validate_new_password("too-short"),
            Err(BaseError::ParamInvalid(field, _)) if field == "new_password"
        ));
    }

    #[test]
    fn weak_password_dictionary_is_rejected_case_insensitively() {
        assert!(matches!(
            validate_new_password("Password123"),
            Err(BaseError::ParamInvalid(field, message)) if field == "new_password" && message.contains("常见")
        ));
        assert!(matches!(
            validate_new_password("123456"),
            Err(BaseError::ParamInvalid(field, _)) if field == "new_password"
        ));
    }

    #[test]
    fn password_must_not_equal_username() {
        assert!(matches!(
            validate_password_field("new_password", "alicealice", Some("alicealice")),
            Err(BaseError::ParamInvalid(field, message)) if field == "new_password" && message.contains("用户名")
        ));
        // 包含用户名子串但整体不同的长口令允许（弱口令字典覆盖高频组合）。
        assert!(
            validate_password_field("new_password", "alice-secret-2024", Some("alice")).is_ok()
        );
        assert!(
            validate_password_field("new_password", "correct-horse-battery", Some("alice")).is_ok()
        );
    }
}

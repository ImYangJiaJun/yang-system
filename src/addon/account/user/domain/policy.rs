//! 用户领域不变量的唯一事实源。

use yang_base::BaseError;

pub(in crate::addon::account::user) const USERNAME_MIN_LENGTH: usize = 3;
pub(in crate::addon::account::user) const USERNAME_MAX_LENGTH: usize = 64;
pub(in crate::addon::account::user) const USERNAME_PATTERN: &str = "^[A-Za-z0-9_-]+$";
pub(in crate::addon::account::user) const PASSWORD_MIN_LENGTH: usize = 10;
pub(in crate::addon::account::user) const PASSWORD_MAX_LENGTH: usize = 128;

pub(in crate::addon::account::user) fn normalize_username(
    username: &str,
) -> Result<String, BaseError> {
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

pub(in crate::addon::account::user) fn validate_password(password: &str) -> Result<(), BaseError> {
    validate_password_field("password", password)
}

pub(in crate::addon::account::user) fn validate_new_password(
    password: &str,
) -> Result<(), BaseError> {
    validate_password_field("new_password", password)
}

fn validate_password_field(field: &str, password: &str) -> Result<(), BaseError> {
    let length = password.chars().count();
    if !(PASSWORD_MIN_LENGTH..=PASSWORD_MAX_LENGTH).contains(&length) {
        return Err(BaseError::ParamInvalid(
            field.to_string(),
            format!("长度必须在 {PASSWORD_MIN_LENGTH}..={PASSWORD_MAX_LENGTH} 之间"),
        ));
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
        assert!(validate_password("1234567890").is_ok());
        assert!(validate_password("123456789").is_err());
        assert!(validate_password(&"x".repeat(PASSWORD_MAX_LENGTH + 1)).is_err());
    }

    #[test]
    fn change_password_reports_the_new_password_field() {
        assert!(matches!(
            validate_new_password("too-short"),
            Err(BaseError::ParamInvalid(field, _)) if field == "new_password"
        ));
    }
}

//! 用户状态的唯一领域表示。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use yang_base::BaseError;

/// 用户账号只允许处于启用或停用状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UserStatus {
    Active,
    Disabled,
}

impl UserStatus {
    pub(crate) const ACTIVE: &'static str = "active";
    pub(crate) const DISABLED: &'static str = "disabled";

    /// 从持久化字符串恢复领域值；未知值按数据库类型损坏失败关闭。
    pub(crate) fn from_storage(value: &str) -> Result<Self, BaseError> {
        match value {
            Self::ACTIVE => Ok(Self::Active),
            Self::DISABLED => Ok(Self::Disabled),
            other => Err(BaseError::from(yang_db::DbError::TypeConversionError(
                format!("users.status 包含未知值: {other:?}"),
            ))),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Active => Self::ACTIVE,
            Self::Disabled => Self::DISABLED,
        }
    }

    pub(crate) const fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }
}

impl fmt::Display for UserStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_mapping_is_exact_and_unknown_values_fail_closed() {
        assert_eq!(
            UserStatus::from_storage("active")
                .unwrap_or_else(|error| panic!("active 应有效: {error}")),
            UserStatus::Active
        );
        assert_eq!(
            UserStatus::from_storage("disabled")
                .unwrap_or_else(|error| panic!("disabled 应有效: {error}")),
            UserStatus::Disabled
        );
        for invalid in ["ACTIVE", "pending", "", " active"] {
            assert!(
                UserStatus::from_storage(invalid).is_err(),
                "{invalid:?} 必须被拒绝"
            );
        }
    }

    #[test]
    fn json_contract_uses_database_wire_values() {
        assert_eq!(
            serde_json::to_value(UserStatus::Active)
                .unwrap_or_else(|error| panic!("状态应可序列化: {error}")),
            serde_json::json!("active")
        );
        assert_eq!(UserStatus::Disabled.to_string(), "disabled");
    }
}

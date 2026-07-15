use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use yang_base::table::{FieldPermissions, TableConfig, TableEntity as TableEntityTrait};
use yang_base::{BaseError, TableEntity};

pub(super) const SYSTEM_ROLE: &str = "system";

#[derive(Clone, Deserialize, Serialize, JsonSchema, sqlx::FromRow, TableEntity)]
#[table(name = "accounts")]
pub(super) struct AccountRow {
    #[entity(primary_key, auto_increment)]
    pub(super) id: i64,
    #[entity(max_length = 64, unique)]
    pub(super) username: String,
    #[entity(max_length = 255)]
    #[serde(skip_serializing)]
    #[schemars(skip)]
    pub(super) password_hash: String,
    #[entity(max_length = 16)]
    pub(super) status: String,
    pub(super) created_at: i64,
    pub(super) updated_at: i64,
}

pub(super) fn account_table_config() -> Result<Arc<TableConfig>, BaseError> {
    let mut config = AccountRow::table_config()
        .clone()
        .display_name("账号")
        .timestamps(true, true, false);
    let protected = HashSet::from([SYSTEM_ROLE.to_string()]);
    let password = config.fields.get_mut("password_hash").ok_or_else(|| {
        BaseError::ConfigError("AccountRow 缺少 password_hash 字段配置".to_string())
    })?;
    password.permissions = FieldPermissions {
        readable_roles: protected.clone(),
        writable_roles: protected.clone(),
        filterable_roles: protected.clone(),
        sortable_roles: protected,
    };
    password.filterable = false;
    password.sortable = false;
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_schema_uses_generated_id_and_protects_password_hash() {
        let config =
            account_table_config().unwrap_or_else(|error| panic!("账号表配置应有效: {error}"));
        let id = config
            .fields
            .get("id")
            .unwrap_or_else(|| panic!("应存在 id 字段"));
        let password = config
            .fields
            .get("password_hash")
            .unwrap_or_else(|| panic!("应存在 password_hash 字段"));

        assert!(id.auto_increment);
        assert!(!password.filterable);
        assert!(!password.sortable);
        assert_eq!(password.permissions.readable_roles.len(), 1);
    }
}

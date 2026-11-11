//! `feishu_user_credential` 表声明：每用户一份飞书 OAuth 凭证。
//!
//! 与管理员单例 `feishu_user_token` 彻底分开：那张表服务审批派发的「姓名 →
//! open_id」解析，本表服务用户本人的批量审批。两套凭证的授权人、生命周期与
//! 数据访问面都不同，不能合并。

use yang_base::definition::{Int, Key, Str, TableSpec, Text, Timestamp};
use yang_base::BaseError;

use super::repository::SYSTEM_ROLE;

/// 凭证状态：可用。
pub(crate) const CREDENTIAL_ACTIVE: &str = "active";
/// 凭证状态：需要用户重新授权。
pub(crate) const CREDENTIAL_REAUTH_REQUIRED: &str = "reauth_required";

pub(crate) const CREDENTIAL_ID: &str = "id";
pub(crate) const CREDENTIAL_IDENTITY_ID: &str = "identity_id";
pub(crate) const CREDENTIAL_USER_ID: &str = "user_id";
pub(crate) const CREDENTIAL_OPEN_ID: &str = "open_id";
pub(crate) const CREDENTIAL_ACCESS_TOKEN: &str = "access_token";
pub(crate) const CREDENTIAL_REFRESH_TOKEN: &str = "refresh_token";
pub(crate) const CREDENTIAL_ACCESS_EXPIRES_AT: &str = "access_token_expires_at";
pub(crate) const CREDENTIAL_REFRESH_EXPIRES_AT: &str = "refresh_token_expires_at";
pub(crate) const CREDENTIAL_SCOPE: &str = "scope";
pub(crate) const CREDENTIAL_AUTHORIZED_AT: &str = "authorized_at";
pub(crate) const CREDENTIAL_STATUS: &str = "status";
pub(crate) const CREDENTIAL_LAST_REFRESHED_AT: &str = "last_refreshed_at";

pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    Ok(TableSpec::new(yang_base::table!("feishu_user_credential"))
        .title("飞书用户令牌")
        .fields(yang_base::fields! {
            id => Key::new().title("ID").filterable(true),
            identity_id => Int::new()
                .title("身份绑定 ID")
                .require(true)
                .unique(true)
                .filterable(true),
            user_id => Int::new().title("系统用户 ID").require(true).filterable(true),
            open_id => Str::new().title("飞书 Open ID").require(true).max_length(64).filterable(true),
            access_token => Text::new()
                .title("用户访问令牌")
                .require(true)
                .secret(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
            refresh_token => Text::new()
                .title("刷新令牌")
                .require(true)
                .secret(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
            access_token_expires_at => Timestamp::new()
                .title("访问令牌过期时间")
                .require(true),
            refresh_token_expires_at => Timestamp::new()
                .title("刷新令牌过期时间")
                .require(true),
            scope => Str::new().title("授权范围快照").max_length(1024),
            authorized_at => Timestamp::new()
                .title("最近一次完整授权时间")
                .require(true),
            status => Str::new()
                .title("凭证状态")
                .require(true)
                .max_length(16)
                .default(CREDENTIAL_ACTIVE)
                .filterable(true),
            last_refreshed_at => Timestamp::new().title("最近刷新时间"),
            created_at => Timestamp::new().title("创建时间").created_at(),
            updated_at => Timestamp::new().title("更新时间").updated_at(),
        })
        .check_named(
            "chk_feishu_user_credential_status",
            "`status` IN ('active', 'reauth_required', 'revoked')",
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_pins_one_credential_per_identity() {
        let spec = table_spec().unwrap_or_else(|error| panic!("凭证表声明应有效: {error}"));
        assert_eq!(spec.name.as_str(), "feishu_user_credential");
        let identity = spec
            .fields
            .iter()
            .find(|field| field.name.as_str() == CREDENTIAL_IDENTITY_ID)
            .unwrap_or_else(|| panic!("identity_id 字段必须存在"));
        assert!(identity.storage.unique, "identity_id 必须唯一");
        let definition = spec
            .table_definition()
            .unwrap_or_else(|error| panic!("凭证表定义应有效: {error}"));
        let access = definition
            .field(CREDENTIAL_ACCESS_TOKEN)
            .unwrap_or_else(|| panic!("access_token 字段必须存在"));
        assert!(access.is_secret(), "access_token 必须是 secret");
    }
}

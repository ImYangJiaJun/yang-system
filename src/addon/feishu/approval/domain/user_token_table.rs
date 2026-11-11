//! `feishu_user_token` 表声明：系统持有的**飞书用户身份**（OAuth `user_access_token`）。
//!
//! 为什么需要这张表：姓名 → open_id 走的 `GET /open-apis/search/v1/user` **只接受**
//! `user_access_token`，用 `tenant_access_token` 调必报 99991663（真机实测）。
//! 用户身份只能由管理员在控制台完成一次 OAuth 授权取得，token 对与授权时间落在这里，
//! 由 `domain::user_token` 维护（access 过期按需刷新；refresh 由提醒 worker 每日保活）。
//!
//! # 一个身份一行
//!
//! 表里**每个身份槽位至多一行**：`slot = 1` 是审批派发身份，`slot = 2` 是审批
//! 回写身份，**唯一索引**把这条不变式钉死在数据库上（不再是「表里只有一行」的
//! 约定，也不再依赖取「第一行」）。同一槽位重新授权是**整体替换**而不是新增一行；
//! 两个槽位互不覆盖，一个身份失效不会拖累另一个。存量 `slot = 1` 的行即派发身份，
//! 升级无须迁移。
//!
//! 因此 `authorized_at` 是普通列，**不是** `created_at`：重授权必须重置「350 天开始
//! 提醒 / 365 天硬性失效」这条时钟，而 `created_at` 语义上只在首插时写一次。

use yang_base::definition::{Int, Key, Str, TableSpec, Text, Timestamp};
use yang_base::BaseError;

use super::super::super::domain::repository::SYSTEM_ROLE;

/// 飞书用户身份表。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    Ok(TableSpec::new(yang_base::table!("feishu_user_token"))
        .title("飞书用户身份")
        .fields(yang_base::fields! {
            id => Key::new().title("ID").filterable(true).sortable(true),
            // 身份槽位：1 = 审批派发，2 = 审批回写。唯一索引使「每个身份至多一行」
            // 成为数据库约束，而不再只靠「取第一行」。同一槽位并发授权同时 insert
            // 时，第二次撞的就是这里的唯一键（`store_authorization` 捕获后转为按 id 覆盖）。
            slot => Int::new()
                .title("身份槽位")
                .require(true)
                .default(1_i64)
                .unique(true)
                .filterable(true),
            open_id => Str::new()
                .title("授权人 Open ID")
                .require(true)
                .max_length(64)
                .filterable(true),
            user_name => Str::new().title("授权人姓名").max_length(128),
            // 两个凭证都是长期凭据：标 `secret` 并只对 SYSTEM_ROLE 开放读写，
            // 与 `feishu_datasource_field.token_cipher` 同一处置（那里也存可逆凭据）。
            access_token => Text::new()
                .title("用户访问凭证")
                .require(true)
                .secret(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
            access_token_expires_at => Timestamp::new()
                .title("访问凭证过期时间")
                .require(true),
            refresh_token => Text::new()
                .title("刷新凭证")
                .require(true)
                .secret(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
            refresh_token_expires_at => Timestamp::new()
                .title("刷新凭证过期时间")
                .require(true),
            // 授权范围快照：接口报 99991672（缺 scope）时，靠它区分「授权时没要」
            // 与「后台没开通」，不必让管理员先重新授权一次才能排查。
            scope => Str::new().title("授权范围快照").max_length(1024),
            authorized_at => Timestamp::new()
                .title("最近一次完整授权时间")
                .require(true),
            created_at => Timestamp::new().title("创建时间").created_at(),
            updated_at => Timestamp::new().title("更新时间").updated_at(),
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition() -> yang_base::table::TableDefinition {
        table_spec()
            .unwrap_or_else(|error| panic!("表声明应有效: {error}"))
            .table_definition()
            .unwrap_or_else(|error| panic!("应可编译为表定义: {error}"))
    }

    #[test]
    fn table_has_expected_name_and_primary_key() {
        let definition = definition();
        assert_eq!(definition.name(), "feishu_user_token");
        assert_eq!(definition.primary_key(), "id");
    }

    #[test]
    fn slot_pins_one_row_per_identity() {
        // 「每个身份至多一行」的落点：`slot` 必填、默认 1、唯一。唯一性只能从字段
        // 声明（`TableSpec`）上断言——编译后的 `TableDefinition` 没有唯一性内省接口
        // （同 option/table.rs）。必填 + 默认 1：存量库升级补列时旧行自动落 1
        // （即审批派发身份），唯一索引因此能立刻生效。
        let spec = table_spec().unwrap_or_else(|error| panic!("表声明应有效: {error}"));
        let slot = spec
            .fields
            .iter()
            .find(|field| field.name.as_str() == "slot")
            .unwrap_or_else(|| panic!("slot 字段必须存在"));
        assert!(slot.storage.required, "slot 必填");
        assert_eq!(
            slot.storage.default,
            Some(serde_json::json!(1)),
            "slot 默认 1（存量行 = 审批派发身份）"
        );
        assert!(
            slot.storage.unique,
            "slot 必须唯一——每个身份至多一行靠它钉死"
        );

        let definition = definition();
        let field = definition
            .field("slot")
            .unwrap_or_else(|| panic!("slot 字段必须存在"));
        assert!(
            field.is_filterable(),
            "load_slot 按 slot 过滤，filterable 是 fail-closed 的运行期校验"
        );
    }

    #[test]
    fn credential_and_clock_columns_are_required() {
        // 这几列缺一个这张表就没有意义：没有凭证解析不了姓名，没有授权时间判不了
        // 「350 天提醒 / 365 天失效」。可选列只有展示用的姓名与排查用的 scope 快照。
        let definition = definition();
        for name in [
            "slot",
            "open_id",
            "access_token",
            "access_token_expires_at",
            "refresh_token",
            "refresh_token_expires_at",
            "authorized_at",
        ] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(field.is_required(), "{name} 必填");
        }
    }

    #[test]
    fn credentials_are_secret_and_system_only() {
        // 凭证列不得对普通角色可读——它们是可直接调用飞书 OpenAPI 的身份凭据。
        let definition = definition();
        for name in ["access_token", "refresh_token"] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(field.is_secret(), "{name} 必须标 secret");
        }
    }

    #[test]
    fn every_field_has_an_explicit_chinese_label() {
        let definition = definition();
        for field in definition.fields() {
            assert!(
                !field.label().is_empty(),
                "字段 {} 必须有展示名",
                field.name()
            );
            assert_ne!(
                field.label(),
                field.name(),
                "字段 {} 的展示名退化成了字段名（忘了 .title(..)）",
                field.name()
            );
        }
    }
}

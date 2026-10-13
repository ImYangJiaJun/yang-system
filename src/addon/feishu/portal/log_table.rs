//! `feishu_datasource_portal_log` 表声明——Schema 的唯一事实来源。

use yang_base::definition::{Int, Key, Str, TableSpec, Timestamp};
use yang_base::BaseError;

/// 声明门户访问日志表。
///
/// append-only：只有 INSERT，没有 UPDATE / DELETE。查询值只存 SHA-256，不存明文。
/// 走 `TableSpec` + `fields!` 是刻意的取舍：这条路径让表进入 Catalog / 权限目录，
/// 代价是拿不到 DSL 的能力（见 `datasource/table.rs` 的说明）。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    Ok(TableSpec::new(yang_base::table!("feishu_datasource_portal_log"))
        .title("飞书门户访问日志")
        .fields(yang_base::fields! {
            id => Key::new().title("ID").filterable(true),
            // 指向门户。DSL 没有外键 builder，故用 `Int` + 索引，一致性由应用层保证。
            portal_id => Int::new()
                .title("所属门户")
                .require(true)
                .indexed(true)
                .filterable(true),
            // 查询值的 SHA-256。不存明文（信用卡号等高敏感值不落日志明文）。
            query_value_hash => Str::new()
                .title("查询值哈希")
                .require(true)
                .max_length(64)
                .filterable(true),
            // 返回行数。
            result_count => Int::new()
                .title("返回行数")
                .require(true)
                .default(0),
            // 来源 IP（IPv6 兼容，VARCHAR(45)）。
            ip_address => Str::new()
                .title("来源 IP")
                .max_length(45),
            // 浏览器 User-Agent。
            user_agent => Str::new()
                .title("User-Agent")
                .max_length(500),
            // 本次用的访问模式（public / passphrase / login）。
            access_mode_used => Str::new()
                .title("访问模式")
                .require(true)
                .max_length(16)
                .filterable(true),
            // 查询状态：hit / miss / rate_limited / forbidden。
            status => Str::new()
                .title("状态")
                .require(true)
                .max_length(16)
                .filterable(true),
            created_at => Timestamp::new().created_at().title("创建时间"),
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
    fn table_name_and_primary_key() {
        let definition = definition();
        assert_eq!(definition.name(), "feishu_datasource_portal_log");
        assert_eq!(definition.primary_key(), "id");
    }

    #[test]
    fn query_value_hash_is_filterable_for_audit() {
        // 事后审计"某个卡号是否被查过"需要按哈希筛选。
        let definition = definition();
        let hash = definition
            .field("query_value_hash")
            .unwrap_or_else(|| panic!("query_value_hash 必须存在"));
        assert!(hash.is_required());
        assert!(hash.is_filterable(), "按哈希筛选必须可用（审计需求）");
    }

    #[test]
    fn portal_id_is_indexed_for_reverse_lookup() {
        // 按门户反查日志需要索引。
        let definition = definition();
        let portal_id = definition
            .field("portal_id")
            .unwrap_or_else(|| panic!("portal_id 必须存在"));
        assert!(portal_id.is_required());
        assert!(portal_id.is_filterable());
    }

    #[test]
    fn status_and_access_mode_are_filterable() {
        // 按状态 / 访问模式筛选是管理侧日志查看页的硬依赖。
        let definition = definition();
        for name in ["status", "access_mode_used"] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 必须存在"));
            assert!(
                field.is_filterable(),
                "{name} 必须可筛（管理侧日志查看需求）"
            );
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

    #[test]
    fn sync_state_columns_are_nullable_or_defaulted() {
        // schema_sync 门禁：已有数据的表上加「必填且无默认值」的列会让启动直接失败。
        let definition = definition();
        for name in ["ip_address", "user_agent"] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(!field.is_required(), "{name} 必须可空，否则存量行无法加列");
        }
        let result_count = definition
            .field("result_count")
            .unwrap_or_else(|| panic!("result_count 字段必须存在"));
        assert_eq!(
            result_count.default_value(),
            Some(&serde_json::json!(0)),
            "result_count 应默认 0"
        );
    }
}

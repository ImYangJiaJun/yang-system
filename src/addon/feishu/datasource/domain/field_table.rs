//! `feishu_datasource_field` 表声明——字段绑定的 Schema 唯一事实来源。

use yang_base::definition::{Int, Key, Str, Switch, TableSpec, Text, Timestamp};
use yang_base::BaseError;

use super::super::super::domain::repository::SYSTEM_ROLE;

/// 声明字段绑定表。
///
/// 一条绑定 = 一个多维表格字段 = 一个 `source_key` = 一个审批控件。
/// `datasource_id` 指向表级行；DSL 没有外键 builder，故用 `Int` + 索引，
/// 一致性由应用层在事务内保证（表级行与绑定行同事务写入）。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    Ok(TableSpec::new(yang_base::table!("feishu_datasource_field"))
        .title("飞书数据源字段")
        .fields(yang_base::fields! {
            id => Key::new().title("ID"),
            datasource_id => Int::new()
                .title("所属数据源")
                .require(true)
                .indexed(true)
                .filterable(true),
            // 身份。**存 field_id 不存 field_name**：名字会被改，
            // 而表级拉取下 `field_names` 是一个请求参数，一个坏名字会拖垮整表。
            // `field_name` 降为缓存，每轮拉取前按 field_id 解析刷新。
            field_id => Str::new()
                .title("字段 ID")
                .require(true)
                .max_length(64)
                .filterable(true),
            field_name => Str::new().title("字段名（缓存）").max_length(255),
            // 进 URL 路径段，全局唯一。唯一索引是硬依赖：出站按它路由。
            source_key => Str::new()
                .title("数据源标识")
                .require(true)
                .unique(true)
                .max_length(64)
                .searchable(true)
                .filterable(true)
                .sortable(true),
            // 只存摘要，校验用；校验路径不需要解密。
            token_hash => Str::new()
                .title("Token 摘要")
                .require(true)
                .max_length(64)
                .secret(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
            // 可逆密文，**只在回显复制那一条路径上解密**。
            token_cipher => Text::new()
                .title("Token 密文")
                .secret(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
            token_rotated_at => Timestamp::new().title("最近轮换时间"),
            encrypt_enabled => Switch::new().title("加密返回").require(true).default(false),
            default_locale => Str::new()
                .title("默认语言")
                .require(true)
                .max_length(16)
                .default("zh_cn"),
            // 同表内的父列。多级由链涌现（A 是 B 的父、B 是 C 的父）。
            parent_field_id => Str::new()
                .title("父字段 ID")
                .max_length(64)
                .indexed(true)
                .filterable(true),
            enabled => Switch::new()
                .title("启用")
                .require(true)
                .default(true)
                .filterable(true),
            snapshot_digest => Str::new().title("快照摘要").max_length(64),
            last_push_at => Timestamp::new().title("最近推送时间"),
            created_at => Timestamp::new().created_at().title("创建时间"),
            updated_at => Timestamp::new().updated_at().title("更新时间").sortable(true),
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
        assert_eq!(definition.name(), "feishu_datasource_field");
        assert_eq!(definition.primary_key(), "id");
    }

    #[test]
    fn field_id_is_required_and_pairs_with_the_datasource() {
        // 身份是 field_id（设计 §3.2 A1），不是 field_name——改名不能断链
        let definition = definition();
        let field_id = definition
            .field("field_id")
            .unwrap_or_else(|| panic!("field_id 必须存在"));
        assert!(field_id.is_required());
        assert!(field_id.is_filterable(), "按 field_id 查绑定要能筛");
    }

    #[test]
    fn source_key_is_unique_and_filterable() {
        // 它进 URL 路径，全局唯一；出站按它路由，必须可筛
        let definition = definition();
        let source_key = definition
            .field("source_key")
            .unwrap_or_else(|| panic!("source_key 必须存在"));
        assert!(source_key.is_required());
        assert!(source_key.is_filterable());
        assert!(source_key.is_sortable());
    }

    #[test]
    fn parent_field_id_is_optional_and_filterable() {
        // 无父的字段没有它；但按父反查子要能筛
        let definition = definition();
        let parent = definition
            .field("parent_field_id")
            .unwrap_or_else(|| panic!("parent_field_id 必须存在"));
        assert!(!parent.is_required(), "无父字段没有父指针");
        assert!(parent.is_filterable());
    }

    #[test]
    fn credential_columns_are_secret_and_never_searchable() {
        let definition = definition();
        for name in ["token_hash", "token_cipher"] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 必须存在"));
            assert!(field.is_secret(), "{name} 必须是 secret");
            assert!(!field.is_searchable(), "{name} 不得进检索面");
            assert!(!field.is_filterable(), "{name} 不得进筛选面");
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
                "字段 {} 忘了 .title(..)",
                field.name()
            );
        }
    }
}

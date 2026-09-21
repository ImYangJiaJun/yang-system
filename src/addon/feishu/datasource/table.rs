//! `feishu_datasource` 表声明——Schema 的唯一事实来源。

use yang_base::definition::{Key, Radio, Str, Switch, TableSpec, Text, Timestamp};
use yang_base::BaseError;

use super::super::domain::repository::SYSTEM_ROLE;

/// 声明数据源注册表。
///
/// 走 `TableSpec` + `fields!` 是刻意的取舍：这条路径让表进入 Catalog / 前端零代码
/// TableView / 权限目录，代价是拿不到 DSL 的能力（见 `option/table.rs` 的说明）。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    Ok(TableSpec::new(yang_base::table!("feishu_datasource"))
        .title("飞书数据源")
        .fields(yang_base::fields! {
            id => Key::new().title("ID"),
            // 路由键：进外部选项接口的 URL。唯一索引保证不会出现两个同 key 的数据源
            source_key => Str::new()
                .title("数据源标识")
                .require(true)
                .unique(true)
                .max_length(64)
                .searchable(true)
                .filterable(true)
                .sortable(true),
            title => Str::new()
                .title("名称")
                .require(true)
                .max_length(100)
                .searchable(true)
                .sortable(true),
            // 只存 SHA-256 摘要，永不存明文。secret(true) 会把读写权限置为 Nobody，
            // 因此必须紧接着显式授回受信角色，否则连 writer 都读写不了
            token_hash => Str::new()
                .title("Token 摘要")
                .require(true)
                .max_length(64)
                .secret(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
            encrypt_enabled => Switch::new().title("加密返回").require(true).default(false),
            default_locale => Str::new()
                .title("默认语言")
                .require(true)
                .max_length(16)
                .default("zh_cn"),
            status => Radio::<String>::new()
                .title("状态")
                .require(true)
                .varchar(16)
                .options([("active", "启用"), ("disabled", "停用")])
                .filterable(true)
                .default("active"),
            // JSON 文本：DSL 没有 Json builder，且该列从不被 SQL 查询进内部
            linkage_mapping => Text::new().title("联动映射"),
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
    fn table_has_expected_name_and_primary_key() {
        let definition = definition();
        assert_eq!(definition.name(), "feishu_datasource");
        // 表必须定义主键，否则 schema-first build 阶段直接失败
        assert_eq!(definition.primary_key(), "id");
    }

    #[test]
    fn source_key_is_required_searchable_filterable_and_sortable() {
        let definition = definition();
        let source_key = definition
            .field("source_key")
            .unwrap_or_else(|| panic!("source_key 字段必须存在"));
        assert!(source_key.is_required(), "source_key 必填");
        // DSL 侧 filterable / sortable 是 fail-closed，未显式打开就是 false
        assert!(source_key.is_filterable(), "按 source_key 过滤必须可用");
        assert!(source_key.is_sortable(), "按 source_key 排序必须可用");
        assert!(source_key.is_searchable(), "source_key 应可被关键词检索");
        assert!(!source_key.is_auto_increment(), "source_key 不是自增列");
    }

    #[test]
    fn token_hash_is_secret_and_never_searchable() {
        let definition = definition();
        let token_hash = definition
            .field("token_hash")
            .unwrap_or_else(|| panic!("token_hash 字段必须存在"));
        assert!(token_hash.is_secret(), "token_hash 必须是 secret 字段");
        assert!(
            !token_hash.is_searchable(),
            "secret 字段不得进入关键词检索面"
        );
        assert!(
            !token_hash.is_filterable(),
            "secret 字段不得进入结构化筛选面"
        );
        assert!(!token_hash.is_sortable(), "secret 字段不得进入排序面");
        assert!(token_hash.is_required(), "token_hash 必填");
    }

    #[test]
    fn status_is_required_with_active_default() {
        let definition = definition();
        let status = definition
            .field("status")
            .unwrap_or_else(|| panic!("status 字段必须存在"));
        assert!(status.is_required(), "status 必填");
        assert_eq!(
            status.default_value(),
            Some(&serde_json::json!("active")),
            "status 应默认 active"
        );
    }

    #[test]
    fn encrypt_enabled_defaults_to_false() {
        let definition = definition();
        let flag = definition
            .field("encrypt_enabled")
            .unwrap_or_else(|| panic!("encrypt_enabled 字段必须存在"));
        assert!(flag.is_required());
        assert_eq!(flag.default_value(), Some(&serde_json::json!(false)));
    }

    #[test]
    fn default_locale_defaults_to_zh_cn() {
        let definition = definition();
        let locale = definition
            .field("default_locale")
            .unwrap_or_else(|| panic!("default_locale 字段必须存在"));
        assert_eq!(
            locale.default_value(),
            Some(&serde_json::json!("zh_cn")),
            "缺少默认语言时也要有一种语言，否则控件显示为空"
        );
    }

    #[test]
    fn timestamps_are_managed_by_the_framework() {
        let definition = definition();
        for name in ["created_at", "updated_at"] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(field.is_required(), "{name} 应由框架自动写入");
        }
    }

    #[test]
    fn title_is_sortable_so_the_ledger_can_order_by_name() {
        // 台账视图在上百条量级下必须能按名称排序。DSL 的 sortable 是 fail-closed：
        // 未显式打开时 TableQuery 会直接拒绝排序请求，所以这是一条能力断言而非优化。
        let definition = definition();
        let title = definition
            .field("title")
            .unwrap_or_else(|| panic!("title 字段必须存在"));
        assert!(title.is_sortable(), "按名称排序必须可用");
        assert!(title.is_searchable(), "名称必须可被关键词检索");
    }

    #[test]
    fn status_is_filterable_so_the_ledger_can_filter_by_state() {
        // 台账工具栏的「全部 / 启用 / 已停用」依赖这一位；同样 fail-closed。
        let definition = definition();
        let status = definition
            .field("status")
            .unwrap_or_else(|| panic!("status 字段必须存在"));
        assert!(status.is_filterable(), "按状态筛选必须可用");
    }

    #[test]
    fn updated_at_is_sortable_for_recency_ordering() {
        let definition = definition();
        let updated_at = definition
            .field("updated_at")
            .unwrap_or_else(|| panic!("updated_at 字段必须存在"));
        assert!(updated_at.is_sortable(), "按更新时间排序必须可用");
    }

    #[test]
    fn every_field_has_an_explicit_chinese_label() {
        // 防线：字段没设 .title() 时展示名会退化成字段名（英文），
        // 前端表格就会满屏 source_key / encrypt_enabled。
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

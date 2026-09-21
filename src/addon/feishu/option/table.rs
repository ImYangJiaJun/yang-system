//! `feishu_option` 表声明——Schema 的唯一事实来源。

use yang_base::definition::{Int, Key, Str, Switch, TableSpec, Text, Timestamp};
use yang_base::BaseError;

/// 声明选项表。
///
/// # 两处 DSL 能力缺口带来的写法
///
/// 1. **`option_id` 的全局唯一由唯一索引强制，而不是主键。** 飞书要求选项 id
///    「全局唯一且固定」，但 `fields!` 的 `Key` 硬编码映射到 `Field::id`（自增大整数），
///    DSL 上也没有 `primary_key()`。约束力等价：唯一索引同样不允许重复。
/// 2. **`i18n` / `extra` 落 `Text` 列存放 JSON 文本。** DSL 没有 Json builder
///    （`simple_builder!` 只实例化 9 个 builder，导出列表里也没有 `Json`）。
///    这两列从不被 SQL 查询进内部，只在读写时由 `domain/` 自己 serde 转换。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    Ok(TableSpec::new(yang_base::table!("feishu_option"))
        .title("飞书选项")
        .fields(yang_base::fields! {
            id => Key::new().title("ID"),
            // 飞书契约的选项 id：唯一索引保证全局唯一
            option_id => Str::new()
                .title("选项 ID")
                .require(true)
                .unique(true)
                .max_length(128)
                .searchable(true)
                .filterable(true)
                .sortable(true),
            source_key => Str::new()
                .title("数据源标识")
                .require(true)
                .max_length(64)
                .indexed(true)
                .filterable(true)
                .sortable(true),
            label => Str::new()
                .title("显示文案")
                .require(true)
                .max_length(255)
                .searchable(true),
            // JSON 文本：{"en_us":"…","ja_jp":"…"}
            i18n => Text::new().title("多语言文案"),
            // `filterable` 是 keyset 翻页的**硬依赖**，不是可选的筛选便利：游标条件
            // `sort_order > ? OR (sort_order = ? AND option_id > ?)` 要经
            // `validate_filter_field`，而 DSL 的 `filterable` 是 fail-closed——
            // 只开 `.sortable(true)` 会让第二页起每次都被 FieldPermissionDenied 打回。
            //
            // 副作用（已知并接受）：DSL 没有「只给内部条件用」的窄写法，`.filterable(true)`
            // 会把筛选权限一并置为 `Everyone`，于是持 `feishu.option.read` 的控制台用户
            // 也多出一个按「排序」筛选的入口。收益（翻页可用）远大于这点面宽。
            sort_order => Int::new()
                .title("排序")
                .require(true)
                .default(0)
                .filterable(true)
                .sortable(true),
            is_default => Switch::new().title("默认选项").require(true).default(false),
            // 禁用而非删除，避免历史审批单引用的选项彻底失联
            enabled => Switch::new()
                .title("启用")
                .require(true)
                .default(true)
                .filterable(true),
            // JSON 文本：预留联动筛选键值
            extra => Text::new().title("扩展字段"),
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
        assert_eq!(definition.name(), "feishu_option");
        assert_eq!(definition.primary_key(), "id");
    }

    #[test]
    fn option_id_is_required_searchable_filterable_and_sortable() {
        let definition = definition();
        let option_id = definition
            .field("option_id")
            .unwrap_or_else(|| panic!("option_id 字段必须存在"));
        assert!(option_id.is_required(), "option_id 必填");
        // 飞书的 query 关键词要能命中选项编码
        assert!(option_id.is_searchable());
        assert!(option_id.is_filterable());
        assert!(option_id.is_sortable());
    }

    #[test]
    fn label_is_required_and_searchable() {
        let definition = definition();
        let label = definition
            .field("label")
            .unwrap_or_else(|| panic!("label 字段必须存在"));
        assert!(label.is_required(), "label 必填");
        // 一个可搜索文本字段都没有时 TableQuery::search 会 fail-closed 报错
        assert!(label.is_searchable(), "label 必须可搜索");
    }

    #[test]
    fn source_key_is_indexed_and_filterable() {
        let definition = definition();
        let source_key = definition
            .field("source_key")
            .unwrap_or_else(|| panic!("source_key 字段必须存在"));
        assert!(source_key.is_required());
        assert!(source_key.is_filterable(), "按数据源取选项依赖该位");
        assert!(source_key.is_sortable(), "游标排序依赖该位");
    }

    #[test]
    fn enabled_defaults_to_true_and_is_filterable() {
        let definition = definition();
        let enabled = definition
            .field("enabled")
            .unwrap_or_else(|| panic!("enabled 字段必须存在"));
        assert_eq!(enabled.default_value(), Some(&serde_json::json!(true)));
        assert!(
            enabled.is_filterable(),
            "取选项时要按 enabled 过滤掉被禁用的项"
        );
    }

    #[test]
    fn sort_order_and_is_default_have_stable_defaults() {
        let definition = definition();
        let sort_order = definition
            .field("sort_order")
            .unwrap_or_else(|| panic!("sort_order 字段必须存在"));
        assert_eq!(sort_order.default_value(), Some(&serde_json::json!(0)));
        assert!(sort_order.is_sortable(), "游标排序依赖该位");
        // keyset 翻页的游标条件要经 validate_filter_field，而 DSL 的 filterable 是
        // fail-closed：只开 sortable 会让第二页起被 FieldPermissionDenied 打回。
        assert!(
            sort_order.is_filterable(),
            "游标翻页依赖该位——少了它 `page_token` 从第二页起必然失败"
        );

        let is_default = definition
            .field("is_default")
            .unwrap_or_else(|| panic!("is_default 字段必须存在"));
        assert_eq!(is_default.default_value(), Some(&serde_json::json!(false)));
    }

    #[test]
    fn updated_at_is_sortable_so_the_detail_page_can_order_by_last_push() {
        // 详情页默认按「最近推送」倒序：只有持管理 Token 的多维表格自动化会写选项行，
        // 所以 updated_at 就是那行选项最后一次被推送的时间，是控制台对
        // 「推送还活着吗」唯一诚实的信号。排序位同样是 fail-closed。
        let definition = definition();
        let updated_at = definition
            .field("updated_at")
            .unwrap_or_else(|| panic!("updated_at 字段必须存在"));
        assert!(updated_at.is_sortable(), "按最近推送排序必须可用");
    }

    #[test]
    fn json_payloads_are_plain_text_columns() {
        // fields! DSL 没有 Json builder，JSON 文本落 Text 列——这条断言把这个
        // 已知取舍钉住，避免将来误以为它们是原生 JSON 类型
        let definition = definition();
        for name in ["i18n", "extra"] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(
                !field.is_required(),
                "{name} 可空：没有额外语言或联动键时不必写"
            );
        }
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

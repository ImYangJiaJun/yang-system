//! `feishu_approval_export_field_map` 表声明：回写规则的字段映射。
//!
//! 一行 = 一个来源字段 → 一个目标列。来源可以是明细行子控件、顶层控件、
//! 系统字段或常量；目标列存 field_id，写入时解析成当前字段名（Bitable create
//! 只认字段名，传 field_id 会报 1254045）。

use yang_base::definition::{
    FieldName, FieldRef, Int, Key, Str, Switch, TableName, TableSpec, Text, Timestamp,
};
use yang_base::BaseError;

/// 声明回写字段映射表。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    let table_name = yang_base::table!("feishu_approval_export_field_map");
    Ok(TableSpec::new(table_name.clone())
        .title("飞书审批回写字段映射")
        .fields(yang_base::fields! {
            id => Key::new().title("ID").filterable(true).sortable(true),
            rule_id => Int::new()
                .title("所属规则")
                .require(true)
                .indexed(true)
                .filterable(true),
            // detail / top / system / constant
            source_kind => Str::new()
                .title("来源类型")
                .require(true)
                .max_length(16)
                .filterable(true),
            // detail/top 为控件 id；system/constant 为空串（唯一键列必须 NOT NULL）。
            source_widget_id => Str::new()
                .title("来源控件 ID")
                .require(true)
                .max_length(128),
            // system 的来源键；其余为空串。
            source_system_key => Str::new()
                .title("系统字段键")
                .require(true)
                .max_length(64),
            constant_value => Text::new().title("常量值"),
            target_field_id => Str::new()
                .title("目标字段 ID")
                .require(true)
                .max_length(128),
            target_field_name => Str::new().title("目标字段名").max_length(255),
            // text / number / date / person / option_text / multi_option /
            // checkbox / url / json
            converter => Str::new()
                .title("转换器")
                .require(true)
                .max_length(32)
                .default("text"),
            option_map => Text::new().title("选项映射"),
            required => Switch::new().title("目标必填").require(true).default(false),
            created_at => Timestamp::new().created_at().title("创建时间"),
            updated_at => Timestamp::new().updated_at().title("更新时间"),
        })
        .unique_named(
            "uk_feishu_approval_export_field_map",
            [
                field_ref(&table_name, "rule_id")?,
                field_ref(&table_name, "source_kind")?,
                field_ref(&table_name, "source_widget_id")?,
                field_ref(&table_name, "source_system_key")?,
                field_ref(&table_name, "target_field_id")?,
            ],
        ))
}

fn field_ref(table_name: &TableName, field: &str) -> Result<FieldRef, BaseError> {
    let field = FieldName::new(field).map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(FieldRef::new(table_name.clone(), field))
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
        assert_eq!(definition.name(), "feishu_approval_export_field_map");
        assert_eq!(definition.primary_key(), "id");
    }

    #[test]
    fn rule_and_source_are_indexed_and_filterable() {
        let definition = definition();
        for name in ["rule_id", "source_kind"] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(field.is_required(), "{name} 必填");
            assert!(field.is_filterable(), "{name} 必须可筛选");
        }
        let rule_id = definition
            .field("rule_id")
            .unwrap_or_else(|| panic!("rule_id 字段必须存在"));
        assert!(rule_id.is_filterable());
    }

    #[test]
    fn required_columns_cover_the_unique_key() {
        let definition = definition();
        for name in [
            "rule_id",
            "source_kind",
            "source_widget_id",
            "source_system_key",
            "target_field_id",
            "converter",
        ] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(field.is_required(), "{name} 必填");
        }
    }

    #[test]
    fn converter_defaults_to_text() {
        let definition = definition();
        let converter = definition
            .field("converter")
            .unwrap_or_else(|| panic!("converter 字段必须存在"));
        assert_eq!(converter.default_value(), Some(&serde_json::json!("text")));
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
                "字段 {} 的展示名退化成了字段名",
                field.name()
            );
        }
    }
}

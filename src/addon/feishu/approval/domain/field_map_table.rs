//! `feishu_approval_field_map` 表声明：审批控件 ↔ 多维表格字段的映射。
//!
//! 一条配置对应多行映射。之所以独立成表而不是配置表里的 JSON 列：映射要按
//! `config_id` 反查（处理一条记录时逐控件取源值），JSON 列无法建索引。

use yang_base::definition::{Int, Key, Str, Switch, TableSpec, Text, Timestamp};
use yang_base::BaseError;

/// 声明字段映射表。
///
/// # 不声明外键
///
/// `schema_sync` **只增不删**，而外键规则被框架硬编码为 `RESTRICT` 且不可改
/// （`crates/yang-base/src/table/definition.rs:628-655`）——**外键一旦声明即永久存在**。
/// 这里用 `config_id` 整数列 + 应用层校验，换掉那份不可撤回的耦合。
/// 配置删除时按 `config_id` 显式清理映射行。
///
/// # 用 `field_id` 而不是字段名作映射目标
///
/// 用户在多维表格里改列名会静默失配（表现为「该控件取不到值」，不报错）。
/// `bitable_field_name` 另存一份仅供控制台展示——与 `feishu_datasource_field`
/// 同一条纪律。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    Ok(
        TableSpec::new(yang_base::table!("feishu_approval_field_map"))
            .title("飞书审批字段映射")
            .fields(yang_base::fields! {
                // 同 `task_table`：DSL 的 `sortable`/`filterable` 是 fail-closed，漏了只在运行期报错。
            id => Key::new().title("ID").filterable(true).sortable(true),
                // 指向 `feishu_approval_config.id`。无外键（理由见本文件顶部）。
                //
                // `filterable` 必开：处理一条记录时要按 config_id 取出全部映射，
                // 而 DSL 的 filterable 是 fail-closed。`indexed` 是性能位——
                // 每条记录的处理都会走这个查询。
                config_id => Int::new()
                    .title("所属配置")
                    .require(true)
                    .indexed(true)
                    .filterable(true),
                // --- 审批侧 ---
                //
                // widget_id 来自「查看指定审批定义」返回的 form[].id。
                // 它与 widget_type 都存**快照**：审批定义被改动后，库里的映射可能与
                // 新定义不再对应，所以保存配置时用最新快照复核一遍（设计 §6.1）。
                widget_id => Str::new()
                    .title("审批控件 ID")
                    .require(true)
                    .max_length(128),
                widget_type => Str::new()
                    .title("审批控件类型")
                    .require(true)
                    .max_length(32),
                required => Switch::new().title("必填").require(true).default(false),
                // --- 多维表格侧 ---
                bitable_field => Str::new()
                    .title("多维表格字段 ID")
                    .require(true)
                    .max_length(128),
                // 仅供控制台展示；对齐靠 `bitable_field`（字段 id）。
                bitable_field_name => Str::new().title("多维表格字段名").max_length(255),
                // --- 值转换 ---
                //
                // 三种：`direct`（文本/数字类直取）、`date`（毫秒时间戳 → RFC3339）、
                // `option`（多维表格选项文案 → 审批控件选项 value）。见设计 §6.2。
                converter => Str::new()
                    .title("转换器")
                    .require(true)
                    .max_length(32)
                    .default("direct"),
                // 单选/多选专用：多维表格选项文案 ↔ 审批控件选项 value。
                // 两者不一定同名（例如飞书侧的 value 是数字 id），所以必须显式配。
                // JSON 文本：{"<多维表格文案>":"<审批控件 value>"}。
                // DSL 没有 Json builder，落 Text 列。
                external_binding => Text::new().title("外部选项绑定"),
                option_map => Text::new().title("选项映射"),
                created_at => Timestamp::new().created_at().title("创建时间"),
                updated_at => Timestamp::new().updated_at().title("更新时间"),
            }),
    )
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
        assert_eq!(definition.name(), "feishu_approval_field_map");
        assert_eq!(definition.primary_key(), "id");
    }

    #[test]
    fn config_id_is_indexed_and_filterable() {
        // 两者都不可省：indexed 是性能（每条记录处理都查），filterable 是正确性
        // （DSL 的 filterable 是 fail-closed，漏了在运行期报 FieldPermissionDenied）。
        let definition = definition();
        let config_id = definition
            .field("config_id")
            .unwrap_or_else(|| panic!("config_id 字段必须存在"));
        assert!(config_id.is_required());
        assert!(config_id.is_filterable(), "按配置取映射依赖该位");
    }

    #[test]
    fn widget_and_field_coordinates_are_required() {
        let definition = definition();
        for name in ["widget_id", "widget_type", "bitable_field", "converter"] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(field.is_required(), "{name} 必填");
        }
    }

    #[test]
    fn converter_defaults_to_direct() {
        // 大多数控件是文本/数字类直取，默认值让最常见的映射少填一次。
        let definition = definition();
        let converter = definition
            .field("converter")
            .unwrap_or_else(|| panic!("converter 字段必须存在"));
        assert_eq!(
            converter.default_value(),
            Some(&serde_json::json!("direct"))
        );
    }

    #[test]
    fn required_defaults_to_false() {
        // 映射表里的 `required` 是快照，默认 false 表示「未标记」；
        // 真正的必填判据以保存时的最新快照为准，不靠这一列的默认值。
        let definition = definition();
        let required = definition
            .field("required")
            .unwrap_or_else(|| panic!("required 字段必须存在"));
        assert_eq!(required.default_value(), Some(&serde_json::json!(false)));
    }

    #[test]
    fn optional_columns_are_nullable() {
        // `bitable_field_name` 是展示用的补充信息；`option_map` 只对单选/多选有意义。
        let definition = definition();
        for name in ["bitable_field_name", "option_map"] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(!field.is_required(), "{name} 可空");
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

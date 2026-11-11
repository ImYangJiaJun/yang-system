//! `feishu_approval_export_rule` 表声明：审批 → 多维表格回写规则。
//!
//! 一条规则 = 一个审批定义 + 一组触发节点 + 一组条件 + 一个目标 Base/表 +
//! 一组字段映射。与审批派发的配置表同构：条件/映射只存坐标与 JSON 文本，
//! 不存任何审批表单值。

use yang_base::definition::{
    FieldName, FieldRef, Key, Str, Switch, TableName, TableSpec, Text, Timestamp,
};
use yang_base::BaseError;

/// 声明回写规则表。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    let table_name = yang_base::table!("feishu_approval_export_rule");
    Ok(TableSpec::new(table_name.clone())
        .title("飞书审批回写规则")
        .fields(yang_base::fields! {
            id => Key::new().title("ID").filterable(true).sortable(true),
            title => Str::new()
                .title("规则名")
                .require(true)
                .max_length(100)
                .searchable(true)
                .sortable(true),
            approval_code => Str::new()
                .title("审批定义 Code")
                .require(true)
                .max_length(128)
                .searchable(true)
                .filterable(true),
            approval_id => Str::new().title("审批定义 ID").max_length(64),
            approval_name => Str::new().title("审批名称").max_length(512).searchable(true),
            //`node_passed` / `final_approved`。
            trigger_mode => Str::new()
                .title("触发模式")
                .require(true)
                .max_length(32)
                .default("node_passed")
                .filterable(true),
            //JSON 数组：一个或多个 node_id。
            trigger_node_ids => Text::new().title("触发节点 ID 列表"),
            //节点通过后是否还要求整个实例最终 APPROVED。
            require_final_approval => Switch::new()
                .title("要求最终通过")
                .require(true)
                .default(true),
            //明细控件 id（行级回写的来源）。
            detail_widget_id => Str::new()
                .title("明细控件 ID")
                .require(true)
                .max_length(128),
            //`matched_rows` / `all_rows`。
            row_filter => Str::new()
                .title("行筛选")
                .require(true)
                .max_length(32)
                .default("matched_rows")
                .filterable(true),
            condition_json => Text::new().title("条件 JSON"),
            target_base_token => Str::new()
                .title("目标 Base Token")
                .require(true)
                .max_length(128)
                .filterable(true),
            target_table_id => Str::new()
                .title("目标数据表 ID")
                .require(true)
                .max_length(128)
                .filterable(true),
            target_base_name => Str::new().title("目标多维表名称").max_length(512).searchable(true),
            target_table_name => Str::new()
                .title("目标数据表名称")
                .max_length(512)
                .searchable(true),
            //目标状态列（标记用）。`None` = 不标记（与 `mark_enabled=false` 同义）。
            status_field => Str::new().title("状态列 ID").max_length(128),
            status_field_name => Str::new().title("状态列名").max_length(255),
            //JSON：有效/已退回/已拒绝/已撤回/已撤销/已删除 的文案。
            status_labels_json => Text::new().title("状态文案 JSON"),
            //JSON：审批评论配置（字段、触发值、状态列、作者、评论模板）。
            //可空 = 该规则不启用「评论审批实例」能力。
            comment_config_json => Text::new().title("审批评论配置 JSON"),
            mark_enabled => Switch::new()
                .title("启用标记")
                .require(true)
                .default(true),
            enabled => Switch::new()
                .title("启用")
                .require(true)
                .default(true)
                .filterable(true),
            //审批定义 form 原文快照（数组形态）。
            definition_snapshot => Text::new().title("定义快照"),
            //审批定义 node_list 原文快照。
            node_snapshot => Text::new().title("节点快照"),
            snapshot_at => Timestamp::new().title("快照时间"),
            definition_hash => Str::new().title("定义哈希").max_length(64),
            //规则生效时间；只处理此时间之后创建的实例。不做历史补录。
            effective_from => Timestamp::new().title("生效时间").filterable(true),
            created_at => Timestamp::new().created_at().title("创建时间"),
            updated_at => Timestamp::new().updated_at().title("更新时间").sortable(true),
        })
        .unique_named(
            "uk_feishu_approval_export_rule_title",
            [
                field_ref(&table_name, "approval_code")?,
                field_ref(&table_name, "title")?,
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
        assert_eq!(definition.name(), "feishu_approval_export_rule");
        assert_eq!(definition.primary_key(), "id");
    }

    #[test]
    fn required_coordinates_and_trigger_are_declared() {
        let definition = definition();
        for name in [
            "title",
            "approval_code",
            "trigger_mode",
            "detail_widget_id",
            "row_filter",
            "target_base_token",
            "target_table_id",
        ] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(field.is_required(), "{name} 必填");
        }
    }

    #[test]
    fn filterable_columns_cover_worker_queries() {
        let definition = definition();
        for name in [
            "approval_code",
            "enabled",
            "target_base_token",
            "target_table_id",
        ] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(field.is_filterable(), "{name} 必须可筛选");
        }
    }

    #[test]
    fn defaults_match_the_v1_decisions() {
        let definition = definition();
        let expected = [
            ("trigger_mode", serde_json::json!("node_passed")),
            ("row_filter", serde_json::json!("matched_rows")),
            ("require_final_approval", serde_json::json!(true)),
            ("mark_enabled", serde_json::json!(true)),
            ("enabled", serde_json::json!(true)),
        ];
        for (name, value) in expected {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert_eq!(field.default_value(), Some(&value), "{name} 默认值");
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
                "字段 {} 的展示名退化成了字段名",
                field.name()
            );
        }
    }
}

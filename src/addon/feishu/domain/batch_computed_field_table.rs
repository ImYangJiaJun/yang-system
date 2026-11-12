//! `feishu_batch_computed_field`：批量审批列表里的只读计算字段配置。

use yang_base::definition::{
    FieldName, FieldRef, Int, Key, Str, Switch, TableName, TableSpec, Timestamp,
};
use yang_base::BaseError;

pub(crate) const COMPUTED_APPROVAL_CODE: &str = "approval_code";
pub(crate) const COMPUTED_NAME: &str = "name";

pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    let fields = yang_base::fields! {
        id => Key::new().title("ID").filterable(true).sortable(true),
        approval_code => Str::new()
            .title("审批定义 Code")
            .require(true)
            .max_length(128)
            .filterable(true)
            .searchable(true),
        name => Str::new()
            .title("字段名")
            .require(true)
            .max_length(64)
            .filterable(true)
            .sortable(true),
        left_field_id => Str::new()
            .title("左操作数控件 ID")
            .require(true)
            .max_length(128),
        left_field_name => Str::new().title("左操作数名称").max_length(128),
        operator => Str::new()
            .title("运算符")
            .require(true)
            .max_length(16)
            .filterable(true),
        right_field_id => Str::new()
            .title("右操作数控件 ID")
            .require(true)
            .max_length(128),
        right_field_name => Str::new().title("右操作数名称").max_length(128),
        precision => Int::new().title("小数位").require(true).default(2_i64),
        enabled => Switch::new().title("启用").require(true).default(true).filterable(true),
        sort_order => Int::new().title("排序").require(true).default(0_i64).sortable(true),
        created_at => Timestamp::new().title("创建时间").created_at(),
        updated_at => Timestamp::new().title("更新时间").updated_at().sortable(true),
    };
    let table_name = TableName::new("feishu_batch_computed_field")
        .map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(TableSpec::new(table_name.clone())
        .title("飞书批量审批计算字段")
        .fields(fields)
        .unique_named(
            "uk_feishu_batch_computed_field_name",
            [
                field_ref(&table_name, COMPUTED_APPROVAL_CODE)?,
                field_ref(&table_name, COMPUTED_NAME)?,
            ],
        )
        .check_named(
            "chk_feishu_batch_computed_field_operator",
            "`operator` IN ('add', 'subtract', 'multiply', 'divide')",
        )
        .check_named(
            "chk_feishu_batch_computed_field_precision",
            "`precision` >= 0 AND `precision` <= 6",
        ))
}

fn field_ref(table_name: &TableName, field: &str) -> Result<FieldRef, BaseError> {
    let field = FieldName::new(field).map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(FieldRef::new(table_name.clone(), field))
}

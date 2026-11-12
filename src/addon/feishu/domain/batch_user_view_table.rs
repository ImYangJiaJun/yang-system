//! `feishu_batch_user_view`：批量审批中每个用户按审批定义保存的个人视图。
//!
//! 计算字段、显示字段与顺序都只属于创建它的用户；表上所有读写都必须带
//! `user_id` 条件，禁止跨用户读取或覆盖。

use yang_base::definition::{
    FieldName, FieldRef, Int, Key, Str, TableName, TableSpec, Text, Timestamp,
};
use yang_base::BaseError;

pub(crate) const VIEW_USER_ID: &str = "user_id";
pub(crate) const VIEW_APPROVAL_CODE: &str = "approval_code";
pub(crate) const VIEW_COMPUTED_FIELDS_JSON: &str = "computed_fields_json";
pub(crate) const VIEW_COLUMNS_JSON: &str = "columns_json";

pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    let fields = yang_base::fields! {
        id => Key::new().title("ID").filterable(true).sortable(true),
        user_id => Int::new()
            .title("系统用户 ID")
            .require(true)
            .filterable(true),
        approval_code => Str::new()
            .title("审批定义 Code")
            .require(true)
            .max_length(128)
            .filterable(true)
            .searchable(true),
        computed_fields_json => Text::new()
            .title("计算字段规则 JSON")
            .require(true),
        columns_json => Text::new()
            .title("显示字段与顺序 JSON")
            .require(true),
        created_at => Timestamp::new().title("创建时间").created_at(),
        updated_at => Timestamp::new().title("更新时间").updated_at().sortable(true),
    };
    let table_name = TableName::new("feishu_batch_user_view")
        .map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(TableSpec::new(table_name.clone())
        .title("飞书批量审批个人视图")
        .fields(fields)
        .unique_named(
            "uk_feishu_batch_user_view_user_approval",
            [
                field_ref(&table_name, VIEW_USER_ID)?,
                field_ref(&table_name, VIEW_APPROVAL_CODE)?,
            ],
        ))
}

fn field_ref(table_name: &TableName, field: &str) -> Result<FieldRef, BaseError> {
    let field = FieldName::new(field).map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(FieldRef::new(table_name.clone(), field))
}

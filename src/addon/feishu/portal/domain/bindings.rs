//! 门户与数据源字段绑定之间的共享校验。
//!
//! `create_portal` 与 `update_portal` 都要回答同一个问题：「这几个 `field_id`
//! 确实是这张数据源已绑定的字段吗」。两边各写一遍必然会漂移——漂移的失效形态是
//! 「向导建得出来、编辑保存不了」，而这类不一致在单测里全绿、只在界面上暴露。
//!
//! 放在 `actions/` 之外：架构门禁要求 `actions/` 下每个文件恰好一个 `handle` +
//! 一个 `register`，共享校验函数没有位置。

use std::collections::HashMap;

use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;

/// 校验数据源存在且处于 `active`。
pub(crate) async fn ensure_datasource_is_active(
    context: &FeishuContext,
    datasource_id: i64,
) -> Result<(), BaseError> {
    let datasource = context
        .datasources()
        .query()
        .select_fields(&["status"])?
        .where_eq("id", serde_json::json!(datasource_id))?
        .optional()
        .await?;
    let Some(datasource) = datasource else {
        return Err(BaseError::ParamInvalid(
            "datasource_id".to_string(),
            "数据源不存在".to_string(),
        ));
    };
    let status: String = datasource
        .optional("status")?
        .unwrap_or_else(|| "active".to_string());
    if status != "active" {
        return Err(BaseError::ParamInvalid(
            "datasource_id".to_string(),
            "数据源未启用，不能挂查询门户".to_string(),
        ));
    }
    Ok(())
}

/// 取出指定 `field_id` 在这张数据源下的选项 `source_key`（只含存在的那几个）。
///
/// 返回 `field_id → source_key`：门户要拿 `source_key` 去 `feishu_option` 里取数，
/// 而 `field_id` 是管理员在界面里勾选时用的身份（改名不断链）。
pub(crate) async fn load_bound_fields(
    context: &FeishuContext,
    datasource_id: i64,
    field_ids: &[String],
) -> Result<HashMap<String, String>, BaseError> {
    if field_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let values = field_ids
        .iter()
        .map(|field_id| serde_json::json!(field_id))
        .collect::<Vec<_>>();
    let rows = context
        .datasource_fields()
        .query()
        .select_fields(&["field_id", "source_key"])?
        .where_eq("datasource_id", serde_json::json!(datasource_id))?
        .where_in("field_id", values)?
        .all()
        .await?;

    let mut bindings = HashMap::with_capacity(rows.len());
    for row in rows {
        bindings.insert(row.require("field_id")?, row.require("source_key")?);
    }
    Ok(bindings)
}

/// 要求 `requested` 里的每个 `field_id` 都在 `bindings` 中；报错时点名缺的那几个。
pub(crate) fn require_all_bound(
    bindings: &HashMap<String, String>,
    requested: &[String],
    field: &str,
) -> Result<(), BaseError> {
    let missing = requested
        .iter()
        .filter(|field_id| !bindings.contains_key(*field_id))
        .cloned()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(BaseError::ParamInvalid(
        field.to_string(),
        format!("以下字段不是该数据源的已绑定字段：{}", missing.join("、")),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_requested_field_must_be_bound() {
        let bindings = HashMap::from([("fldA".to_string(), "key_a".to_string())]);
        assert!(require_all_bound(&bindings, &["fldA".to_string()], "exposed_columns").is_ok());

        let error = require_all_bound(
            &bindings,
            &["fldA".to_string(), "fldB".to_string()],
            "exposed_columns",
        )
        .err()
        .unwrap_or_else(|| panic!("未绑定的字段必须被拒"));
        // 报错要点名缺的那一个，而不是「有字段没绑定」——否则管理员无从下手。
        assert!(matches!(
            error,
            BaseError::ParamInvalid(field, message)
                if field == "exposed_columns" && message.contains("fldB") && !message.contains("fldA")
        ));
    }

    #[test]
    fn an_empty_request_is_trivially_bound() {
        assert!(require_all_bound(&HashMap::new(), &[], "exposed_columns").is_ok());
    }
}

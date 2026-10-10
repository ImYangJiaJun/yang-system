//! 外部选项只存绑定；按当前记录的选择从根到叶解析。
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use yang_base::{
    table::{Record, WhereCondition},
    BaseError,
};

use super::approval_convert::{is_empty, labels_from_cell, WidgetMap};
use super::approval_match::{Column, FormWidget};
use super::approval_provision::ProvisionError;
use super::context::FeishuContext;
use super::repository::all_pages;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct BindingLevel {
    pub(crate) binding_id: i64,
    pub(crate) source_key: String,
    pub(crate) field_id: String,
    pub(crate) bitable_field: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalBinding {
    pub(crate) datasource_id: i64,
    pub(crate) levels: Vec<BindingLevel>,
}

fn invalid(message: impl Into<String>) -> ProvisionError {
    ProvisionError::Invalid(vec![message.into()])
}

pub(crate) async fn load_bindings(
    context: &FeishuContext,
    form: &[FormWidget],
    columns: &[Column],
) -> Result<BTreeMap<String, ExternalBinding>, ProvisionError> {
    let names: BTreeSet<String> = form
        .iter()
        .flat_map(|w| w.walk())
        .filter(|w| w.links_to_our_options())
        .map(|w| w.name.trim().to_string())
        .collect();
    if names.is_empty() {
        return Ok(BTreeMap::new());
    }
    // shortcut: 字段绑定最多读 20 页，绑定元数据超限时升级此读取方式。
    let rows = all_pages(
        context.datasource_fields().query().select_fields(&[
            "id",
            "datasource_id",
            "field_id",
            "field_name",
            "source_key",
            "parent_field_id",
            "enabled",
        ])?,
        20,
    )
    .await?;
    let bindings = bindings_from_rows(&rows, &names, columns)?;
    for binding in bindings.values() {
        let source = context
            .datasources()
            .query()
            .where_primary_key_eq(json!(binding.datasource_id))?
            .optional()
            .await?;
        if !source
            .as_ref()
            .map(|r| r.optional::<String>("status"))
            .transpose()?
            .flatten()
            .is_some_and(|s| s == "active")
        {
            return Err(invalid("外部选项所属数据源不存在或已停用"));
        }
    }
    Ok(bindings)
}

pub(super) fn bindings_from_rows(
    rows: &[Record],
    names: &BTreeSet<String>,
    columns: &[Column],
) -> Result<BTreeMap<String, ExternalBinding>, ProvisionError> {
    for row in rows {
        if row.get("enabled").and_then(Value::as_bool) == Some(true)
            && binding_display_name(row)?.is_none()
        {
            tracing::warn!(source_key = ?row.get("source_key"), "跳过没有列名的外部选项绑定");
        }
    }
    let mut result = BTreeMap::new();
    for name in names {
        let matches = rows
            .iter()
            .filter(|r| {
                r.get("enabled").and_then(Value::as_bool) == Some(true)
                    && r.get("field_name").and_then(Value::as_str).map(str::trim)
                        == Some(name.as_str())
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(invalid(format!("控件「{name}」的外部选项绑定缺失或有歧义")));
        }
        let datasource_id: i64 = matches[0].require("datasource_id")?;
        let mut current = matches[0];
        let mut seen = BTreeSet::new();
        let mut levels = Vec::new();
        loop {
            let binding_id: i64 = current.require("id")?;
            if !seen.insert(binding_id) {
                return Err(invalid("外部选项父链成环"));
            }
            if !current.optional::<bool>("enabled")?.unwrap_or(false) {
                return Err(invalid("外部选项父绑定已停用"));
            }
            let field_name: String = current.require("field_name")?;
            let columns = columns
                .iter()
                .filter(|c| c.field_name.trim() == field_name.trim())
                .collect::<Vec<_>>();
            if columns.len() != 1 {
                return Err(invalid(format!(
                    "外部选项父链的列「{field_name}」在派发表中缺失或重名"
                )));
            }
            levels.push(BindingLevel {
                binding_id,
                source_key: current.require("source_key")?,
                field_id: current.require("field_id")?,
                bitable_field: columns[0].field_id.clone(),
            });
            let parent: Option<String> = current.optional("parent_field_id")?;
            let Some(parent) = parent.filter(|s| !s.trim().is_empty()) else {
                break;
            };
            let parents = rows
                .iter()
                .filter(|r| {
                    r.get("datasource_id").and_then(Value::as_i64) == Some(datasource_id)
                        && r.get("field_id").and_then(Value::as_str) == Some(parent.as_str())
                })
                .collect::<Vec<_>>();
            if parents.len() != 1 {
                return Err(invalid("外部选项父绑定缺失或有歧义"));
            }
            current = parents[0];
        }
        levels.reverse();
        result.insert(
            name.clone(),
            ExternalBinding {
                datasource_id,
                levels,
            },
        );
    }
    Ok(result)
}

pub(crate) fn binding_display_name(binding: &Record) -> Result<Option<String>, BaseError> {
    Ok(binding
        .optional::<String>("field_name")?
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty()))
}

/// 数据问题不发起审批；数据库错误由调用方保留为可重试。
pub(crate) async fn resolve_options(
    context: &FeishuContext,
    widgets: &mut [WidgetMap],
    cells: &Map<String, Value>,
) -> Result<(), BaseError> {
    for widget in widgets {
        let Some(binding) = &widget.external_binding else {
            continue;
        };
        let cell = cells.get(&widget.bitable_field).unwrap_or(&Value::Null);
        if is_empty(cell) {
            continue;
        }
        if binding.levels.is_empty()
            || binding.levels.last().map(|l| &l.bitable_field) != Some(&widget.bitable_field)
        {
            return Err(data_error("外部选项绑定快照损坏"));
        }
        let source = context
            .datasources()
            .query()
            .where_primary_key_eq(json!(binding.datasource_id))?
            .optional()
            .await?;
        if !source
            .as_ref()
            .map(|r| r.optional::<String>("status"))
            .transpose()?
            .flatten()
            .is_some_and(|s| s == "active")
        {
            return Err(data_error("外部选项所属数据源不存在或已停用"));
        }
        let mut parent_key = None;
        let mut parent_field: Option<&str> = None;
        let mut seen = BTreeSet::new();
        for (index, level) in binding.levels.iter().enumerate() {
            if !seen.insert(level.binding_id) {
                return Err(data_error("外部选项父链成环"));
            }
            let row = context
                .datasource_fields()
                .query()
                .where_primary_key_eq(json!(level.binding_id))?
                .optional()
                .await?
                .ok_or_else(|| data_error("外部选项绑定已删除"))?;
            let parent: Option<String> = row.optional("parent_field_id")?;
            if row.require::<i64>("datasource_id")? != binding.datasource_id
                || row.require::<String>("source_key")? != level.source_key
                || row.require::<String>("field_id")? != level.field_id
                || !row.require::<bool>("enabled")?
                || parent.as_deref().filter(|s| !s.trim().is_empty()) != parent_field
            {
                return Err(data_error(
                    "外部选项绑定已停用、归属或父链已改变，请重建配置",
                ));
            }
            let cell = cells.get(&level.bitable_field).unwrap_or(&Value::Null);
            let labels = labels_from_cell(cell);
            let leaf = index + 1 == binding.levels.len();
            if labels.is_empty() || (!leaf && labels.len() != 1) {
                return Err(data_error("外部选项父级必须选择唯一文案"));
            }
            for label in labels {
                let id = selected_option(context, &level.source_key, &label, parent_key.as_deref())
                    .await?;
                if leaf {
                    widget.option_map.insert(label, id);
                } else {
                    parent_key = Some(id);
                }
            }
            parent_field = Some(&level.field_id);
        }
    }
    Ok(())
}

fn data_error(message: impl Into<String>) -> BaseError {
    BaseError::ParamInvalid("external_binding".to_string(), message.into())
}

async fn selected_option(
    context: &FeishuContext,
    source: &str,
    label: &str,
    parent: Option<&str>,
) -> Result<String, BaseError> {
    let query = context
        .options()
        .query()
        .select_fields(&["option_id", "label"])?
        .where_eq("source_key", json!(source))?
        .where_eq("enabled", json!(true))?
        .where_eq("label", json!(label))?;
    let query = match parent {
        Some(parent) => query.where_eq("parent_key", json!(parent))?,
        None => query.where_or(vec![
            WhereCondition::IsNull {
                field: "parent_key".into(),
            },
            WhereCondition::Eq {
                field: "parent_key".into(),
                value: json!(""),
            },
        ])?,
    };
    // MySQL 排序规则不区分大小写/重音；不能从这些近似命中中任取一行。
    let rows = query.page(1, 100)?.all().await?;
    if rows.len() == 100 {
        return Err(data_error("选项文案存在过多近似命中，请清理数据"));
    }
    exact_option(&rows, label)
}

pub(super) fn exact_option(rows: &[Record], label: &str) -> Result<String, BaseError> {
    let matches = rows
        .iter()
        .filter(|r| r.get("label").and_then(Value::as_str) == Some(label))
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(data_error(format!(
            "选项「{label}」不存在、已停用或在当前父级下有歧义"
        )));
    }
    matches[0].require("option_id")
}

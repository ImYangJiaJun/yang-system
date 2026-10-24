//! 字段绑定的维护机制：加载、远端校验、source_key 派生、子树影响面、门户引用同步。
//!
//! 这一层只做**共享的机制**；三个写入口（改绑 / 停用启用 / 新增）各自在
//! `datasource/actions/` 里，按「一个 Action 一个文件」的仓库约束落地。
//!
//! # 身份约束
//!
//! 维护只能动 `field_id` / `field_name` / `parent_field_id` / `enabled`。
//! `source_key`、`token_hash`、`token_cipher`、绑定行 id、数据源 id 一律不碰：
//! 前两者是飞书控件 URL 与凭据，改它们等于让已配好的控件失效。

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use yang_base::table::Record;
use yang_base::BaseError;
use yang_db::Transaction;

use super::bitable::{
    is_option_source_field_type, is_single_value_field_type, list_all_fields, BitableCoordinates,
    FieldItem,
};
use super::context::FeishuContext;
use super::outbound::{OutboundTransport, Sleeper};
use super::source_key::valid_source_key;
use super::tenant_token::TenantTokenProvider;

/// 一条绑定在维护视角下的形状（比拉取多 `enabled`，名字是缓存）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BindingRow {
    pub(crate) id: i64,
    pub(crate) field_id: String,
    pub(crate) field_name: Option<String>,
    pub(crate) source_key: String,
    pub(crate) parent_field_id: Option<String>,
    pub(crate) enabled: bool,
}

impl BindingRow {
    /// 业务人员看的名字：缓存优先，没有名字才退回字段 ID。
    pub(crate) fn display_name(&self) -> &str {
        self.field_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.field_id)
    }
}

/// 一条可维护的表级数据源。
#[derive(Debug, Clone)]
pub(crate) struct MaintainableDatasource {
    pub(crate) ingest_mode: String,
    pub(crate) coordinates: Option<BitableCoordinates>,
}

/// 读取表级数据源；不存在时明确 404，而不是「更新了 0 行」的静默成功。
pub(crate) async fn load_datasource(
    context: &FeishuContext,
    datasource_id: i64,
) -> Result<MaintainableDatasource, BaseError> {
    let row = context
        .datasources()
        .query()
        .select_fields(&[
            "ingest_mode",
            "bitable_base_token",
            "bitable_table_id",
            "bitable_view_id",
        ])?
        .where_eq("id", serde_json::json!(datasource_id))?
        .optional()
        .await?
        .ok_or_else(|| BaseError::RecordNotFound("数据源不存在".to_string()))?;

    let base_token = trimmed_owned(row.optional::<String>("bitable_base_token")?);
    let table_id = trimmed_owned(row.optional::<String>("bitable_table_id")?);
    let coordinates = match (base_token, table_id) {
        (Some(app_token), Some(table_id)) => Some(BitableCoordinates {
            app_token,
            table_id,
            view_id: trimmed_owned(row.optional::<String>("bitable_view_id")?),
        }),
        _ => None,
    };

    Ok(MaintainableDatasource {
        ingest_mode: row
            .optional::<String>("ingest_mode")?
            .unwrap_or_else(|| "push".to_string()),
        coordinates,
    })
}

/// 字段维护只对「定时拉取」源成立：push / xlsx 没有可刷新的远端字段目录。
pub(crate) fn require_pull_coordinates(
    datasource: &MaintainableDatasource,
) -> Result<BitableCoordinates, BaseError> {
    if datasource.ingest_mode != "pull" {
        return Err(BaseError::ParamInvalid(
            "datasource_id".to_string(),
            "字段维护只支持「定时拉取」数据源；手工推送与文件导入的绑定语义不在本期范围"
                .to_string(),
        ));
    }
    datasource.coordinates.clone().ok_or_else(|| {
        BaseError::ParamInvalid(
            "datasource_id".to_string(),
            "数据源缺少 Base Token 或数据表 ID，无法读取多维表格字段".to_string(),
        )
    })
}

/// 读取远端字段目录；失败按运维可读的 `ParamInvalid` 返回。
pub(crate) async fn load_remote_fields(
    transport: &dyn OutboundTransport,
    sleeper: &dyn Sleeper,
    tokens: &TenantTokenProvider,
    coordinates: &BitableCoordinates,
) -> Result<Vec<FieldItem>, BaseError> {
    list_all_fields(transport, sleeper, tokens, coordinates)
        .await
        .map_err(super::outbound_setup::outbound_error)
}

/// 读取该数据源的全部绑定（含停用），按主键排序保证展示稳定。
pub(crate) async fn load_bindings(
    context: &FeishuContext,
    datasource_id: i64,
) -> Result<Vec<BindingRow>, BaseError> {
    let rows = context
        .datasource_fields()
        .query()
        .select_fields(&[
            "id",
            "field_id",
            "field_name",
            "source_key",
            "parent_field_id",
            "enabled",
        ])?
        .where_eq("datasource_id", serde_json::json!(datasource_id))?
        .all()
        .await?;

    let mut bindings = rows
        .iter()
        .map(|row| {
            Ok(BindingRow {
                id: row.require("id")?,
                field_id: row.require("field_id")?,
                field_name: row.optional("field_name")?,
                source_key: row.require("source_key")?,
                parent_field_id: row.optional("parent_field_id")?,
                enabled: row.optional("enabled")?.unwrap_or(true),
            })
        })
        .collect::<Result<Vec<_>, BaseError>>()?;
    bindings.sort_by_key(|binding| binding.id);
    Ok(bindings)
}

/// 直接引用 `field_id` 作为父字段的绑定（含停用）。
///
/// 改绑/停用父字段前必须先把它们改指或解除父级：否则父的 `field_id` 一变，
/// 子绑定的父指针就指向一个不存在的字段，下一轮拉取会整棵子树暂停。
pub(crate) fn children_of<'a>(bindings: &'a [BindingRow], field_id: &str) -> Vec<&'a BindingRow> {
    bindings
        .iter()
        .filter(|binding| trimmed(binding.parent_field_id.as_deref()) == Some(field_id))
        .collect()
}

/// 校验远端字段可以作为选项来源，并返回它的元数据。
///
/// `ignore_source_key`：改绑时忽略目标绑定自身，避免「目标字段已被自己绑定」的假冲突。
pub(crate) fn validate_target_field(
    fields: &[FieldItem],
    field_id: &str,
    bindings: &[BindingRow],
    ignore_source_key: Option<&str>,
) -> Result<FieldItem, BaseError> {
    let field_id = field_id.trim();
    if field_id.is_empty() {
        return Err(BaseError::ParamInvalid(
            "field_id".to_string(),
            "字段 ID 不能为空".to_string(),
        ));
    }
    let field = fields
        .iter()
        .find(|field| field.field_id == field_id)
        .ok_or_else(|| {
            BaseError::ParamInvalid(
                "field_id".to_string(),
                "多维表格里找不到这个字段（可能已被删除）".to_string(),
            )
        })?;
    let name = field.field_name.trim();
    if name.is_empty() {
        return Err(BaseError::ParamInvalid(
            "field_id".to_string(),
            "该字段的字段名为空，不能作为选项来源".to_string(),
        ));
    }
    let same_name = fields
        .iter()
        .filter(|other| other.field_name.trim() == name)
        .count();
    if same_name > 1 {
        return Err(BaseError::ParamInvalid(
            "field_id".to_string(),
            format!(
                "字段名「{name}」在表内不唯一（{same_name} 列同名），按名字取数会取到不确定的列"
            ),
        ));
    }
    if let Some(field_type) = field.field_type {
        if !is_option_source_field_type(field_type) {
            let label = field
                .ui_type
                .as_deref()
                .map(str::trim)
                .filter(|label| !label.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("type={field_type}"));
            return Err(BaseError::ParamInvalid(
                "field_id".to_string(),
                format!(
                    "字段「{name}」的类型是 {label}，不能作为选项来源；\
                     取数列必须指向文本、数字、单选、多选、日期、电话、超链接或公式列"
                ),
            ));
        }
    }
    if let Some(other) = bindings.iter().find(|binding| {
        binding.field_id == field_id && Some(binding.source_key.as_str()) != ignore_source_key
    }) {
        return Err(BaseError::ParamInvalid(
            "field_id".to_string(),
            format!(
                "字段「{name}」已被数据源标识 {} 绑定，同一字段不能绑定两次",
                other.source_key
            ),
        ));
    }
    Ok(field.clone())
}

/// 校验一条待写入的父指针（父必须存在且启用；自指直接拒绝）。
pub(crate) fn validate_parent_for_change(
    bindings: &[BindingRow],
    target_field_id: &str,
    parent_field_id: Option<&str>,
) -> Result<(), BaseError> {
    let Some(parent) = trimmed(parent_field_id) else {
        return Ok(());
    };
    if parent == target_field_id {
        return Err(BaseError::ParamInvalid(
            "parent_field_id".to_string(),
            "不能把自己设为父字段".to_string(),
        ));
    }
    let Some(parent_binding) = bindings.iter().find(|binding| binding.field_id == parent) else {
        return Err(BaseError::ParamInvalid(
            "parent_field_id".to_string(),
            format!("父字段 {parent} 不在本数据源的绑定里"),
        ));
    };
    if !parent_binding.enabled {
        return Err(BaseError::ParamInvalid(
            "parent_field_id".to_string(),
            format!(
                "父字段「{}」已停用；请先启用父字段，或改指到其他父字段",
                parent_binding.display_name()
            ),
        ));
    }
    Ok(())
}

/// 校验一条父链上的**每一级**远端字段仍然可用。
///
/// [`validate_parent_for_change`] 只检查直接父绑定存在且启用；这里继续沿祖先链
/// 上溯，确保每一级字段都没被删、没重名、类型可取值。否则保存会成功，下一轮拉取
/// 却把整棵子树跳过——问题被留给 worker 才发现。
pub(crate) fn validate_ancestor_fields(
    fields: &[FieldItem],
    bindings: &[BindingRow],
    parent_field_id: Option<&str>,
) -> Result<(), BaseError> {
    let mut visited: HashSet<&str> = HashSet::new();
    let mut cursor = trimmed(parent_field_id);
    while let Some(parent) = cursor {
        if !visited.insert(parent) {
            return Err(BaseError::ParamInvalid(
                "parent_field_id".to_string(),
                "父链成环".to_string(),
            ));
        }
        let Some(binding) = bindings.iter().find(|binding| binding.field_id == parent) else {
            return Err(BaseError::ParamInvalid(
                "parent_field_id".to_string(),
                format!("父字段 {parent} 不在本数据源的绑定里"),
            ));
        };
        if !binding.enabled {
            return Err(BaseError::ParamInvalid(
                "parent_field_id".to_string(),
                format!(
                    "祖先字段「{}」已停用；请先启用它，或改指到其他父字段",
                    binding.display_name()
                ),
            ));
        }
        let ancestor = validate_target_field(fields, parent, bindings, Some(&binding.source_key))?;
        // 多选列可以当叶子取数列（逐元素展开），但不能当级联父列：父键按
        // 「一格一个文案」折叠，多值父会让子项挂到不确定的父上。
        if ancestor
            .field_type
            .is_some_and(|field_type| !is_single_value_field_type(field_type))
        {
            return Err(BaseError::ParamInvalid(
                "parent_field_id".to_string(),
                format!(
                    "父字段「{}」是多选列，不能作为级联父列；多选列只能作为叶子取数列",
                    ancestor.field_name.trim()
                ),
            ));
        }
        cursor = trimmed(binding.parent_field_id.as_deref());
    }
    Ok(())
}

/// 校验「提议后的启用绑定图」：字段 ID 唯一、父字段存在且启用、父链不成环。
///
/// 只校验启用中的绑定：停用行不参与拉取，它的父指针坏掉不会影响本轮同步；
/// 但它仍然会被 [`children_of`] 当作子级拦住父字段的改绑/停用，避免留下失效指针。
pub(crate) fn validate_enabled_graph(bindings: &[BindingRow]) -> Result<(), BaseError> {
    let enabled: Vec<&BindingRow> = bindings.iter().filter(|binding| binding.enabled).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    for binding in &enabled {
        if !seen.insert(binding.field_id.as_str()) {
            return Err(BaseError::ParamInvalid(
                "field_id".to_string(),
                format!("字段 {} 在启用绑定里出现了不止一次", binding.field_id),
            ));
        }
    }

    for binding in &enabled {
        let Some(parent) = trimmed(binding.parent_field_id.as_deref()) else {
            continue;
        };
        if parent == binding.field_id {
            return Err(BaseError::ParamInvalid(
                "parent_field_id".to_string(),
                format!("字段「{}」不能把自己设为父字段", binding.display_name()),
            ));
        }
        if !seen.contains(parent) {
            return Err(BaseError::ParamInvalid(
                "parent_field_id".to_string(),
                format!(
                    "字段「{}」的父字段 {} 不在启用绑定里；请先把父字段启用或改指到其他字段",
                    binding.display_name(),
                    parent
                ),
            ));
        }
    }

    let parent_map: HashMap<&str, &str> = enabled
        .iter()
        .filter_map(|binding| {
            trimmed(binding.parent_field_id.as_deref())
                .map(|parent| (binding.field_id.as_str(), parent))
        })
        .collect();
    for start in parent_map.keys().copied() {
        let mut path: Vec<&str> = Vec::new();
        let mut cursor = Some(start);
        while let Some(node) = cursor {
            if let Some(position) = path.iter().position(|&seen| seen == node) {
                return Err(BaseError::ParamInvalid(
                    "parent_field_id".to_string(),
                    format!("父链成环：{}", path[position..].join(" → ")),
                ));
            }
            path.push(node);
            cursor = parent_map.get(node).copied();
        }
    }
    Ok(())
}

/// `table_id` + `field_id` → 默认 `source_key`，与前端 `sourceKeyFromFieldId` 同一口径。
///
/// 必须带 `table_id`：同一 Base 的克隆表共享 `field_id`，只用 `field_id` 会整批撞
/// 全局唯一索引。
pub(crate) fn source_key_from_table_and_field(table_id: &str, field_id: &str) -> String {
    let mut key: String = format!("{}_{}", table_id.trim(), field_id.trim())
        .to_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    if !key.starts_with(|character: char| character.is_ascii_lowercase()) {
        key.insert_str(0, "f_");
    }
    key.truncate(64);
    key
}

/// 在已占用的标识集合里找一个可用的 `source_key`。
///
/// 优先原值；冲突时追加 `_2`、`_3`……。仍然冲突返回 `None`，由调用方给出可读错误。
pub(crate) fn unique_source_key(base: &str, occupied: &HashSet<String>) -> Option<String> {
    if !occupied.contains(base) {
        return Some(base.to_string());
    }
    for suffix_number in 2..=9999 {
        let suffix = format!("_{suffix_number}");
        let keep = 64usize.saturating_sub(suffix.len());
        let stem: String = base.chars().take(keep).collect();
        if stem.is_empty() {
            break;
        }
        let candidate = format!("{stem}{suffix}");
        if !occupied.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// 校验并规范化一个由调用方显式给出的 `source_key`。
pub(crate) fn validate_source_key(
    value: &str,
    occupied: &HashSet<String>,
) -> Result<String, BaseError> {
    let value = value.trim();
    if !valid_source_key(value) {
        return Err(BaseError::ParamInvalid(
            "source_key".to_string(),
            "数据源标识必须是 1..=64 字节、小写字母开头的 [a-z0-9_]".to_string(),
        ));
    }
    if occupied.contains(value) {
        return Err(BaseError::ParamInvalid(
            "source_key".to_string(),
            format!("数据源标识 {value} 已被占用，请换一个标识"),
        ));
    }
    Ok(value.to_string())
}

/// 把 `source_key` 的唯一键冲突折算成参数错误（与创建路径同一口径）。
pub(crate) fn fold_duplicate_source_key(error: BaseError) -> BaseError {
    if let BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(message)) = &error {
        if message.contains("Duplicate entry")
            && message.contains("uk_feishu_datasource_field_source_key")
        {
            return BaseError::ParamInvalid(
                "source_key".to_string(),
                "数据源标识已被占用，请换一个标识".to_string(),
            );
        }
    }
    error
}

/// 改绑时同步门户里按 `field_id` 保存的引用。
///
/// 门户表存了 `query_field_id`、`exposed_columns`、`result_labels` 三处 `field_id`；
/// 原地改绑如果不跟着改，开放列会在查询时被静默丢弃（门户代码解析不到就 `filter_map`
/// 掉），表现为「门户突然少了一列」。所以这三处必须与绑定更新在同一事务里改。
///
/// 返回实际更新的门户数。任何门户的 JSON 解析失败都 fail-closed，不静默跳过。
pub(crate) async fn sync_portal_field_references(
    context: &FeishuContext,
    transaction: &mut Transaction,
    datasource_id: i64,
    old_field_id: &str,
    new_field_id: &str,
) -> Result<u64, BaseError> {
    if old_field_id == new_field_id {
        return Ok(0);
    }
    let rows = context
        .portals()
        .query()
        .select_fields(&["id", "query_field_id", "exposed_columns", "result_labels"])?
        .where_eq("datasource_id", serde_json::json!(datasource_id))?
        .all()
        .await?;

    let mut updated = 0u64;
    for row in rows {
        let portal_id: i64 = row.require("id")?;
        let mut update = Record::new();

        let query_field_id: String = row.require("query_field_id")?;
        if query_field_id == old_field_id {
            update.insert("query_field_id", serde_json::json!(new_field_id));
        }

        let exposed_raw: String = row.require("exposed_columns")?;
        let mut exposed: Vec<String> = serde_json::from_str(&exposed_raw).map_err(|error| {
            BaseError::ConfigError(format!("门户 #{portal_id} 的开放列不是合法 JSON：{error}"))
        })?;
        let before: Vec<String> = exposed.clone();
        for column in &mut exposed {
            if column == old_field_id {
                *column = new_field_id.to_string();
            }
        }
        // 去重但保持顺序：改绑可能让新旧列指向同一个字段。
        let mut seen: HashSet<String> = HashSet::new();
        exposed.retain(|column| seen.insert(column.clone()));
        if exposed != before {
            update.insert(
                "exposed_columns",
                serde_json::json!(serde_json::to_string(&exposed)
                    .map_err(|error| { BaseError::JsonSerializeFailed(error.to_string()) })?),
            );
        }

        if let Some(labels_raw) = row.optional::<String>("result_labels")? {
            let mut labels: HashMap<String, String> =
                serde_json::from_str(&labels_raw).map_err(|error| {
                    BaseError::ConfigError(format!(
                        "门户 #{portal_id} 的展示名映射不是合法 JSON：{error}"
                    ))
                })?;
            if let Some(value) = labels.remove(old_field_id) {
                labels.entry(new_field_id.to_string()).or_insert(value);
                update.insert(
                    "result_labels",
                    serde_json::json!(serde_json::to_string(&labels)
                        .map_err(|error| { BaseError::JsonSerializeFailed(error.to_string()) })?),
                );
            }
        }

        if !update.as_map().is_empty() {
            context
                .portals()
                .query()
                .where_eq("id", serde_json::json!(portal_id))?
                .update_in_tx(transaction, update)
                .await?;
            updated += 1;
        }
    }
    Ok(updated)
}

/// 引用某个 `source_key` 的审批配置（只读影响面）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ApprovalConfigRef {
    pub(crate) id: i64,
    pub(crate) title: Option<String>,
}

/// 查引用该数据源字段的审批配置。
///
/// 改绑不改 `source_key`，所以配置行不会断；但配置里的 `option_map` 快照可能过期，
/// 需要在控制台提示「重新保存配置以重建选项映射」。
pub(crate) async fn affected_approval_configs(
    context: &FeishuContext,
    source_key: &str,
) -> Result<Vec<ApprovalConfigRef>, BaseError> {
    let rows = context
        .approval_field_maps()
        .query()
        .select_fields(&["config_id"])?
        .where_eq("option_source_key", serde_json::json!(source_key))?
        .all()
        .await?;
    let mut ids: Vec<i64> = rows
        .iter()
        .map(|row| row.require("config_id"))
        .collect::<Result<_, BaseError>>()?;
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let values: Vec<serde_json::Value> = ids.iter().map(|id| serde_json::json!(id)).collect();
    let config_rows = context
        .approval_configs()
        .query()
        .select_fields(&["id", "title"])?
        .where_in("id", values)?
        .all()
        .await?;
    let mut titles: HashMap<i64, String> = HashMap::new();
    for row in config_rows {
        titles.insert(row.require("id")?, row.require("title")?);
    }

    Ok(ids
        .into_iter()
        .map(|id| ApprovalConfigRef {
            id,
            title: titles.get(&id).cloned(),
        })
        .collect())
}

/// trim 后非空才算有值。
fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

fn trimmed_owned(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(
        id: i64,
        field_id: &str,
        source_key: &str,
        parent: Option<&str>,
        enabled: bool,
    ) -> BindingRow {
        BindingRow {
            id,
            field_id: field_id.to_string(),
            field_name: Some(format!("字段 {field_id}")),
            source_key: source_key.to_string(),
            parent_field_id: parent.map(str::to_string),
            enabled,
        }
    }

    fn field(field_id: &str, field_name: &str, field_type: i32) -> FieldItem {
        FieldItem {
            is_primary: false,
            field_id: field_id.to_string(),
            field_name: field_name.to_string(),
            field_type: Some(field_type),
            ui_type: None,
            property: None,
        }
    }

    #[test]
    fn a_parent_with_children_is_blocked_before_repoint_or_disable() {
        let bindings = vec![
            binding(1, "fldA", "a", None, true),
            binding(2, "fldB", "b", Some("fldA"), true),
            binding(3, "fldC", "c", Some("fldB"), true),
        ];
        let children = children_of(&bindings, "fldA");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].field_id, "fldB");
        assert!(
            children_of(&bindings, "fldB")[0].field_id == "fldC",
            "孙字段不是 fldA 的直接子级，不应拦 fldA 的维护"
        );
    }

    #[test]
    fn the_enabled_graph_rejects_missing_parents_and_cycles() {
        let missing = vec![
            binding(1, "fldA", "a", None, true),
            binding(2, "fldB", "b", Some("fldGONE"), true),
        ];
        assert!(validate_enabled_graph(&missing).is_err());

        let cycle = vec![
            binding(1, "fldA", "a", Some("fldB"), true),
            binding(2, "fldB", "b", Some("fldA"), true),
        ];
        assert!(validate_enabled_graph(&cycle).is_err());
    }

    #[test]
    fn disabled_bindings_do_not_participate_in_the_enabled_graph() {
        let bindings = vec![
            binding(1, "fldA", "a", None, true),
            binding(2, "fldB", "b", Some("fldGONE"), false),
        ];
        assert!(validate_enabled_graph(&bindings).is_ok());
    }

    #[test]
    fn validate_target_field_rejects_deleted_duplicate_and_unsupported() {
        let fields = vec![
            field("fldA", "币种", 3),
            field("fldB", "币种", 3),
            field("fldC", "附件列", 17),
        ];
        let bindings = vec![binding(1, "fldA", "currency", None, true)];
        let err = validate_target_field(&fields, "fldGONE", &bindings, None)
            .err()
            .unwrap_or_else(|| panic!("缺失字段应被拒"));
        assert!(err.to_string().contains("找不到"));

        let err = validate_target_field(&fields, "fldB", &bindings, None)
            .err()
            .unwrap_or_else(|| panic!("重名字段应被拒"));
        assert!(err.to_string().contains("不唯一"));

        let err = validate_target_field(&fields, "fldC", &bindings, None)
            .err()
            .unwrap_or_else(|| panic!("类型不支持应被拒"));
        assert!(err.to_string().contains("不能作为选项来源"));
    }

    #[test]
    fn validate_target_field_accepts_multi_select_as_a_leaf_source() {
        let fields = vec![field("fldM", "可提供的附件类型", 4)];
        let bindings: Vec<BindingRow> = Vec::new();
        let field = validate_target_field(&fields, "fldM", &bindings, None)
            .unwrap_or_else(|error| panic!("多选列应可作为选项来源: {error}"));
        assert_eq!(field.field_type, Some(4));
    }

    #[test]
    fn a_multi_select_field_cannot_be_a_parent() {
        // 叶子可以，父列不行：父键按一格一个文案折叠，多值父会产生歧义。
        let fields = vec![field("fldM", "多选父", 4), field("fldC", "子", 3)];
        let bindings = vec![
            binding(1, "fldM", "multi", None, true),
            binding(2, "fldC", "child", Some("fldM"), true),
        ];
        let error = validate_ancestor_fields(&fields, &bindings, Some("fldM"))
            .err()
            .unwrap_or_else(|| panic!("多选父列应被拒"));
        assert!(error.to_string().contains("不能作为级联父列"));
    }

    #[test]
    fn validate_target_field_ignores_the_binding_being_repointed() {
        let fields = vec![field("fldA", "币种", 3)];
        let bindings = vec![binding(1, "fldA", "currency", None, true)];
        assert!(validate_target_field(&fields, "fldA", &bindings, Some("currency")).is_ok());
        assert!(
            validate_target_field(&fields, "fldA", &bindings, None).is_err(),
            "同一字段被另一条绑定占用时仍要拒绝"
        );
    }

    #[test]
    fn the_derived_source_key_is_globally_unique_and_legal() {
        let key = source_key_from_table_and_field("tblauuOafa4acvT3", "fldA");
        assert_eq!(key, "tblauuOafa4acvT3_flda".to_lowercase());
        assert!(valid_source_key(&key));

        // 首字符不是字母时补 `f_`；非法字符替换成下划线；截断到 64 字节。
        let key = source_key_from_table_and_field("123", "字段 A");
        assert!(key.starts_with("f_"));
        assert!(valid_source_key(&key));

        let long = source_key_from_table_and_field(&"t".repeat(80), &"f".repeat(80));
        assert_eq!(long.len(), 64);
        assert!(valid_source_key(&long));
    }

    #[test]
    fn a_duplicate_source_key_gets_a_stable_suffix() {
        let mut occupied = HashSet::new();
        occupied.insert("tbl_fld".to_string());
        occupied.insert("tbl_fld_2".to_string());
        assert_eq!(
            unique_source_key("tbl_fld", &occupied).as_deref(),
            Some("tbl_fld_3")
        );

        let explicit = validate_source_key("tbl_fld", &occupied);
        assert!(explicit.is_err(), "显式标识冲突要给出可读错误");
    }

    #[test]
    fn a_parent_must_be_enabled_and_in_the_binding_set() {
        let bindings = vec![
            binding(1, "fldA", "a", None, true),
            binding(2, "fldB", "b", None, false),
        ];
        assert!(validate_parent_for_change(&bindings, "fldX", Some("fldA")).is_ok());
        assert!(
            validate_parent_for_change(&bindings, "fldX", Some("fldB")).is_err(),
            "停用的父字段不能作为新父级"
        );
        assert!(validate_parent_for_change(&bindings, "fldX", Some("fldGONE")).is_err());
        assert!(validate_parent_for_change(&bindings, "fldX", Some("fldX")).is_err());
    }

    #[test]
    fn the_whole_ancestor_chain_must_have_usable_remote_fields() {
        let fields = vec![field("fldA", "根", 3), field("fldB", "父", 3)];
        let bindings = vec![
            binding(1, "fldA", "a", None, true),
            binding(2, "fldB", "b", Some("fldA"), true),
            binding(3, "fldC", "c", Some("fldB"), true),
        ];
        assert!(validate_ancestor_fields(&fields, &bindings, Some("fldB")).is_ok());

        // 祖父被删：只校验直接父（fldB）会放过，整条链校验会拒。
        let missing_grandparent = vec![field("fldB", "父", 3)];
        assert!(
            validate_ancestor_fields(&missing_grandparent, &bindings, Some("fldB")).is_err(),
            "祖先链上任何一级不可用都必须拒绝"
        );

        // 祖先停用同样不可用。
        let mut disabled = bindings.clone();
        disabled[0].enabled = false;
        assert!(validate_ancestor_fields(&fields, &disabled, Some("fldB")).is_err());
    }
}

//! 字段绑定的输入形状与校验——创建与更新**共用**的唯一事实源。
//!
//! 两条路径用同一套规则是刻意的：各写一份的后果是漂移，而漂移只在运行期暴露
//! （比如创建时挡了父链成环、更新时没挡，于是有人能绕过界面造出一个环）。

use std::collections::{HashMap, HashSet};

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::BaseError;

use super::source_key::valid_source_key;

/// 一条字段绑定的输入。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct FieldBindingInput {
    /// 多维表格字段 ID。**身份就是它**，不是字段名——改名不能断链。
    pub(crate) field_id: String,
    /// 进 URL 路径段的数据源标识；全局唯一、创建后不可改。
    pub(crate) source_key: String,
    /// 同表内的父列 `field_id`；无父给 `null` 或省略。
    #[serde(default)]
    pub(crate) parent_field_id: Option<String>,
}

/// 父链是否成环。
///
/// 从每个节点沿父指针走，步数上限取节点数：无环时每步都走到一个**没走过的**节点，
/// 因此最多走「边数 ≤ 节点数」步；超过就一定回到了走过的节点。
///
/// 刻意不用递归——深链会爆栈，而链深由用户输入决定。
pub(crate) fn has_parent_cycle(fields: &[FieldBindingInput]) -> bool {
    let parent: HashMap<&str, &str> = parent_edges(fields).collect();

    for start in parent.keys().copied() {
        let mut cursor = start;
        let mut steps = 0usize;
        while let Some(next) = parent.get(cursor).copied() {
            cursor = next;
            steps += 1;
            if steps > parent.len() {
                return true;
            }
        }
    }
    false
}

/// 有效父边（跳过我空白的父指针）。
fn parent_edges(fields: &[FieldBindingInput]) -> impl Iterator<Item = (&str, &str)> {
    fields.iter().filter_map(|field| {
        field
            .parent_field_id
            .as_deref()
            .map(str::trim)
            .filter(|parent| !parent.is_empty())
            .map(|parent| (field.field_id.trim(), parent))
    })
}

/// 校验一份勾选集合。
///
/// 顺序有讲究：先查便宜的形状，再查需要建索引的集合关系，最后才查父指针——
/// 错误消息要指向**最先出问题**的那个输入。
pub(crate) fn validate_fields(fields: &[FieldBindingInput]) -> Result<(), BaseError> {
    if fields.is_empty() {
        return Err(BaseError::ParamInvalid(
            "fields".to_string(),
            "至少要勾选一个字段".to_string(),
        ));
    }

    let mut seen_ids: HashSet<&str> = HashSet::new();
    let mut seen_keys: HashSet<&str> = HashSet::new();
    for field in fields {
        let field_id = field.field_id.trim();
        if field_id.is_empty() {
            return Err(BaseError::ParamInvalid(
                "field_id".to_string(),
                "字段 ID 不能为空".to_string(),
            ));
        }
        if !seen_ids.insert(field_id) {
            return Err(BaseError::ParamInvalid(
                "field_id".to_string(),
                format!("字段 {field_id} 勾选了不止一次"),
            ));
        }
        if !valid_source_key(field.source_key.trim()) {
            return Err(BaseError::ParamInvalid(
                "source_key".to_string(),
                "数据源标识必须是 1..=64 字节、小写字母开头的 [a-z0-9_]".to_string(),
            ));
        }
        if !seen_keys.insert(field.source_key.trim()) {
            return Err(BaseError::ParamInvalid(
                "source_key".to_string(),
                format!("数据源标识 {} 重复", field.source_key.trim()),
            ));
        }
    }

    // 父指针必须在**勾选集合内**：没勾就没有它的选项可挂，拉取时也读不到父列。
    for field in fields {
        let Some(parent) = field
            .parent_field_id
            .as_deref()
            .map(str::trim)
            .filter(|parent| !parent.is_empty())
        else {
            continue;
        };
        if parent == field.field_id.trim() {
            return Err(BaseError::ParamInvalid(
                "parent_field_id".to_string(),
                "不能把自己设为父列".to_string(),
            ));
        }
        if !seen_ids.contains(parent) {
            return Err(BaseError::ParamInvalid(
                "parent_field_id".to_string(),
                format!("父列 {parent} 不在勾选集合内"),
            ));
        }
    }
    if has_parent_cycle(fields) {
        return Err(BaseError::ParamInvalid(
            "parent_field_id".to_string(),
            "父链成环".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(field_id: &str, source_key: &str, parent: Option<&str>) -> FieldBindingInput {
        FieldBindingInput {
            field_id: field_id.to_string(),
            source_key: source_key.to_string(),
            parent_field_id: parent.map(str::to_string),
        }
    }

    #[test]
    fn accepts_a_three_level_chain() {
        // 目标台账上真有三级：费用大类 → 费用类型 → 银行流水摘要-编码
        let fields = vec![
            field("fldTyg5VBz", "main_exp_cat", None),
            field("fldEblAr7X", "fee_type", Some("fldTyg5VBz")),
            field("fldM0j5Do3", "summary_code", Some("fldEblAr7X")),
        ];
        assert!(validate_fields(&fields).is_ok());
    }

    #[test]
    fn rejects_an_empty_set() {
        assert!(validate_fields(&[]).is_err());
    }

    #[test]
    fn rejects_duplicates_self_parent_and_missing_parent() {
        let base = || vec![field("fldA", "a", None), field("fldB", "b", Some("fldA"))];

        let mut dup_id = base();
        dup_id.push(field("fldA", "a2", None));
        assert!(validate_fields(&dup_id).is_err(), "同一列勾两次");

        let mut dup_key = base();
        dup_key[1].source_key = "a".to_string();
        assert!(validate_fields(&dup_key).is_err(), "source_key 重复");

        let mut self_parent = base();
        self_parent[0].parent_field_id = Some("fldA".to_string());
        assert!(validate_fields(&self_parent).is_err(), "自指");

        let mut missing = base();
        missing[1].parent_field_id = Some("fldMissing".to_string());
        assert!(validate_fields(&missing).is_err(), "父不在勾选集合内");
    }

    #[test]
    fn rejects_cycles_of_every_length() {
        let two = vec![
            field("fldA", "a", Some("fldB")),
            field("fldB", "b", Some("fldA")),
        ];
        assert!(validate_fields(&two).is_err(), "两节点环");

        let three = vec![
            field("fldA", "a", Some("fldB")),
            field("fldB", "b", Some("fldC")),
            field("fldC", "c", Some("fldA")),
        ];
        assert!(validate_fields(&three).is_err(), "三节点环");

        let self_loop = vec![field("fldA", "a", Some("fldA"))];
        assert!(validate_fields(&self_loop).is_err(), "自环");
    }

    #[test]
    fn a_blank_parent_pointer_is_treated_as_no_parent() {
        // 界面清空父列时可能提交空串而不是 null；两者语义相同
        let fields = vec![field("fldA", "a", Some("   "))];
        assert!(validate_fields(&fields).is_ok());
    }

    #[test]
    fn rejects_an_illegal_source_key_shape() {
        let fields = vec![field("fldA", "Bad-Key", None)];
        assert!(validate_fields(&fields).is_err());
    }
}

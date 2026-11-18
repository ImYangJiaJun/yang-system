//! 无行键时的明细行对账。
//!
//! 审批表单与台账之间没有稳定行键，唯一可靠的身份是**内容**：同一组明细行
//! 里，内容完全相同就是同一条；内容不同的行无法证明谁对应谁。因此这里做
//! **多重集匹配**——先按映射字段的内容签名抵消完全相同的行，剩余表单行新建、
//! 剩余台账行软作废。唯一例外是组的 leader / 单条的锚点记录：它必须在对账后
//! 继续存在，否则组结算和后续退回同步就没有入口。

use std::collections::BTreeMap;

use super::apply::{current_value, BitableRecord};
use super::encode::encode_cell;
use super::error::SyncError;
use super::plan::{FieldPatch, RecordPlan};
use super::value::SyncValue;

/// 一条已配对的「表单行 → 台账记录」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RowMatch {
    pub(crate) plan_index: usize,
    pub(crate) record_id: String,
}

/// 一轮对账的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MatchPlan {
    pub(crate) pairs: Vec<RowMatch>,
    pub(crate) create_plans: Vec<usize>,
    pub(crate) void_records: Vec<String>,
}

/// 按内容多重集匹配表单行与台账记录。
///
/// `plans` 与表单明细行一一对应；`records` 是当前仍属于本组/本实例的台账记录。
/// `anchor_record_id` 是 leader/锚点记录：表单至少有一行时，它一定被配对（优先
/// 精确匹配，否则占用一条未配对表单行），不会被作废。
pub(crate) fn match_rows(
    plans: &[RecordPlan],
    records: &[BitableRecord],
    anchor_record_id: Option<&str>,
    allow_clear: bool,
) -> Result<MatchPlan, SyncError> {
    if plans.is_empty() {
        return Err(SyncError::EmptyDetailRows);
    }
    let common = common_field_types(plans, allow_clear)?;
    let plan_signatures: Vec<String> = plans
        .iter()
        .map(|plan| plan_signature(plan, &common, allow_clear))
        .collect::<Result<_, _>>()?;
    let record_signatures: Vec<String> = records
        .iter()
        .map(|record| record_signature(record, &common))
        .collect();

    let mut plan_used = vec![false; plans.len()];
    let mut record_used = vec![false; records.len()];
    let mut pairs = Vec::new();

    // leader/锚点固定吃第 0 条表单行：第 0 条计划可能带 leader 专属派生字段
    // （顶层公式/附件并集/多选并集），它只能写到 leader 记录上。其余行仍按
    // 内容精确匹配，剩余行再按顺序配对，最终多重集一定等于表单行集合。
    if let Some(anchor) = anchor_record_id {
        let Some(anchor_index) = records.iter().position(|record| record.record_id == anchor)
        else {
            return Err(SyncError::Review {
                reason: "anchor_record_missing",
            });
        };
        plan_used[0] = true;
        record_used[anchor_index] = true;
        pairs.push(RowMatch {
            plan_index: 0,
            record_id: anchor.to_string(),
        });
    }

    for (record_index, record) in records.iter().enumerate() {
        if record_used[record_index] {
            continue;
        }
        let signature = &record_signatures[record_index];
        if let Some(plan_index) = (0..plans.len())
            .find(|index| !plan_used[*index] && plan_signatures[*index] == *signature)
        {
            plan_used[plan_index] = true;
            record_used[record_index] = true;
            pairs.push(RowMatch {
                plan_index,
                record_id: record.record_id.clone(),
            });
        }
    }

    // 精确匹配后剩下的行/记录按顺序配对：每个计划都会完整覆写它分到的那条
    // 记录，所以无论谁配谁，最终活动行的多重集都等于表单行集合。这样行数不变
    // 的编辑会尽量复用原记录，只有数量变化才新增/作废。
    let remaining_plans: Vec<usize> = (0..plans.len())
        .filter(|index| !plan_used[*index])
        .collect();
    let remaining_records: Vec<usize> = (0..records.len())
        .filter(|index| !record_used[*index])
        .collect();
    for (plan_index, record_index) in remaining_plans.into_iter().zip(remaining_records) {
        plan_used[plan_index] = true;
        record_used[record_index] = true;
        pairs.push(RowMatch {
            plan_index,
            record_id: records[record_index].record_id.clone(),
        });
    }

    let create_plans = (0..plans.len())
        .filter(|index| !plan_used[*index])
        .collect();
    let void_records = (0..records.len())
        .filter(|index| !record_used[*index])
        .map(|index| records[index].record_id.clone())
        .collect();
    Ok(MatchPlan {
        pairs,
        create_plans,
        void_records,
    })
}

/// 所有表单行共有的可比较字段及目标类型。
///
/// 只取交集：条件控件的字段只在部分行出现时，忽略它参与匹配——配对后再按各自
/// 表单行写入，最终多重集仍然正确；取并集则会把“缺失=不清空”误当成期望空值。
fn common_field_types(
    plans: &[RecordPlan],
    allow_clear: bool,
) -> Result<BTreeMap<String, i32>, SyncError> {
    let mut iter = plans.iter();
    let Some(first) = iter.next() else {
        return Ok(BTreeMap::new());
    };
    let mut common = field_types(first, allow_clear)?;
    for plan in iter {
        let types = field_types(plan, allow_clear)?;
        common.retain(|field, field_type| types.get(field) == Some(field_type));
    }
    Ok(common)
}

fn field_types(plan: &RecordPlan, allow_clear: bool) -> Result<BTreeMap<String, i32>, SyncError> {
    let mut types = BTreeMap::new();
    for patch in &plan.patches {
        if matches!(patch.target_type, 18 | 21) {
            // 关联列的回读是 record_id 集合，而计划里是实例 Code；匹配阶段
            // 不做一次额外出站反查，交给 apply 层写后校验。
            continue;
        }
        if patch.value.is_empty() && !allow_clear {
            continue;
        }
        if encode_cell(patch.target_type, &patch.value, allow_clear)?.is_none() {
            continue;
        }
        types.insert(patch.field_id.clone(), patch.target_type);
    }
    Ok(types)
}

fn plan_signature(
    plan: &RecordPlan,
    common: &BTreeMap<String, i32>,
    allow_clear: bool,
) -> Result<String, SyncError> {
    let mut text = String::new();
    for field_id in common.keys() {
        let Some(patch) = plan
            .patches
            .iter()
            .find(|patch| &patch.field_id == field_id)
        else {
            text.push_str(field_id);
            text.push_str("=absent\x1e");
            continue;
        };
        let key = desired_key(patch, allow_clear)?.unwrap_or_else(|| "skip".to_string());
        text.push_str(field_id);
        text.push('=');
        text.push_str(&key);
        text.push('\x1e');
    }
    Ok(text)
}

fn desired_key(patch: &FieldPatch, allow_clear: bool) -> Result<Option<String>, SyncError> {
    if matches!(patch.target_type, 18 | 21) {
        return Ok(None);
    }
    if patch.value.is_empty() && !allow_clear {
        return Ok(None);
    }
    let Some(cell) = encode_cell(patch.target_type, &patch.value, allow_clear)? else {
        return Ok(None);
    };
    let desired = if patch.target_type == 17 {
        // 附件编码后只剩 file_token；名称/大小在 patch.value 里，与 apply 同源。
        patch.value.clone()
    } else {
        current_value(patch.target_type, &cell)
    };
    Ok(Some(desired.canonical_key()))
}

fn record_signature(record: &BitableRecord, common: &BTreeMap<String, i32>) -> String {
    let mut text = String::new();
    for (field_id, field_type) in common {
        let value = record
            .cells
            .get(field_id)
            .map(|cell| current_value(*field_type, cell))
            .unwrap_or(SyncValue::Empty);
        text.push_str(field_id);
        text.push('=');
        text.push_str(&value.canonical_key());
        text.push('\x1e');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::feishu::domain::approval_form_sync::plan::FieldPatch;
    use crate::addon::feishu::domain::approval_form_sync::value::SyncValue;
    use serde_json::json;

    fn plan(values: &[(&str, &str)]) -> RecordPlan {
        RecordPlan {
            record_id: String::new(),
            task_id: None,
            patches: values
                .iter()
                .map(|(field, value)| FieldPatch {
                    field_id: (*field).to_string(),
                    target_type: 1,
                    value: SyncValue::Text((*value).to_string()),
                })
                .collect(),
            skipped: Vec::new(),
            reviews: Vec::new(),
            verifications: Vec::new(),
        }
    }

    fn record(id: &str, values: &[(&str, &str)]) -> BitableRecord {
        BitableRecord {
            record_id: id.to_string(),
            cells: values
                .iter()
                .map(|(field, value)| ((*field).to_string(), json!(value)))
                .collect(),
        }
    }

    #[test]
    fn identical_rows_pair_without_creates_or_voids() {
        let plans = [plan(&[("f", "A")]), plan(&[("f", "B")])];
        let records = [record("r1", &[("f", "A")]), record("r2", &[("f", "B")])];
        let matched = match_rows(&plans, &records, Some("r1"), false)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(matched.pairs.len(), 2);
        assert!(matched.create_plans.is_empty());
        assert!(matched.void_records.is_empty());
        assert_eq!(
            matched.pairs,
            vec![
                RowMatch {
                    plan_index: 0,
                    record_id: "r1".into()
                },
                RowMatch {
                    plan_index: 1,
                    record_id: "r2".into()
                }
            ]
        );
    }

    #[test]
    fn added_row_becomes_a_create() {
        let plans = [
            plan(&[("f", "A")]),
            plan(&[("f", "X")]),
            plan(&[("f", "B")]),
        ];
        let records = [record("r1", &[("f", "A")]), record("r2", &[("f", "B")])];
        let matched = match_rows(&plans, &records, Some("r1"), false)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(matched.pairs.len(), 2);
        assert_eq!(matched.create_plans, vec![1]);
        assert!(matched.void_records.is_empty());
    }

    #[test]
    fn deleted_row_becomes_a_void() {
        let plans = [plan(&[("f", "A")])];
        let records = [record("r1", &[("f", "A")]), record("r2", &[("f", "B")])];
        let matched = match_rows(&plans, &records, Some("r1"), false)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(matched.pairs.len(), 1);
        assert!(matched.create_plans.is_empty());
        assert_eq!(matched.void_records, vec!["r2"]);
    }

    #[test]
    fn edited_non_anchor_row_reuses_the_existing_record() {
        let plans = [plan(&[("f", "A")]), plan(&[("f", "B2")])];
        let records = [record("r1", &[("f", "A")]), record("r2", &[("f", "B")])];
        let matched = match_rows(&plans, &records, Some("r1"), false)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(matched.pairs.len(), 2);
        assert_eq!(
            matched.pairs[1],
            RowMatch {
                plan_index: 1,
                record_id: "r2".into()
            }
        );
        assert!(matched.create_plans.is_empty());
        assert!(matched.void_records.is_empty());
    }

    #[test]
    fn edited_anchor_row_is_updated_in_place() {
        let plans = [plan(&[("f", "A2")])];
        let records = [record("r1", &[("f", "A")])];
        let matched = match_rows(&plans, &records, Some("r1"), false)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            matched.pairs,
            vec![RowMatch {
                plan_index: 0,
                record_id: "r1".into()
            }]
        );
        assert!(matched.create_plans.is_empty());
        assert!(matched.void_records.is_empty());
    }

    #[test]
    fn deleted_anchor_reuses_the_anchor_and_voids_the_other_record() {
        let plans = [plan(&[("f", "B")])];
        let records = [record("r1", &[("f", "A")]), record("r2", &[("f", "B")])];
        let matched = match_rows(&plans, &records, Some("r1"), false)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            matched.pairs,
            vec![RowMatch {
                plan_index: 0,
                record_id: "r1".into()
            }]
        );
        assert!(matched.create_plans.is_empty());
        assert_eq!(matched.void_records, vec!["r2"]);
    }

    #[test]
    fn duplicate_rows_match_by_multiplicity() {
        let plans = [plan(&[("f", "A")]), plan(&[("f", "A")])];
        let records = [
            record("r1", &[("f", "A")]),
            record("r2", &[("f", "A")]),
            record("r3", &[("f", "A")]),
        ];
        let matched = match_rows(&plans, &records, Some("r1"), false)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(matched.pairs.len(), 2);
        assert!(matched.create_plans.is_empty());
        assert_eq!(matched.void_records, vec!["r3"]);
    }

    #[test]
    fn empty_plans_are_rejected() {
        let records = [record("r1", &[("f", "A")])];
        assert_eq!(
            match_rows(&[], &records, Some("r1"), false),
            Err(SyncError::EmptyDetailRows)
        );
    }
}

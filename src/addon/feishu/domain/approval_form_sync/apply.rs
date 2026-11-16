//! 差异与应用：读当前台账 → 只写变化字段 → 写后回读校验。
//!
//! `BitableStore` 用 `field_id` 作键；真实实现负责把 `field_id` 解析成当前字段名
//! （`batch_update` 的 `fields` 按字段名），本层不依赖列名。

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use serde_json::Value;

use super::super::approval_export::form::{text_of_value, FixedNumber};
use super::encode::encode_cell;
use super::error::SyncError;
use super::plan::RecordPlan;
use super::value::SyncValue;

/// 多维表当前记录（单元格按 `field_id` 归一）。
#[derive(Debug, Clone)]
pub(crate) struct BitableRecord {
    pub(crate) record_id: String,
    pub(crate) cells: BTreeMap<String, Value>,
}

/// 一条待更新记录（键是 `field_id`，值已编码）。
#[derive(Debug, Clone)]
pub(crate) struct BitableUpdate {
    pub(crate) record_id: String,
    pub(crate) cells: BTreeMap<String, Value>,
}

/// 台账读写端口；真实实现负责分页/限流/字段名解析。
#[async_trait]
pub(crate) trait BitableStore: Send + Sync {
    async fn get_records(&self, record_ids: &[String]) -> Result<Vec<BitableRecord>, SyncError>;
    async fn update_records(&self, updates: &[BitableUpdate]) -> Result<(), SyncError>;
}

/// 关联审批列（18/21）的实例 Code → record_id。
#[async_trait]
pub(crate) trait ConnectResolver: Send + Sync {
    async fn record_ids(&self, codes: &[String]) -> Result<Vec<String>, SyncError>;
}

/// 一次应用的结果。
#[derive(Debug, Default, Clone)]
pub(crate) struct ApplyReport {
    pub(crate) written: Vec<String>,
    pub(crate) unchanged: Vec<String>,
    pub(crate) reviews: Vec<SyncError>,
}

/// 把一条记录的映射结果应用到台账。
pub(crate) async fn apply_plan(
    store: &dyn BitableStore,
    connects: &dyn ConnectResolver,
    plan: &RecordPlan,
    allow_clear: bool,
    verify: bool,
) -> Result<ApplyReport, SyncError> {
    let mut report = ApplyReport {
        reviews: plan
            .reviews
            .iter()
            .map(|review| review.error.clone())
            .collect(),
        ..ApplyReport::default()
    };
    if plan.patches.is_empty() {
        return Ok(report);
    }

    let records = store
        .get_records(std::slice::from_ref(&plan.record_id))
        .await?;
    let current = records
        .into_iter()
        .find(|record| record.record_id == plan.record_id)
        .ok_or(SyncError::BitableRead {
            record_id: plan.record_id.clone(),
            reason: "record_not_found",
        })?;

    let mut cells = BTreeMap::new();
    for patch in &plan.patches {
        if patch.value.is_empty() && !allow_clear {
            report.unchanged.push(patch.field_id.clone());
            continue;
        }
        // 关联审批列（18/21）需要先把实例 Code 反查成 record_id；文本/超链接
        // 目标由 encode 直接处理 Code。
        if matches!(patch.target_type, 18 | 21) {
            let SyncValue::ConnectCodes(codes) = &patch.value else {
                report.reviews.push(SyncError::InvalidValue {
                    widget_id: String::new(),
                    reason: "关联列的值不是实例 Code",
                });
                continue;
            };
            let desired = connects.record_ids(codes).await?;
            let current_ids = current
                .cells
                .get(&patch.field_id)
                .map(link_record_ids)
                .unwrap_or_default();
            let desired_set: BTreeSet<&String> = desired.iter().collect();
            let current_set: BTreeSet<&String> = current_ids.iter().collect();
            if desired_set == current_set {
                report.unchanged.push(patch.field_id.clone());
            } else {
                cells.insert(
                    patch.field_id.clone(),
                    serde_json::json!({ "link_record_ids": desired }),
                );
                report.written.push(patch.field_id.clone());
            }
            continue;
        }

        let Some(cell) = encode_cell(patch.target_type, &patch.value, allow_clear)? else {
            report.unchanged.push(patch.field_id.clone());
            continue;
        };
        let current_value = current
            .cells
            .get(&patch.field_id)
            .map(|value| current_value(patch.target_type, value))
            .unwrap_or(SyncValue::Empty);
        if current_value.canonical_key() == patch.value.canonical_key() {
            report.unchanged.push(patch.field_id.clone());
        } else {
            cells.insert(patch.field_id.clone(), cell);
            report.written.push(patch.field_id.clone());
        }
    }

    if cells.is_empty() {
        return Ok(report);
    }
    store
        .update_records(&[BitableUpdate {
            record_id: plan.record_id.clone(),
            cells: cells.clone(),
        }])
        .await?;

    if verify {
        let after = store
            .get_records(std::slice::from_ref(&plan.record_id))
            .await?;
        let after = after
            .into_iter()
            .find(|record| record.record_id == plan.record_id)
            .ok_or(SyncError::BitableRead {
                record_id: plan.record_id.clone(),
                reason: "record_disappeared_after_write",
            })?;
        for (field_id, expected) in cells {
            let Some(actual) = after.cells.get(&field_id) else {
                report.reviews.push(SyncError::BitableWrite {
                    record_id: plan.record_id.clone(),
                    reason: "field_missing_after_write",
                });
                continue;
            };
            let expected_value = current_value(
                // 目标类型不影响 canonical 比较：直接按期望值的类型解析。
                target_type_of(&expected),
                &expected,
            );
            let actual_value = current_value(target_type_of(&expected), actual);
            if expected_value.canonical_key() != actual_value.canonical_key() {
                report.reviews.push(SyncError::BitableWrite {
                    record_id: plan.record_id.clone(),
                    reason: "write_verify_mismatch",
                });
            }
        }
    }
    Ok(report)
}

/// 当前台账单元格 → 语义值。
fn current_value(field_type: i32, cell: &Value) -> SyncValue {
    match field_type {
        1 | 13 => text_cell(cell)
            .map(SyncValue::Text)
            .unwrap_or(SyncValue::Empty),
        2 => number_cell(cell)
            .map(SyncValue::Number)
            .unwrap_or(SyncValue::Empty),
        3 => label_cell(cell)
            .map(|label| SyncValue::Labels(vec![label]))
            .unwrap_or(SyncValue::Empty),
        4 => {
            let labels = labels_cell(cell);
            if labels.is_empty() {
                SyncValue::Empty
            } else {
                SyncValue::Labels(labels)
            }
        }
        5 => millis_cell(cell)
            .map(SyncValue::Millis)
            .unwrap_or(SyncValue::Empty),
        7 => match cell {
            Value::Bool(flag) => SyncValue::Bool(*flag),
            Value::String(text) if text == "true" || text == "false" => {
                SyncValue::Bool(text == "true")
            }
            _ => SyncValue::Empty,
        },
        11 => {
            let ids = person_ids_cell(cell);
            if ids.is_empty() {
                SyncValue::Empty
            } else {
                SyncValue::PersonIds(ids)
            }
        }
        15 => text_cell(cell)
            .map(SyncValue::Text)
            .unwrap_or(SyncValue::Empty),
        17 => {
            let files = attachments_cell(cell);
            if files.is_empty() {
                SyncValue::Empty
            } else {
                SyncValue::Attachments(files)
            }
        }
        18 | 21 => {
            let ids = link_record_ids(cell);
            if ids.is_empty() {
                SyncValue::Empty
            } else {
                SyncValue::Text(ids.join(","))
            }
        }
        _ => SyncValue::Empty,
    }
}

fn target_type_of(cell: &Value) -> i32 {
    match cell {
        Value::String(_) => 1,
        Value::Bool(_) => 7,
        Value::Number(_) => 2,
        Value::Array(items) => {
            if items.iter().all(|item| item.get("file_token").is_some()) {
                17
            } else if items.iter().all(|item| item.get("id").is_some()) {
                11
            } else if items.iter().all(Value::is_string) {
                4
            } else {
                1
            }
        }
        // 关联列的回读值形如 `{"link_record_ids": [...]}`；必须按 18 解析，
        // 否则期望值与实际值都会退化成空值，写后校验变成永远通过。
        Value::Object(map) if map.get("link_record_ids").is_some() => 18,
        _ => 1,
    }
}

fn text_cell(cell: &Value) -> Option<String> {
    let text = text_of_value(cell);
    (!text.trim().is_empty()).then_some(text)
}

fn number_cell(cell: &Value) -> Option<FixedNumber> {
    match cell {
        Value::Number(_) | Value::String(_) => FixedNumber::from_json(cell),
        Value::Array(items) if items.len() == 1 => number_cell(&items[0]),
        Value::Object(map) => map.get("value").and_then(number_cell),
        _ => None,
    }
}

fn millis_cell(cell: &Value) -> Option<i64> {
    match cell {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.trim().parse::<i64>().ok(),
        Value::Array(items) if items.len() == 1 => millis_cell(&items[0]),
        Value::Object(map) => map
            .get("value")
            .and_then(millis_cell)
            .or_else(|| map.get("start").and_then(millis_cell)),
        _ => None,
    }
}

fn label_cell(cell: &Value) -> Option<String> {
    match cell {
        Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        Value::Number(number) => Some(number.to_string()),
        Value::Object(map) => ["text", "name", "label", "value"]
            .iter()
            .find_map(|key| map.get(*key).and_then(label_cell)),
        Value::Array(items) => items.first().and_then(label_cell),
        _ => None,
    }
}

fn labels_cell(cell: &Value) -> Vec<String> {
    fn collect(cell: &Value, labels: &mut Vec<String>) {
        match cell {
            Value::Array(items) => items.iter().for_each(|item| collect(item, labels)),
            _ => {
                if let Some(label) = label_cell(cell) {
                    if !labels.contains(&label) {
                        labels.push(label);
                    }
                }
            }
        }
    }
    let mut labels = Vec::new();
    collect(cell, &mut labels);
    labels
}

fn person_ids_cell(cell: &Value) -> Vec<String> {
    fn collect(cell: &Value, ids: &mut Vec<String>) {
        match cell {
            Value::String(text) => {
                for part in text.split([',', '，', ';', '；']) {
                    let part = part.trim();
                    if !part.is_empty() && !ids.contains(&part.to_string()) {
                        ids.push(part.to_string());
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| collect(item, ids)),
            Value::Object(map) => {
                if let Some(id) = ["id", "open_id", "user_id"]
                    .iter()
                    .find_map(|key| map.get(*key).and_then(Value::as_str))
                {
                    if !ids.contains(&id.to_string()) {
                        ids.push(id.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    let mut ids = Vec::new();
    collect(cell, &mut ids);
    ids
}

fn attachments_cell(cell: &Value) -> Vec<super::value::PreparedAttachment> {
    fn collect(cell: &Value, files: &mut Vec<super::value::PreparedAttachment>) {
        match cell {
            Value::Array(items) => items.iter().for_each(|item| collect(item, files)),
            Value::Object(map) => {
                let Some(token) = map.get("file_token").and_then(Value::as_str) else {
                    return;
                };
                let name = map
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let size = map.get("size").and_then(Value::as_i64);
                if !token.trim().is_empty() {
                    files.push(super::value::PreparedAttachment {
                        file_token: token.to_string(),
                        name,
                        size,
                    });
                }
            }
            _ => {}
        }
    }
    let mut files = Vec::new();
    collect(cell, &mut files);
    files
}

fn link_record_ids(cell: &Value) -> Vec<String> {
    fn collect(cell: &Value, ids: &mut Vec<String>) {
        match cell {
            Value::String(text) => {
                if !text.trim().is_empty() && !ids.contains(&text.to_string()) {
                    ids.push(text.to_string());
                }
            }
            Value::Array(items) => items.iter().for_each(|item| collect(item, ids)),
            Value::Object(map) => {
                if let Some(items) = map.get("link_record_ids") {
                    collect(items, ids);
                } else if let Some(id) = map.get("id").and_then(Value::as_str) {
                    if !ids.contains(&id.to_string()) {
                        ids.push(id.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    let mut ids = Vec::new();
    collect(cell, &mut ids);
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::feishu::domain::approval_form_sync::plan::{
        FieldPatch, RecordPlan, ReviewField,
    };
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeStore {
        records: Mutex<BTreeMap<String, BTreeMap<String, Value>>>,
        updates: Mutex<Vec<BitableUpdate>>,
        fail_write: Mutex<bool>,
        corrupt_write: Mutex<bool>,
        tamper_connect: Mutex<bool>,
    }

    impl FakeStore {
        fn with(record_id: &str, cells: BTreeMap<String, Value>) -> Self {
            let store = Self::default();
            store
                .records
                .lock()
                .unwrap_or_else(|error| panic!("{error}"))
                .insert(record_id.to_string(), cells);
            store
        }

        fn updates(&self) -> Vec<BitableUpdate> {
            self.updates
                .lock()
                .map(|updates| updates.clone())
                .unwrap_or_default()
        }
    }

    #[async_trait]
    impl BitableStore for FakeStore {
        async fn get_records(
            &self,
            record_ids: &[String],
        ) -> Result<Vec<BitableRecord>, SyncError> {
            let records = self.records.lock().map_err(|_| SyncError::BitableRead {
                record_id: String::new(),
                reason: "lock",
            })?;
            Ok(record_ids
                .iter()
                .filter_map(|record_id| {
                    records.get(record_id).map(|cells| BitableRecord {
                        record_id: record_id.clone(),
                        cells: cells.clone(),
                    })
                })
                .collect())
        }

        async fn update_records(&self, updates: &[BitableUpdate]) -> Result<(), SyncError> {
            if *self
                .fail_write
                .lock()
                .map_err(|_| SyncError::BitableWrite {
                    record_id: String::new(),
                    reason: "lock",
                })?
            {
                return Err(SyncError::BitableWrite {
                    record_id: updates[0].record_id.clone(),
                    reason: "permission_denied",
                });
            }
            let corrupt = *self
                .corrupt_write
                .lock()
                .map_err(|_| SyncError::BitableWrite {
                    record_id: String::new(),
                    reason: "lock",
                })?;
            let tamper_connect =
                *self
                    .tamper_connect
                    .lock()
                    .map_err(|_| SyncError::BitableWrite {
                        record_id: String::new(),
                        reason: "lock",
                    })?;
            let mut records = self.records.lock().map_err(|_| SyncError::BitableWrite {
                record_id: String::new(),
                reason: "lock",
            })?;
            for update in updates {
                if let Some(record) = records.get_mut(&update.record_id) {
                    for (field_id, value) in &update.cells {
                        let stored = if corrupt {
                            Value::String("corrupted".into())
                        } else if tamper_connect && value.get("link_record_ids").is_some() {
                            serde_json::json!({"link_record_ids": ["rec-OTHER"]})
                        } else {
                            value.clone()
                        };
                        record.insert(field_id.clone(), stored);
                    }
                }
            }
            if let Ok(mut stored) = self.updates.lock() {
                stored.extend_from_slice(updates);
            }
            Ok(())
        }
    }

    struct FakeConnects;

    #[async_trait]
    impl ConnectResolver for FakeConnects {
        async fn record_ids(&self, codes: &[String]) -> Result<Vec<String>, SyncError> {
            Ok(codes.iter().map(|code| format!("rec-{code}")).collect())
        }
    }

    fn make_plan(record_id: &str, patches: Vec<FieldPatch>) -> RecordPlan {
        RecordPlan {
            record_id: record_id.to_string(),
            task_id: Some(1),
            patches,
            skipped: Vec::new(),
            reviews: Vec::new(),
        }
    }

    fn number(text: &str) -> SyncValue {
        SyncValue::Number(FixedNumber::parse(text).unwrap_or_else(|| panic!("{text}")))
    }

    #[tokio::test]
    async fn writes_only_changed_fields_and_skips_unchanged() {
        let store = FakeStore::with(
            "rec1",
            BTreeMap::from([
                ("fld_text".to_string(), serde_json::json!("同じ")),
                ("fld_amount".to_string(), serde_json::json!(100)),
            ]),
        );
        let plan = make_plan(
            "rec1",
            vec![
                FieldPatch {
                    field_id: "fld_text".into(),
                    target_type: 1,
                    value: SyncValue::Text("同じ".into()),
                },
                FieldPatch {
                    field_id: "fld_amount".into(),
                    target_type: 2,
                    value: number("120"),
                },
            ],
        );
        let report = apply_plan(&store, &FakeConnects, &plan, false, true)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(report.written, vec!["fld_amount".to_string()]);
        assert_eq!(report.unchanged, vec!["fld_text".to_string()]);
        let updates = store.updates();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].cells.len(), 1);
        assert_eq!(updates[0].cells["fld_amount"], serde_json::json!(120));
    }

    #[tokio::test]
    async fn unchanged_plan_does_not_write() {
        let store = FakeStore::with(
            "rec1",
            BTreeMap::from([("fld_amount".to_string(), serde_json::json!(120))]),
        );
        let plan = make_plan(
            "rec1",
            vec![FieldPatch {
                field_id: "fld_amount".into(),
                target_type: 2,
                value: number("120"),
            }],
        );
        apply_plan(&store, &FakeConnects, &plan, false, false)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(store.updates().is_empty());
    }

    #[tokio::test]
    async fn write_failure_and_verify_mismatch_are_reported() {
        let store = FakeStore::with(
            "rec1",
            BTreeMap::from([("fld_amount".to_string(), serde_json::json!(100))]),
        );
        *store
            .fail_write
            .lock()
            .unwrap_or_else(|error| panic!("{error}")) = true;
        let plan = make_plan(
            "rec1",
            vec![FieldPatch {
                field_id: "fld_amount".into(),
                target_type: 2,
                value: number("120"),
            }],
        );
        assert!(matches!(
            apply_plan(&store, &FakeConnects, &plan, false, false).await,
            Err(SyncError::BitableWrite { .. })
        ));

        *store
            .fail_write
            .lock()
            .unwrap_or_else(|error| panic!("{error}")) = false;
        *store
            .corrupt_write
            .lock()
            .unwrap_or_else(|error| panic!("{error}")) = true;
        let report = apply_plan(&store, &FakeConnects, &plan, false, true)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(!report.reviews.is_empty(), "写后回读不一致必须 review");
    }

    #[tokio::test]
    async fn connect_relation_resolves_codes_and_compares_record_ids() {
        let store = FakeStore::with(
            "rec1",
            BTreeMap::from([(
                "fld_connect".to_string(),
                serde_json::json!({"link_record_ids":["rec-A"]}),
            )]),
        );
        let plan = make_plan(
            "rec1",
            vec![FieldPatch {
                field_id: "fld_connect".into(),
                target_type: 18,
                value: SyncValue::ConnectCodes(vec!["A".into()]),
            }],
        );
        apply_plan(&store, &FakeConnects, &plan, false, false)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(store.updates().is_empty(), "集合相同不写");

        let plan = make_plan(
            "rec1",
            vec![FieldPatch {
                field_id: "fld_connect".into(),
                target_type: 18,
                value: SyncValue::ConnectCodes(vec!["A".into(), "B".into()]),
            }],
        );
        apply_plan(&store, &FakeConnects, &plan, false, false)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let updates = store.updates();
        assert_eq!(
            updates[0].cells["fld_connect"],
            serde_json::json!({"link_record_ids":["rec-A","rec-B"]})
        );
    }

    #[tokio::test]
    async fn connect_readback_compares_link_record_ids() {
        let store = FakeStore::with(
            "rec1",
            BTreeMap::from([(
                "fld_connect".to_string(),
                serde_json::json!({"link_record_ids":[]}),
            )]),
        );
        *store
            .tamper_connect
            .lock()
            .unwrap_or_else(|error| panic!("{error}")) = true;
        let plan = make_plan(
            "rec1",
            vec![FieldPatch {
                field_id: "fld_connect".into(),
                target_type: 18,
                value: SyncValue::ConnectCodes(vec!["A".into()]),
            }],
        );
        let report = apply_plan(&store, &FakeConnects, &plan, false, true)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(
            report
                .reviews
                .iter()
                .any(|error| error.code() == "bitable_write"),
            "关联列回读成另一组 record_id 必须报 review"
        );
    }

    #[tokio::test]
    async fn clear_is_skipped_when_not_allowed() {
        let store = FakeStore::with(
            "rec1",
            BTreeMap::from([("fld_text".to_string(), serde_json::json!("旧值"))]),
        );
        let plan = make_plan(
            "rec1",
            vec![FieldPatch {
                field_id: "fld_text".into(),
                target_type: 1,
                value: SyncValue::Empty,
            }],
        );
        let report = apply_plan(&store, &FakeConnects, &plan, false, false)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(report.written.is_empty());
        assert!(store.updates().is_empty());
    }

    #[tokio::test]
    async fn plan_reviews_are_carried_into_report() {
        let store = FakeStore::with("rec1", BTreeMap::new());
        let mut plan = make_plan("rec1", Vec::new());
        plan.reviews.push(ReviewField {
            field_id: "fld".into(),
            error: SyncError::Review { reason: "drift" },
        });
        let report = apply_plan(&store, &FakeConnects, &plan, false, false)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(report.reviews.len(), 1);
    }
}

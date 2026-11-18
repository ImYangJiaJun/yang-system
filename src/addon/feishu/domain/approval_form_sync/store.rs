//! 真实多维表读写端口。
//!
//! `batch_update` 的 `fields` 按**当前字段名**作键，而领域层按 `field_id` 组织；
//! 本层负责把 id 解析成当前名，并把 `batch_get` 的按名响应重映射回按 id。

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde_json::Value;

use super::super::approval_convert::connection_codes;
use super::super::bitable::{self, BitableCoordinates, FieldItem, RecordItem};
use super::super::outbound::{OutboundTransport, Sleeper};
use super::super::tenant_token::TenantTokenProvider;
use super::apply::{BitableRecord, BitableStore, BitableUpdate};
use super::error::SyncError;

/// 一次批量写的分片大小；`batch_update` 单次上限更高，但 100 条能把毒记录
/// 隔离在较小的批次里。
const WRITE_CHUNK: usize = 100;

pub(crate) struct BitableStoreImpl<'a> {
    pub(crate) transport: &'a dyn OutboundTransport,
    pub(crate) sleeper: &'a dyn Sleeper,
    pub(crate) tokens: &'a TenantTokenProvider,
    pub(crate) coordinates: &'a BitableCoordinates,
    pub(crate) fields: &'a [FieldItem],
}

#[async_trait]
impl BitableStore for BitableStoreImpl<'_> {
    async fn get_records(&self, record_ids: &[String]) -> Result<Vec<BitableRecord>, SyncError> {
        let mut records = Vec::with_capacity(record_ids.len());
        for chunk in record_ids.chunks(WRITE_CHUNK) {
            let items = bitable::get_records_by_ids(
                self.transport,
                self.sleeper,
                self.tokens,
                self.coordinates,
                chunk,
            )
            .await
            .map_err(|_| SyncError::BitableRead {
                record_id: chunk.first().cloned().unwrap_or_default(),
                reason: "batch_get_failed",
            })?;
            for item in items {
                records.push(to_record(self.fields, item)?);
            }
        }
        Ok(records)
    }

    async fn update_records(&self, updates: &[BitableUpdate]) -> Result<(), SyncError> {
        let mut rows: Vec<(String, BTreeMap<String, Value>)> = Vec::with_capacity(updates.len());
        for update in updates {
            let mut fields = BTreeMap::new();
            for (field_id, cell) in &update.cells {
                let name = bitable::resolve_field_name(self.fields, field_id).map_err(|_| {
                    SyncError::BitableWrite {
                        record_id: update.record_id.clone(),
                        reason: "field_not_found",
                    }
                })?;
                fields.insert(name, cell.clone());
            }
            if !fields.is_empty() {
                rows.push((update.record_id.clone(), fields));
            }
        }
        for chunk in rows.chunks(WRITE_CHUNK) {
            bitable::batch_update_records(
                self.transport,
                self.sleeper,
                self.tokens,
                self.coordinates,
                chunk,
            )
            .await
            .map_err(|_| SyncError::BitableWrite {
                record_id: chunk.first().map(|(id, _)| id.clone()).unwrap_or_default(),
                reason: "batch_update_failed",
            })?;
        }
        Ok(())
    }

    async fn create_record(
        &self,
        fields: BTreeMap<String, Value>,
        client_token: &str,
    ) -> Result<String, SyncError> {
        let mut named = serde_json::Map::with_capacity(fields.len());
        for (field_id, cell) in fields {
            let name = bitable::resolve_field_name(self.fields, &field_id).map_err(|_| {
                SyncError::BitableWrite {
                    record_id: String::new(),
                    reason: "field_not_found",
                }
            })?;
            named.insert(name, cell);
        }
        let created = bitable::create_record(
            self.transport,
            self.sleeper,
            self.tokens,
            self.coordinates,
            named,
            client_token,
        )
        .await
        .map_err(|_| SyncError::BitableWrite {
            record_id: String::new(),
            reason: "create_record_failed",
        })?;
        Ok(created.record_id)
    }
}

fn to_record(fields: &[FieldItem], item: RecordItem) -> Result<BitableRecord, SyncError> {
    let cells = bitable::rekey_cells_by_field_id(fields, &item.fields).map_err(|_| {
        SyncError::BitableRead {
            record_id: item.record_id.clone(),
            reason: "unknown_field_in_record",
        }
    })?;
    Ok(BitableRecord {
        record_id: item.record_id,
        cells: cells.into_iter().collect(),
    })
}

/// 关联审批列（18/21）的实例 Code → record_id。
///
/// 正向派发只支持**同表**关联：从被关联行的回填列里解析实例 Code。反向同样只在
/// 本表扫描回填列；跨表关联正向也不支持，这里不猜。扫描有页数上限，超过即
/// `Review`，不静默截断。
pub(crate) struct BitableConnectResolver<'a> {
    pub(crate) transport: &'a dyn OutboundTransport,
    pub(crate) sleeper: &'a dyn Sleeper,
    pub(crate) tokens: &'a TenantTokenProvider,
    pub(crate) coordinates: &'a BitableCoordinates,
    pub(crate) backfill_field_name: &'a str,
    pub(crate) max_pages: u32,
}

#[async_trait]
impl super::apply::ConnectResolver for BitableConnectResolver<'_> {
    async fn record_ids(&self, codes: &[String]) -> Result<Vec<String>, SyncError> {
        let field_names = vec![self.backfill_field_name.to_string()];
        let snapshot = bitable::list_all_records(
            self.transport,
            self.sleeper,
            self.tokens,
            self.coordinates,
            &field_names,
            self.max_pages,
        )
        .await
        .map_err(|_| SyncError::Review {
            reason: "connect_scan_failed",
        })?;
        let mut by_code: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for item in snapshot.items {
            let Some(cell) = item.fields.get(self.backfill_field_name) else {
                continue;
            };
            let Ok(found) = connection_codes(cell) else {
                continue;
            };
            for code in found {
                by_code
                    .entry(code)
                    .or_default()
                    .push(item.record_id.clone());
            }
        }
        let mut record_ids = Vec::with_capacity(codes.len());
        for code in codes {
            match by_code.get(code).map(Vec::as_slice) {
                Some([record_id]) => record_ids.push(record_id.clone()),
                Some(_) => {
                    return Err(SyncError::OptionAmbiguous {
                        widget_id: String::new(),
                    })
                }
                None => {
                    return Err(SyncError::OptionUnresolved {
                        widget_id: String::new(),
                    })
                }
            }
        }
        Ok(record_ids)
    }
}

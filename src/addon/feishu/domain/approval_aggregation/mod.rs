//! 同批聚合派发：一致性签名摘要、组键、组 uuid、分组规划与成员集合判定。
//!
//! 设计见 `docs/architecture/2026-10-31-feishu-approval-aggregation-design.md` §4.3。
//!
//! # 为什么组键里必须带成员集合
//!
//! 组实例身份（`uuid` / `instance_code` / `serial_number`）只落组行，组 `uuid` 由组键
//! 派生。成员集合变化却不换键的话，「同一签名、不同成员」的两批会派生同一个 `uuid`，
//! 创建时吃 `60012` 并回捞到**别人**的实例；反过来签名变了也不能复用旧组，那是两批
//! 不同的业务，编号与明细行都不同。
//!
//! # 本层不做 I/O
//!
//! 签名由 `approval_convert::build_signature` 产出（签名阶段禁止网络副作用），这里只做
//! 摘要、归并与键派生，全部可离线单测。持久化在 [`repository`]。

#![allow(dead_code)] // 纯函数先落地并自带测试；消费者（播种、组处理）在后续任务接入。

pub(crate) mod repository;

use std::collections::{HashMap, HashSet};

use sha2::{Digest, Sha256};

/// 一条候选记录与它的一致性签名（`build_signature` 的原始 JSON 字符串）。
///
/// 签名原文参与归并、摘要参与组键：同原文必然同摘要，所以用原文做归并键，
/// 省掉「摘要碰撞」这种在业务上不可能发生、却要在代码里解释的假设。
pub(crate) struct GroupCandidate {
    pub(crate) record_id: String,
    pub(crate) signature: String,
}

impl GroupCandidate {
    pub(crate) fn new(record_id: impl Into<String>, signature: impl Into<String>) -> Self {
        Self {
            record_id: record_id.into(),
            signature: signature.into(),
        }
    }
}

/// 一个待新建的组：组键、签名摘要与成员（保持扫描顺序，`group_seq` 由调用方按此下标落）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedGroup {
    pub(crate) group_key: String,
    pub(crate) signature_hash: String,
    pub(crate) members: Vec<String>,
}

/// 签名摘要：`sha256(json)` 前 16 字节的十六进制（32 字符）。
///
/// 取前 16 字节与 `approval_uuid` 同一条口径：够用（签名相等性判定，不是密码学用途）、
/// 稳定、且长度落在组表 `signature_hash` 的 64 字符上限内。
pub(crate) fn signature_hash(signature_json: &str) -> String {
    hex32(&Sha256::digest(signature_json.as_bytes()))
}

/// 组键：`sha256(config_id|signature_hash|排序后的成员 record_id)` 前 16 字节的十六进制。
///
/// 成员按字典序排序后参与，所以扫描顺序变化不会换键；成员集合变化必然换键（每个成员
/// 以 `长度:值` 形式拼入，拼接是单射的——不靠「record_id 里不会出现分隔符」这种假设）。
pub(crate) fn group_key(config_id: i64, signature_hash: &str, record_ids: &[String]) -> String {
    let mut sorted = record_ids.to_vec();
    sorted.sort();
    let mut name = format!("{config_id}|{signature_hash}");
    for record_id in &sorted {
        name.push_str(&format!("|{}:{record_id}", record_id.len()));
    }
    hex32(&Sha256::digest(name.as_bytes()))
}

/// 组 `uuid`：与单条任务同一条派生（`base|table|approval_code|group_key`）。
///
/// 复用 `derive_uuid` 是为了让「组」与「单条」共用同一套幂等语义：换审批定义、
/// 换表都要能作为新单子重发。
pub(crate) fn group_uuid(
    base_token: &str,
    table_id: &str,
    approval_code: &str,
    group_key: &str,
) -> String {
    super::approval_uuid::derive_uuid(base_token, table_id, approval_code, group_key)
}

/// 按签名归并候选记录，返回待新建的组。
///
/// 组成员保持**扫描顺序**（`group_seq` = 成员下标），组顺序 = 签名首次出现顺序。
/// 签名失败的记录由调用方以 `error:{record_id}` 作签名传入：每次不同，于是各自成组
/// （处理期照常暂停报错，而不是被静默合并进正常组）。
pub(crate) fn plan_groups(config_id: i64, candidates: &[GroupCandidate]) -> Vec<PlannedGroup> {
    let mut planned: Vec<PlannedGroup> = Vec::new();
    let mut positions: HashMap<&str, usize> = HashMap::new();
    for candidate in candidates {
        if let Some(&at) = positions.get(candidate.signature.as_str()) {
            planned[at].members.push(candidate.record_id.clone());
            continue;
        }
        positions.insert(candidate.signature.as_str(), planned.len());
        planned.push(PlannedGroup {
            group_key: String::new(),
            signature_hash: signature_hash(&candidate.signature),
            members: vec![candidate.record_id.clone()],
        });
    }
    for group in &mut planned {
        group.group_key = group_key(config_id, &group.signature_hash, &group.members);
    }
    planned
}

/// 组内**全部**成员是否都在本次勾选集合内。
///
/// 解散组的判据之一：只要有一个成员没被重新勾选，就不动这个组——用户没重新勾选，
/// 系统不替他撤回（设计 §4.4）。
pub(crate) fn members_all_selected(members: &[String], selected: &HashSet<String>) -> bool {
    members.iter().all(|member| selected.contains(member))
}

/// 摘要前 16 字节的十六进制串（32 字符）。
fn hex32(digest: &[u8]) -> String {
    digest
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn members(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn plan_groups_merges_identical_signatures_in_scan_order() {
        // 扫描顺序 A、B、C；A 与 C 同签名 ⇒ 一组（成员保持扫描顺序），B 单独一组。
        let planned = plan_groups(
            7,
            &[
                GroupCandidate::new("recA", r#"{"a":1}"#),
                GroupCandidate::new("recB", r#"{"a":2}"#),
                GroupCandidate::new("recC", r#"{"a":1}"#),
            ],
        );
        assert_eq!(planned.len(), 2, "同签名必须合成一组");
        assert_eq!(planned[0].members, members(&["recA", "recC"]));
        assert_eq!(planned[1].members, members(&["recB"]));
        assert_eq!(planned[0].signature_hash, signature_hash(r#"{"a":1}"#));
        assert_ne!(
            planned[0].group_key, planned[1].group_key,
            "不同签名必须派生不同的组键"
        );
    }

    #[test]
    fn group_key_depends_on_member_set() {
        let base = group_key(1, "aaaa", &members(&["recA", "recB"]));
        assert_eq!(
            base,
            group_key(1, "aaaa", &members(&["recB", "recA"])),
            "成员顺序不参与组键"
        );
        assert_eq!(base.len(), 32, "组键是 32 位十六进制");
        assert_ne!(
            base,
            group_key(1, "aaaa", &members(&["recA"])),
            "成员集合变化必须换键"
        );
        assert_ne!(
            base,
            group_key(1, "bbbb", &members(&["recA", "recB"])),
            "签名变化必须换键"
        );
        assert_ne!(
            base,
            group_key(2, "aaaa", &members(&["recA", "recB"])),
            "配置变化必须换键"
        );
    }

    #[test]
    fn group_uuid_is_deterministic_and_36_chars() {
        let value = group_uuid("appBase", "tblTable", "CODE-1", "groupkey");
        assert_eq!(value.len(), 36, "组 uuid 必须是规范 UUID 形态");
        assert_eq!(
            value,
            group_uuid("appBase", "tblTable", "CODE-1", "groupkey")
        );
        assert_ne!(value, group_uuid("appBase", "tblTable", "CODE-1", "other"));
        assert_ne!(
            value,
            group_uuid("appBase", "tblTable", "CODE-2", "groupkey"),
            "换审批定义要能作为新单子重发"
        );
    }

    #[test]
    fn members_all_selected_requires_every_member() {
        let selected: HashSet<String> = members(&["recA", "recB"]).into_iter().collect();
        assert!(members_all_selected(&members(&["recA", "recB"]), &selected));
        assert!(
            !members_all_selected(&members(&["recA", "recC"]), &selected),
            "少一个成员就不算全选"
        );
        assert!(!members_all_selected(&members(&["recC"]), &selected));
    }

    #[test]
    fn signature_hash_is_stable_32_hex() {
        let value = signature_hash(r#"{"widget":"x"}"#);
        assert_eq!(value.len(), 32);
        assert!(value.chars().all(|character| character.is_ascii_hexdigit()));
        assert_eq!(value, signature_hash(r#"{"widget":"x"}"#));
        assert_ne!(value, signature_hash(r#"{"widget":"y"}"#));
    }
}

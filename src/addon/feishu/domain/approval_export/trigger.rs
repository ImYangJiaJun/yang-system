//! 触发节点判定：这个实例现在该不该回写。
//!
//! # 真机口径
//!
//! 1. 节点身份只能用 `task_list[].node_id`。`timeline[].node_key` 是
//!    `APPROVAL_x_y`，与 `node_id` 不是一回事。
//! 2. 节点被退回后重新审批时，`task_list` 会**同时保留**旧任务（`DONE`）与新
//!    任务（`APPROVED`）。先按该节点最近一次 `ROLLBACK` / `ROLLBACK_SELECTED`
//!    的时间过滤旧任务，再判通过。
//! 3. `AND` / `SEQUENTIAL`：过滤后的任务**全部** `APPROVED`（`AUTO_PASS` 算通过）；
//!    `OR`：任一通过。`DONE` / `PENDING` / `REJECTED` / `ROLLBACK` 都不算通过。
//! 4. 实例最终通过 = `status == "APPROVED"` 且 `reverted != true`。

use std::collections::BTreeMap;

use super::super::approval::{InstanceDetail, TaskNode, TimelineRecord};
use super::{ExportRule, TriggerMode, FINAL_NODE_ID};

/// 任务级「已通过」状态。
const PASS_STATUSES: &[&str] = &["APPROVED", "AUTO_PASS"];

/// 会重置节点审核时间的流程动态。
const ROLLBACK_EVENTS: &[&str] = &["ROLLBACK", "ROLLBACK_SELECTED"];

/// 触发判定的结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TriggerOutcome {
    /// 触发条件已满足，可以回写。`node_id` 是规则级 run 行要落的触发节点。
    Passed { node_id: String },
    /// 触发节点还没到达（没有任何任务）。
    NotReached,
    /// 触发节点已通过，但规则要求最终通过且实例仍为 `PENDING`。
    WaitingFinal { node_id: String },
    /// 节点任务存在但未全部通过（或最终通过模式下实例终态不是 APPROVED）。
    NotPassed { node_id: String },
    /// 最近一次退回发生在通过之后，节点重新在审理中。
    RolledBack { node_id: String },
}

impl TriggerOutcome {
    #[cfg(test)]
    pub(crate) fn is_passed(&self) -> bool {
        matches!(self, Self::Passed { .. })
    }

    /// 触发节点（忽略最终通过要求）是否仍然通过。
    ///
    /// 标记判定用：`PENDING` 且这里为 `false`，说明实例被退回了。
    pub(crate) fn node_still_passed(&self) -> bool {
        matches!(self, Self::Passed { .. } | Self::WaitingFinal { .. })
    }
}

/// 计算实例当前是否满足规则的触发条件。
pub(crate) fn compute_node_passed(rule: &ExportRule, detail: &InstanceDetail) -> TriggerOutcome {
    match rule.trigger_mode {
        TriggerMode::FinalApproved => final_outcome(detail),
        TriggerMode::NodePassed => node_outcome(rule, detail),
    }
}

fn final_approved(detail: &InstanceDetail) -> bool {
    detail.status.as_deref() == Some("APPROVED") && detail.reverted != Some(true)
}

fn final_outcome(detail: &InstanceDetail) -> TriggerOutcome {
    if final_approved(detail) {
        TriggerOutcome::Passed {
            node_id: FINAL_NODE_ID.to_string(),
        }
    } else if detail.status.as_deref() == Some("PENDING") {
        TriggerOutcome::WaitingFinal {
            node_id: FINAL_NODE_ID.to_string(),
        }
    } else {
        TriggerOutcome::NotPassed {
            node_id: FINAL_NODE_ID.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeState {
    NotReached,
    Passed,
    NotPassed,
    RolledBack,
}

fn node_outcome(rule: &ExportRule, detail: &InstanceDetail) -> TriggerOutcome {
    let tasks = detail.task_list.as_deref().unwrap_or(&[]);
    let timeline = detail.timeline.as_deref().unwrap_or(&[]);
    let rollbacks = latest_rollbacks(tasks, timeline);

    let mut passed_node: Option<&str> = None;
    let mut rolled_back_node: Option<&str> = None;
    let mut not_passed_node: Option<&str> = None;
    for node_id in &rule.trigger_node_ids {
        match node_state(tasks, node_id, rollbacks.get(node_id).copied()) {
            NodeState::Passed => {
                passed_node = Some(node_id);
                break;
            }
            NodeState::RolledBack => {
                rolled_back_node.get_or_insert(node_id);
            }
            NodeState::NotPassed => {
                not_passed_node.get_or_insert(node_id);
            }
            NodeState::NotReached => {}
        }
    }

    let Some(node_id) = passed_node else {
        if let Some(node_id) = rolled_back_node {
            return TriggerOutcome::RolledBack {
                node_id: node_id.to_string(),
            };
        }
        if let Some(node_id) = not_passed_node {
            return TriggerOutcome::NotPassed {
                node_id: node_id.to_string(),
            };
        }
        return TriggerOutcome::NotReached;
    };

    if !rule.require_final_approval {
        return TriggerOutcome::Passed {
            node_id: node_id.to_string(),
        };
    }
    if final_approved(detail) {
        TriggerOutcome::Passed {
            node_id: node_id.to_string(),
        }
    } else if detail.status.as_deref() == Some("PENDING") {
        TriggerOutcome::WaitingFinal {
            node_id: node_id.to_string(),
        }
    } else {
        // 终态但不是通过（拒绝/撤回/撤销/删除）：节点已过但最终没通过，
        // 已写过的行由标记逻辑处理，这里不再触发新写入。
        TriggerOutcome::NotPassed {
            node_id: node_id.to_string(),
        }
    }
}

fn node_state(tasks: &[TaskNode], node_id: &str, rollback_at: Option<i64>) -> NodeState {
    let mut node_tasks: Vec<&TaskNode> = tasks
        .iter()
        .filter(|task| task.node_id.as_deref() == Some(node_id))
        .collect();
    if node_tasks.is_empty() {
        return NodeState::NotReached;
    }

    // 只统计最近一次退回之后的（新）任务；旧任务没有 start_time 时无法判断，
    // 保留（宁可算作新任务，也不要凭空丢掉一次真实审批）。
    if let Some(rollback_at) = rollback_at {
        node_tasks
            .retain(|task| task_start_time(task).map_or(true, |started| started > rollback_at));
        if node_tasks.is_empty() {
            return NodeState::RolledBack;
        }
    }

    let node_type = node_tasks
        .iter()
        .find_map(|task| task.r#type.as_deref().filter(|value| !value.is_empty()))
        .unwrap_or("AND");
    let passed: Vec<bool> = node_tasks
        .iter()
        .map(|task| {
            task.status
                .as_deref()
                .is_some_and(|status| PASS_STATUSES.contains(&status))
        })
        .collect();
    let node_passed = if node_type == "OR" {
        passed.iter().any(|value| *value)
    } else {
        // AND / SEQUENTIAL / 未知类型都按「全部通过」处理；未知类型从严不会
        // 提前写入，宁可多等一轮。
        passed.iter().all(|value| *value)
    };

    match (node_passed, rollback_at.is_some()) {
        (true, _) => NodeState::Passed,
        (false, true) => NodeState::RolledBack,
        (false, false) => NodeState::NotPassed,
    }
}

/// 每个节点最近一次退回事件的时间（毫秒）。
///
/// 事件通过 `timeline.task_id → task_list.id → node_id` 归属；映射不到的退回
/// 事件无法安全归属，直接忽略（不影响其它节点）。
fn latest_rollbacks(tasks: &[TaskNode], timeline: &[TimelineRecord]) -> BTreeMap<String, i64> {
    let task_node: BTreeMap<&str, &str> = tasks
        .iter()
        .filter_map(|task| Some((task.id.as_deref()?, task.node_id.as_deref()?)))
        .collect();
    let mut latest: BTreeMap<String, i64> = BTreeMap::new();
    for record in timeline {
        let event_type = record.r#type.as_deref().unwrap_or_default();
        if !ROLLBACK_EVENTS.contains(&event_type) {
            continue;
        }
        let Some(created_at) = record
            .create_time
            .as_deref()
            .and_then(|value| value.trim().parse::<i64>().ok())
        else {
            continue;
        };
        let Some(node_id) = record
            .task_id
            .as_deref()
            .and_then(|task_id| task_node.get(task_id).copied())
        else {
            continue;
        };
        let entry = latest.entry(node_id.to_string()).or_insert(created_at);
        if created_at > *entry {
            *entry = created_at;
        }
    }
    latest
}

fn task_start_time(task: &TaskNode) -> Option<i64> {
    task.start_time
        .as_deref()
        .and_then(|value| value.trim().parse::<i64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::feishu::domain::approval_export::Condition;
    use crate::addon::feishu::domain::approval_export::RowFilter;
    use crate::addon::feishu::domain::approval_export::StatusLabels;

    fn rule(mode: TriggerMode, nodes: &[&str], require_final_approval: bool) -> ExportRule {
        ExportRule {
            id: 1,
            title: "测试规则".to_string(),
            approval_code: "code".to_string(),
            trigger_mode: mode,
            trigger_node_ids: nodes.iter().map(|node| node.to_string()).collect(),
            require_final_approval,
            detail_widget_id: "detail".to_string(),
            row_filter: RowFilter::MatchedRows,
            condition: Condition::default(),
            target_base_token: "base".to_string(),
            target_table_id: "table".to_string(),
            status_field: None,
            status_field_name: None,
            status_labels: StatusLabels::default(),
            mark_enabled: true,
            enabled: true,
            effective_from: None,
        }
    }

    fn task(id: &str, node_id: &str, node_type: &str, status: &str, start_time: &str) -> TaskNode {
        TaskNode {
            id: Some(id.to_string()),
            node_id: Some(node_id.to_string()),
            node_name: None,
            user_id: None,
            open_id: Some(format!("ou_{id}")),
            status: Some(status.to_string()),
            r#type: Some(node_type.to_string()),
            start_time: Some(start_time.to_string()),
            end_time: Some("0".to_string()),
        }
    }

    fn detail(
        status: &str,
        reverted: Option<bool>,
        tasks: Vec<TaskNode>,
        timeline: Vec<TimelineRecord>,
    ) -> InstanceDetail {
        InstanceDetail {
            instance_code: "INST-1".to_string(),
            approval_code: Some("code".to_string()),
            serial_number: None,
            status: Some(status.to_string()),
            start_time: Some("1000".to_string()),
            end_time: Some("0".to_string()),
            open_id: None,
            user_id: None,
            department_id: None,
            department_name: None,
            uuid: None,
            reverted,
            task_list: Some(tasks),
            timeline: Some(timeline),
            form: None,
        }
    }

    fn rollback(task_id: &str, create_time: &str) -> TimelineRecord {
        TimelineRecord {
            r#type: Some("ROLLBACK_SELECTED".to_string()),
            create_time: Some(create_time.to_string()),
            user_id: None,
            open_id: None,
            ext: None,
            task_id: Some(task_id.to_string()),
        }
    }

    #[test]
    fn and_node_requires_every_task_approved() {
        let rule = rule(TriggerMode::NodePassed, &["n1"], false);
        let all = detail(
            "PENDING",
            None,
            vec![
                task("1", "n1", "AND", "APPROVED", "1000"),
                task("2", "n1", "AND", "APPROVED", "1001"),
            ],
            vec![],
        );
        assert!(compute_node_passed(&rule, &all).is_passed());

        let partial = detail(
            "PENDING",
            None,
            vec![
                task("1", "n1", "AND", "APPROVED", "1000"),
                task("2", "n1", "AND", "PENDING", "1001"),
            ],
            vec![],
        );
        assert_eq!(
            compute_node_passed(&rule, &partial),
            TriggerOutcome::NotPassed {
                node_id: "n1".to_string()
            }
        );
    }

    #[test]
    fn or_node_passes_with_any_approval() {
        let rule = rule(TriggerMode::NodePassed, &["n1"], false);
        let detail = detail(
            "PENDING",
            None,
            vec![
                task("1", "n1", "OR", "APPROVED", "1000"),
                task("2", "n1", "OR", "PENDING", "1001"),
            ],
            vec![],
        );
        assert!(compute_node_passed(&rule, &detail).is_passed());
    }

    #[test]
    fn sequential_requires_all_approvals() {
        let rule = rule(TriggerMode::NodePassed, &["n1"], false);
        let partial = detail(
            "PENDING",
            None,
            vec![
                task("1", "n1", "SEQUENTIAL", "APPROVED", "1000"),
                task("2", "n1", "SEQUENTIAL", "PENDING", "1001"),
            ],
            vec![],
        );
        assert!(!compute_node_passed(&rule, &partial).is_passed());
        let all = detail(
            "PENDING",
            None,
            vec![
                task("1", "n1", "SEQUENTIAL", "APPROVED", "1000"),
                task("2", "n1", "SEQUENTIAL", "AUTO_PASS", "1001"),
            ],
            vec![],
        );
        assert!(compute_node_passed(&rule, &all).is_passed());
    }

    #[test]
    fn auto_pass_counts_as_approved() {
        let rule = rule(TriggerMode::NodePassed, &["n1"], false);
        let detail = detail(
            "PENDING",
            None,
            vec![
                task("1", "n1", "AND", "APPROVED", "1000"),
                task("2", "n1", "AND", "AUTO_PASS", "1001"),
            ],
            vec![],
        );
        assert!(compute_node_passed(&rule, &detail).is_passed());
    }

    #[test]
    fn old_done_tasks_before_a_rollback_are_ignored() {
        let rule = rule(TriggerMode::NodePassed, &["n1"], false);
        let tasks = vec![
            task("1", "n1", "AND", "DONE", "1000"),
            task("2", "n1", "AND", "APPROVED", "3000"),
        ];
        let rolled = detail("PENDING", None, tasks.clone(), vec![rollback("1", "2000")]);
        assert!(
            compute_node_passed(&rule, &rolled).is_passed(),
            "退回后的新 APPROVED 应被视为通过"
        );

        let no_rollback = detail("PENDING", None, tasks, vec![]);
        assert_eq!(
            compute_node_passed(&rule, &no_rollback),
            TriggerOutcome::NotPassed {
                node_id: "n1".to_string()
            },
            "DONE 本身不算通过"
        );
    }

    #[test]
    fn rollback_without_reapproval_is_rolled_back() {
        let rule = rule(TriggerMode::NodePassed, &["n1"], false);
        let detail = detail(
            "PENDING",
            None,
            vec![
                task("1", "n1", "AND", "DONE", "1000"),
                task("2", "n1", "AND", "PENDING", "3000"),
            ],
            vec![rollback("1", "2000")],
        );
        assert_eq!(
            compute_node_passed(&rule, &detail),
            TriggerOutcome::RolledBack {
                node_id: "n1".to_string()
            }
        );
    }

    #[test]
    fn require_final_approval_waits_for_the_instance_to_approve() {
        let rule = rule(TriggerMode::NodePassed, &["n1"], true);
        let tasks = vec![task("1", "n1", "AND", "APPROVED", "1000")];
        let pending = detail("PENDING", None, tasks.clone(), vec![]);
        assert_eq!(
            compute_node_passed(&rule, &pending),
            TriggerOutcome::WaitingFinal {
                node_id: "n1".to_string()
            }
        );
        let approved = detail("APPROVED", None, tasks.clone(), vec![]);
        assert!(compute_node_passed(&rule, &approved).is_passed());
        let rejected = detail("REJECTED", None, tasks, vec![]);
        assert_eq!(
            compute_node_passed(&rule, &rejected),
            TriggerOutcome::NotPassed {
                node_id: "n1".to_string()
            }
        );
    }

    #[test]
    fn final_approved_mode_uses_the_instance_state() {
        let rule = rule(TriggerMode::FinalApproved, &[], false);
        assert_eq!(
            compute_node_passed(&rule, &detail("APPROVED", None, vec![], vec![])),
            TriggerOutcome::Passed {
                node_id: FINAL_NODE_ID.to_string()
            }
        );
        assert_eq!(
            compute_node_passed(&rule, &detail("PENDING", None, vec![], vec![])),
            TriggerOutcome::WaitingFinal {
                node_id: FINAL_NODE_ID.to_string()
            }
        );
        assert_eq!(
            compute_node_passed(&rule, &detail("APPROVED", Some(true), vec![], vec![])),
            TriggerOutcome::NotPassed {
                node_id: FINAL_NODE_ID.to_string()
            },
            "撤销后即使 status 仍是 APPROVED 也不能算最终通过"
        );
    }

    #[test]
    fn multiple_trigger_nodes_trigger_on_the_first_passed_node() {
        let rule = rule(TriggerMode::NodePassed, &["n1", "n2"], false);
        let detail = detail(
            "PENDING",
            None,
            vec![
                task("1", "n1", "AND", "PENDING", "1000"),
                task("2", "n2", "AND", "APPROVED", "1001"),
            ],
            vec![],
        );
        assert_eq!(
            compute_node_passed(&rule, &detail),
            TriggerOutcome::Passed {
                node_id: "n2".to_string()
            }
        );
    }

    #[test]
    fn no_trigger_node_reached_is_not_reached() {
        let rule = rule(TriggerMode::NodePassed, &["n1"], false);
        assert_eq!(
            compute_node_passed(&rule, &detail("PENDING", None, vec![], vec![])),
            TriggerOutcome::NotReached
        );
    }
}

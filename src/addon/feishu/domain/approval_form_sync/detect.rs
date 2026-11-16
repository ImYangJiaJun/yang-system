//! 从实例详情判定「退回 → 同一实例内重新提交」。
//!
//! 飞书没有「重新提交」timeline 类型；可判定的证据是：
//! 1. `timeline` 出现 `ROLLBACK` / `ROLLBACK_SELECTED`；
//! 2. 被退回的任务/节点能证明是**发起人**（`START` 或发起人 open_id）；
//! 3. 退回之后 `task_list` 出现新任务（`start_time > 退回时间`），或实例已终态。
//!
//! 所有时间统一成毫秒；入站事件的 `operate_time` 存的是秒，不能直接混用。

use super::super::approval::{InstanceDetail, TaskNode, TimelineRecord};

/// 一次实例快照里的退回窗口。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct RollbackWindow {
    /// 最近一次「退回给发起人」的毫秒时间。
    pub(crate) last_initiator_rollback_ms: Option<i64>,
    /// 退回之后是否出现新任务（或实例已终态）。
    pub(crate) resubmitted: bool,
    /// 最近一次任务开始时间（毫秒）。
    pub(crate) last_task_start_ms: Option<i64>,
    /// 有退回，但目标不是发起人（例如退回给前一个审批人）。
    pub(crate) non_initiator_rollback: bool,
}

impl RollbackWindow {
    /// 只有「退回给发起人 + 已重提」才进入写回。
    pub(crate) fn should_sync(&self) -> bool {
        self.last_initiator_rollback_ms.is_some() && self.resubmitted
    }

    /// 已退回给发起人，但还没看到重提证据：等待，不要写。
    pub(crate) fn awaiting_resubmit(&self) -> bool {
        self.last_initiator_rollback_ms.is_some() && !self.resubmitted
    }
}

/// 解析实例详情，返回退回窗口。
pub(crate) fn inspect(detail: &InstanceDetail) -> RollbackWindow {
    let mut window = RollbackWindow::default();
    let tasks = detail.task_list.as_deref().unwrap_or_default();
    let timeline = detail.timeline.as_deref().unwrap_or_default();

    window.last_task_start_ms = tasks
        .iter()
        .filter_map(|task| task.start_time.as_deref().and_then(parse_millis))
        .max();

    let mut latest_initiator_rollback = None;
    for entry in timeline {
        if !is_rollback(entry) {
            continue;
        }
        let Some(rollback_ms) = entry.create_time.as_deref().and_then(parse_millis) else {
            continue;
        };
        if rollback_targets_initiator(entry, tasks, detail) {
            latest_initiator_rollback = Some(
                latest_initiator_rollback
                    .map_or(rollback_ms, |current: i64| current.max(rollback_ms)),
            );
        } else {
            window.non_initiator_rollback = true;
        }
    }
    window.last_initiator_rollback_ms = latest_initiator_rollback;

    let terminal = detail
        .status
        .as_deref()
        .is_some_and(|status| matches!(status, "APPROVED" | "REJECTED" | "CANCELED" | "DELETED"));
    window.resubmitted = latest_initiator_rollback.is_some_and(|rollback_ms| {
        terminal
            || window
                .last_task_start_ms
                .is_some_and(|start_ms| start_ms > rollback_ms)
    });
    window
}

fn is_rollback(entry: &TimelineRecord) -> bool {
    matches!(
        entry.r#type.as_deref().map(str::trim),
        Some("ROLLBACK" | "ROLLBACK_SELECTED")
    )
}

/// 退回目标是否为发起人。
///
/// 判据按可靠性排序：
/// 1. `timeline.task_id` 命中 `task_list`，该任务的 `open_id == detail.open_id`；
/// 2. 该任务的 `node_id` / `type` 是 `START`；
/// 3. `timeline.ext` 的 `rollback_node_ids` 含 `START`。
fn rollback_targets_initiator(
    entry: &TimelineRecord,
    tasks: &[TaskNode],
    detail: &InstanceDetail,
) -> bool {
    if let Some(target) = entry.task_id.as_deref().and_then(|task_id| {
        tasks
            .iter()
            .find(|task| task.id.as_deref() == Some(task_id))
    }) {
        if is_initiator_task(target, detail) {
            return true;
        }
    }
    if let Some(ext) = entry.ext.as_deref() {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(ext) {
            for key in ["rollback_node_ids", "rollback_custom_node_ids"] {
                if let Some(nodes) = value.get(key).and_then(serde_json::Value::as_array) {
                    if nodes.iter().any(|node| {
                        node.as_str()
                            .is_some_and(|node| node.trim().eq_ignore_ascii_case("START"))
                    }) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn is_initiator_task(task: &TaskNode, detail: &InstanceDetail) -> bool {
    if let (Some(task_open_id), Some(initiator)) =
        (task.open_id.as_deref(), detail.open_id.as_deref())
    {
        if !task_open_id.trim().is_empty() && task_open_id.trim() == initiator.trim() {
            return true;
        }
    }
    task.node_id
        .as_deref()
        .is_some_and(|node| node.trim().eq_ignore_ascii_case("START"))
        || task
            .r#type
            .as_deref()
            .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("START"))
}

/// 毫秒时间戳；兼容字符串/数字。
fn parse_millis(raw: &str) -> Option<i64> {
    raw.trim().parse::<i64>().ok().filter(|value| *value > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, open_id: &str, start: i64) -> TaskNode {
        TaskNode {
            id: Some(id.to_string()),
            node_id: Some("node-1".to_string()),
            node_name: None,
            user_id: None,
            open_id: Some(open_id.to_string()),
            status: Some("PENDING".to_string()),
            r#type: Some("AND".to_string()),
            start_time: Some(start.to_string()),
            end_time: Some("0".to_string()),
        }
    }

    fn rollback(task_id: &str, at: i64) -> TimelineRecord {
        TimelineRecord {
            r#type: Some("ROLLBACK".to_string()),
            create_time: Some(at.to_string()),
            user_id: None,
            open_id: None,
            ext: None,
            task_id: Some(task_id.to_string()),
        }
    }

    fn make_detail(
        tasks: Vec<TaskNode>,
        timeline: Vec<TimelineRecord>,
        status: &str,
    ) -> InstanceDetail {
        InstanceDetail {
            instance_code: "INST".into(),
            approval_code: None,
            serial_number: None,
            status: Some(status.into()),
            start_time: None,
            end_time: None,
            open_id: Some("ou_initiator".into()),
            user_id: None,
            department_id: None,
            department_name: None,
            uuid: None,
            reverted: None,
            task_list: Some(tasks),
            timeline: Some(timeline),
            form: None,
        }
    }

    #[test]
    fn initiator_rollback_with_new_task_triggers_sync() {
        let detail = make_detail(
            vec![
                task("t1", "ou_initiator", 1_000),
                task("t2", "ou_approver", 3_000),
            ],
            vec![rollback("t1", 2_000)],
            "PENDING",
        );
        let window = inspect(&detail);
        assert!(window.should_sync());
        assert_eq!(window.last_initiator_rollback_ms, Some(2_000));
        assert_eq!(window.last_task_start_ms, Some(3_000));
    }

    #[test]
    fn rollback_without_resubmit_awaits() {
        let detail = make_detail(
            vec![task("t1", "ou_initiator", 1_000)],
            vec![rollback("t1", 2_000)],
            "PENDING",
        );
        let window = inspect(&detail);
        assert!(!window.should_sync());
        assert!(window.awaiting_resubmit());
    }

    #[test]
    fn rollback_to_previous_approver_does_not_trigger() {
        let detail = make_detail(
            vec![
                task("t1", "ou_initiator", 1_000),
                task("t2", "ou_approver", 1_500),
            ],
            vec![rollback("t2", 2_000)],
            "PENDING",
        );
        let window = inspect(&detail);
        assert!(!window.should_sync());
        assert!(window.non_initiator_rollback);
    }

    #[test]
    fn start_node_or_ext_proves_initiator_target() {
        let mut start = task("t1", "", 1_000);
        start.node_id = Some("START".into());
        let detail = make_detail(
            vec![start, task("t2", "ou_approver", 3_000)],
            vec![rollback("t1", 2_000)],
            "PENDING",
        );
        assert!(inspect(&detail).should_sync());

        let mut ext_rollback = rollback("t9", 2_000);
        ext_rollback.ext = Some(r#"{"rollback_node_ids":["START"]}"#.into());
        let detail = make_detail(
            vec![task("t2", "ou_approver", 3_000)],
            vec![ext_rollback],
            "PENDING",
        );
        assert!(inspect(&detail).should_sync());
    }

    #[test]
    fn terminal_status_counts_as_resubmitted() {
        let detail = make_detail(
            vec![task("t1", "ou_initiator", 1_000)],
            vec![rollback("t1", 2_000)],
            "APPROVED",
        );
        assert!(inspect(&detail).should_sync());
    }

    #[test]
    fn invalid_or_zero_times_do_not_trigger() {
        let mut bad = rollback("t1", 0);
        bad.create_time = Some("not-a-time".into());
        let detail = make_detail(
            vec![task("t1", "ou_initiator", 1_000)],
            vec![bad],
            "PENDING",
        );
        let window = inspect(&detail);
        assert!(!window.should_sync());
        assert_eq!(window.last_initiator_rollback_ms, None);
    }
}

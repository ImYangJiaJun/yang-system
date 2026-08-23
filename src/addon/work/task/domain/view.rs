//! 任务树与任务清单两个正式 View。

use yang_base::definition::{
    ActionConfirmation, ActionInteraction, ActionPlacement, ActionPresentationSpec, SortDirection,
    TableSortSpec, TreeViewSpec, ViewName, ViewSpec,
};
use yang_base::BaseError;

pub(in crate::addon::work::task) fn build_all() -> Result<Vec<ViewSpec>, BaseError> {
    Ok(vec![outline()?, backlog()?])
}

fn outline() -> Result<ViewSpec, BaseError> {
    Ok(base(
        ViewName::new("outline").map_err(|error| BaseError::ConfigError(error.to_string()))?,
        "任务树",
    )
    .tree(
        TreeViewSpec::new(
            yang_base::field!("work_task.id"),
            yang_base::field!("work_task.parent_task"),
            yang_base::field!("work_task.title"),
        )
        .max_nodes(100),
    ))
}

fn backlog() -> Result<ViewSpec, BaseError> {
    Ok(base(
        ViewName::new("backlog").map_err(|error| BaseError::ConfigError(error.to_string()))?,
        "任务清单",
    ))
}

fn base(name: ViewName, title: &str) -> ViewSpec {
    ViewSpec::new(name)
        .title(title)
        .data_action(yang_base::action!("work.task.select"))
        .field(yang_base::field!("work_task.id"))
        .field(yang_base::field!("work_task.project_project"))
        .field(yang_base::field!("work_task.parent_task"))
        .field(yang_base::field!("work_task.title"))
        .field(yang_base::field!("work_task.status"))
        .field(yang_base::field!("work_task.priority"))
        .field(yang_base::field!("work_task.due_at"))
        .field(yang_base::field!("work_task.created_at"))
        .field(yang_base::field!("work_task.updated_at"))
        .default_sort(TableSortSpec::new(
            yang_base::field!("work_task.created_at"),
            SortDirection::Asc,
        ))
        .present_action(
            yang_base::action!("work.task.add"),
            ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Form),
        )
        .present_action(
            yang_base::action!("work.task.put"),
            ActionPresentationSpec::new(ActionPlacement::Row, ActionInteraction::Form)
                .record_parameter("id"),
        )
        .present_action(
            yang_base::action!("work.task.del"),
            ActionPresentationSpec::new(ActionPlacement::Row, ActionInteraction::Invoke)
                .record_parameter("id")
                .confirmation(ActionConfirmation::new(
                    "确认删除任务",
                    "含子任务的节点会被数据库拒绝删除",
                )),
        )
        .present_action(
            yang_base::action!("work.task.complete"),
            ActionPresentationSpec::new(ActionPlacement::Bulk, ActionInteraction::Invoke)
                .confirmation(ActionConfirmation::new(
                    "批量完成任务",
                    "只会更新当前工作区内已选择的任务",
                )),
        )
}

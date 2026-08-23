//! 个人项目列表 View。

use yang_base::definition::{
    ActionConfirmation, ActionInteraction, ActionPlacement, ActionPresentationSpec, SortDirection,
    TableSortSpec, ViewName, ViewSpec,
};
use yang_base::BaseError;

pub(in crate::addon::work::project) fn build() -> Result<ViewSpec, BaseError> {
    let name =
        ViewName::new("portfolio").map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(ViewSpec::new(name)
        .title("项目组合")
        .data_action(yang_base::action!("work.project.select"))
        .field(yang_base::field!("work_project.id"))
        .field(yang_base::field!("work_project.name"))
        .field(yang_base::field!("work_project.status"))
        .field(yang_base::field!("work_project.created_at"))
        .field(yang_base::field!("work_project.updated_at"))
        .default_sort(TableSortSpec::new(
            yang_base::field!("work_project.updated_at"),
            SortDirection::Desc,
        ))
        .present_action(
            yang_base::action!("work.project.add"),
            ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Form),
        )
        .present_action(
            yang_base::action!("work.project.put"),
            ActionPresentationSpec::new(ActionPlacement::Row, ActionInteraction::Form)
                .record_parameter("id"),
        )
        .present_action(
            yang_base::action!("work.project.del"),
            ActionPresentationSpec::new(ActionPlacement::Row, ActionInteraction::Invoke)
                .record_parameter("id")
                .confirmation(ActionConfirmation::new(
                    "确认删除项目",
                    "仅没有任务的项目可删除，此操作无法撤销",
                )),
        ))
}

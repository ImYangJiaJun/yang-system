//! 验收项目的 UI View 契约。

use anyhow::Context;
use yang_base::definition::{
    ActionConfirmation, ActionInteraction, ActionPlacement, ActionPresentationSpec, SortDirection,
    TableSortSpec, TreeViewSpec, ViewName, ViewSpec,
};

pub(super) fn item_view() -> anyhow::Result<ViewSpec> {
    let confirm = ActionConfirmation::new("确认删除项目", "此操作无法撤销");
    Ok(
        ViewSpec::new(ViewName::new("main").context("View 名称无效")?)
            .title("项目目录")
            .data_action(yang_base::action!("demo.items.list"))
            .field(yang_base::field!("demo_items.id"))
            .field(yang_base::field!("demo_items.name"))
            .field(yang_base::field!("demo_items.category_id"))
            .field(yang_base::field!("demo_items.status"))
            .field(yang_base::field!("demo_items.parent_id"))
            .default_sort(TableSortSpec::new(
                yang_base::field!("demo_items.name"),
                SortDirection::Asc,
            ))
            .tree(
                TreeViewSpec::new(
                    yang_base::field!("demo_items.id"),
                    yang_base::field!("demo_items.parent_id"),
                    yang_base::field!("demo_items.name"),
                )
                .max_nodes(100),
            )
            .present_action(
                yang_base::action!("demo.items.add"),
                ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Form),
            )
            .present_action(
                yang_base::action!("demo.items.edit"),
                ActionPresentationSpec::new(ActionPlacement::Row, ActionInteraction::Form),
            )
            .present_action(
                yang_base::action!("demo.items.delete"),
                ActionPresentationSpec::new(ActionPlacement::Row, ActionInteraction::Invoke)
                    .confirmation(confirm),
            )
            .present_action(
                yang_base::action!("demo.items.insight"),
                ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Custom)
                    .view_id("demo.items.insight"),
            ),
    )
}

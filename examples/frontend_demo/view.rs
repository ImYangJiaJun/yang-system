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
                ActionPresentationSpec::new(ActionPlacement::Row, ActionInteraction::Form)
                    .record_parameter("id"),
            )
            .present_action(
                yang_base::action!("demo.items.delete"),
                ActionPresentationSpec::new(ActionPlacement::Row, ActionInteraction::Invoke)
                    .record_parameter("id")
                    .confirmation(confirm),
            )
            .present_action(
                yang_base::action!("demo.items.insight"),
                ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Custom)
                    .view_id("demo.items.insight"),
            )
            .present_action(
                yang_base::action!("demo.items.bulk_delete"),
                ActionPresentationSpec::new(ActionPlacement::Bulk, ActionInteraction::Invoke)
                    .confirmation(ActionConfirmation::new(
                        "确认批量删除",
                        "将删除所有选中项目，此操作无法撤销",
                    )),
            )
            .present_action(
                yang_base::action!("demo.api.download"),
                ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Download),
            )
            .present_action(
                yang_base::action!("demo.api.preview"),
                ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Preview),
            )
            .present_action(
                yang_base::action!("demo.api.redirect"),
                ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Navigate),
            )
            .present_action(
                yang_base::action!("demo.api.upload"),
                ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Form),
            ),
    )
}

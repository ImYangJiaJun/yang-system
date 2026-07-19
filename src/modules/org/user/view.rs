//! 企业成员列表的 UI View 契约。

use yang_base::definition::{
    ActionConfirmation, ActionInteraction, ActionPlacement, ActionPresentationSpec, ViewName,
    ViewSpec,
};
use yang_base::BaseError;

pub(super) fn build() -> Result<ViewSpec, BaseError> {
    let name = ViewName::new("list").map_err(|error| BaseError::ConfigError(error.to_string()))?;
    let confirm_delete = ActionConfirmation::new("确认删除成员", "删除后该用户将失去企业访问权");
    Ok(ViewSpec::new(name)
        .data_action(yang_base::action!("org.user.select"))
        .field(yang_base::field!("org_user.id"))
        .field(yang_base::field!("org_user.org_org"))
        .field(yang_base::field!("org_user.user_user"))
        .field(yang_base::field!("org_user.created_at"))
        .present_action(
            yang_base::action!("org.user.add"),
            ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Form),
        )
        .present_action(
            yang_base::action!("org.user.put"),
            ActionPresentationSpec::new(ActionPlacement::Row, ActionInteraction::Form),
        )
        .present_action(
            yang_base::action!("org.user.del"),
            ActionPresentationSpec::new(ActionPlacement::Row, ActionInteraction::Invoke)
                .confirmation(confirm_delete),
        ))
}

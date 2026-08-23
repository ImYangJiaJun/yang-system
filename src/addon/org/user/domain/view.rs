//! 企业成员列表的 UI View 契约。

use yang_base::definition::{
    ActionConfirmation, ActionInteraction, ActionPlacement, ActionPresentationSpec, ViewName,
    ViewSpec,
};
use yang_base::BaseError;

pub(in crate::addon::org::user) fn build() -> Result<ViewSpec, BaseError> {
    let name = ViewName::new("list").map_err(|error| BaseError::ConfigError(error.to_string()))?;
    let confirm_delete = ActionConfirmation::new("确认删除成员", "删除后该用户将失去企业访问权");
    Ok(ViewSpec::new(name)
        .data_action(yang_base::action!("org.user.select"))
        .field(yang_base::field!("org_user.id"))
        .field(yang_base::field!("org_user.org_org"))
        .field(yang_base::field!("org_user.user_user"))
        .field(yang_base::field!("org_user.name"))
        .field(yang_base::field!("org_user.position"))
        .field(yang_base::field!("org_user.email"))
        .field(yang_base::field!("org_user.phone"))
        .field(yang_base::field!("org_user.admin"))
        .field(yang_base::field!("org_user.status"))
        .field(yang_base::field!("org_user.created_at"))
        .field(yang_base::field!("org_user.updated_at"))
        .present_action(
            yang_base::action!("org.user.add"),
            ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Form),
        )
        .present_action(
            yang_base::action!("org.user.put"),
            ActionPresentationSpec::new(ActionPlacement::Row, ActionInteraction::Form)
                .record_parameter("id"),
        )
        .present_action(
            yang_base::action!("org.user.del"),
            ActionPresentationSpec::new(ActionPlacement::Row, ActionInteraction::Invoke)
                .record_parameter("id")
                .confirmation(confirm_delete),
        ))
}

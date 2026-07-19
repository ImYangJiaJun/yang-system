//! 企业 Action 清单。

mod list;
mod select;
mod support;

use list::OrgListAction;
use select::OrgSelectAction;
use yang_base::definition::Actions;

pub(super) fn all() -> Actions {
    yang_base::actions![OrgListAction, OrgSelectAction]
}

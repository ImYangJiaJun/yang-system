//! 各业务 Module 复用的账户身份展示声明。

use yang_base::definition::AccountIdentitySpec;

pub(crate) fn user_identity() -> AccountIdentitySpec {
    AccountIdentitySpec::new("user", "个人账户", "person").order(10)
}

pub(crate) fn organization_identity() -> AccountIdentitySpec {
    AccountIdentitySpec::new("org", "企业账号", "organization").order(20)
}

pub(crate) fn administrator_identity() -> AccountIdentitySpec {
    AccountIdentitySpec::new("admin", "管理平台", "administrator").order(30)
}

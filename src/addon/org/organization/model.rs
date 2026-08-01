//! 企业对外读模型。

use schemars::JsonSchema;
use serde::Serialize;
use yang_base::table::Record;
use yang_base::BaseError;

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct OrganizationView {
    pub(super) id: i64,
    pub(super) name: String,
    pub(super) code: String,
    pub(super) status: String,
    pub(super) created_at: i64,
}

impl TryFrom<&Record> for OrganizationView {
    type Error = BaseError;

    fn try_from(record: &Record) -> Result<Self, Self::Error> {
        Ok(Self {
            id: record.require("id")?,
            name: record.require("name")?,
            code: record.require("code")?,
            status: record.require("status")?,
            created_at: record.require("created_at")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn organization_view_requires_the_declared_contract() {
        let record = Record::new()
            .set("id", 7)
            .set("name", "Acme")
            .set("code", "ACME")
            .set("status", "active")
            .set("created_at", 10);
        let view = OrganizationView::try_from(&record)
            .unwrap_or_else(|error| panic!("完整企业记录应可转换: {error}"));
        assert_eq!(view.id, 7);
        assert!(OrganizationView::try_from(&Record::new().set("id", 7)).is_err());
    }
}

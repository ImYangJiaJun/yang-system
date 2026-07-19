//! 验收服务的 Schema Module。

use yang_base::definition::{Fields, Int, Key, Module, ModuleName, ModuleSpec, Str, Table};

struct DemoCategoryModule;

impl Module for DemoCategoryModule {
    fn name(&self) -> ModuleName {
        yang_base::module!("demo.category")
    }

    fn table(&self) -> Option<yang_base::definition::TableName> {
        Some(yang_base::table!("demo_category"))
    }

    fn fields(&self) -> Fields {
        yang_base::fields! {
            id => Key::new().title("ID"),
            name => Str::new().title("分类名称").require(true),
        }
    }
}

struct DemoItemModule;

impl Module for DemoItemModule {
    fn name(&self) -> ModuleName {
        yang_base::module!("demo.items")
    }

    fn table(&self) -> Option<yang_base::definition::TableName> {
        Some(yang_base::table!("demo_items"))
    }

    fn fields(&self) -> Fields {
        yang_base::fields! {
            id => Key::new().title("ID"),
            name => Str::new()
                .title("名称")
                .require(true)
                .searchable(true)
                .filterable(true)
                .sortable(true),
            category_id => Table::new()
                .title("分类")
                .require(true)
                .target(yang_base::field!("demo_category.id"))
                .display([yang_base::field!("demo_category.name")])
                .select(yang_base::action!("demo.category.options")),
            status => Str::new()
                .title("状态")
                .require(true)
                .filterable(true)
                .sortable(true),
            parent_id => Int::new().title("父节点"),
        }
    }
}

pub(super) fn category() -> ModuleSpec {
    DemoCategoryModule.into_spec()
}

pub(super) fn items() -> ModuleSpec {
    DemoItemModule.into_spec()
}

//! 前端诊断 Action 清单。

mod report;

use yang_base::definition::ModuleSpec;

pub(super) fn register_all(module: ModuleSpec) -> ModuleSpec {
    report::register(module)
}

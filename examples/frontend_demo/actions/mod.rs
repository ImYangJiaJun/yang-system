//! 验收 Action 路由表；每个 Action 的定义与实现独占一个文件。

mod add;
mod category_options;
mod delete;
mod download;
mod echo;
mod edit;
mod insight;
mod list;
mod preview;
mod redirect;
mod upload;

use super::model::{DemoItems, NoInput};
use std::path::PathBuf;
use std::sync::Arc;
use yang_base::action::{ActionContext, UiCatalogAction};
use yang_base::definition::{
    ActionName, ActionResponseKind, HttpMethod, ModuleSpec, MultipartSpec,
};
use yang_base::BaseError;

fn action_name(value: &str) -> Result<ActionName, BaseError> {
    ActionName::new(value).map_err(|error| BaseError::ConfigError(error.to_string()))
}

pub(super) fn register_api(module: ModuleSpec, fixture: PathBuf) -> Result<ModuleSpec, BaseError> {
    let module = module.native_action(UiCatalogAction);
    let module = module
        .action_fn(action_name("echo")?, echo::handle)
        .route(HttpMethod::Post, "/api/v1/demo/echo")
        .display_name("回显输入")
        .description("用于验收默认 ActionDemo 的真实 HTTP 调用")
        .public()
        .register();
    let module = module
        .action_fn(action_name("upload")?, upload::handle)
        .route(HttpMethod::Post, "/api/v1/demo/upload")
        .display_name("上传验收文件")
        .description("验证受限 multipart 表单与请求作用域文件")
        .public()
        .multipart(
            MultipartSpec::new(["text/plain"])
                .max_fields(1)
                .max_files(1)
                .max_file_bytes(1024)
                .max_total_bytes(131072),
        )
        .register();
    let module = module
        .action_fn(action_name("download")?, {
            let path = fixture.clone();
            move |ctx: ActionContext, input: NoInput| download::handle(ctx, input, path.clone())
        })
        .route(HttpMethod::Get, "/api/v1/demo/download")
        .display_name("下载验收文件")
        .description("验证附件下载不会被 JSON 解析")
        .public()
        .response_kind(ActionResponseKind::Download)
        .register();
    let module = module
        .action_fn(action_name("preview")?, {
            move |ctx: ActionContext, input: NoInput| preview::handle(ctx, input, fixture.clone())
        })
        .route(HttpMethod::Get, "/api/v1/demo/preview")
        .display_name("预览验收文件")
        .description("验证浏览器内联预览通道")
        .public()
        .response_kind(ActionResponseKind::Preview)
        .register();
    let module = module
        .action_fn(action_name("redirect")?, redirect::handle)
        .route(HttpMethod::Get, "/api/v1/demo/redirect")
        .display_name("重定向验收")
        .description("验证前端展示 Location 而不是静默跳走")
        .public()
        .response_kind(ActionResponseKind::Redirect)
        .register();
    // scaffold:action-registration
    Ok(module)
}

pub(super) fn register_category(module: ModuleSpec) -> Result<ModuleSpec, BaseError> {
    let module = module
        .action_fn(action_name("options")?, category_options::handle)
        .route(HttpMethod::Post, "/api/v1/demo/categories/options")
        .display_name("分类选项")
        .description("通用关系选择器 options")
        .public()
        .register();
    Ok(module)
}

pub(super) fn register_items(
    module: ModuleSpec,
    items: DemoItems,
) -> Result<ModuleSpec, BaseError> {
    let module = module
        .action_fn(action_name("list")?, {
            let items = Arc::clone(&items);
            move |ctx: ActionContext, input: list::ListInput| {
                list::handle(ctx, input, Arc::clone(&items))
            }
        })
        .route(HttpMethod::Post, "/api/v1/demo/items/query")
        .display_name("项目列表数据")
        .description("为通用 TableView 提供标准分页数据")
        .public()
        .register();
    let module = module
        .action_fn(action_name("add")?, {
            let items = Arc::clone(&items);
            move |ctx: ActionContext, input: add::AddInput| {
                add::handle(ctx, input, Arc::clone(&items))
            }
        })
        .route(HttpMethod::Post, "/api/v1/demo/items")
        .display_name("新增项目")
        .description("通用表单新增演示")
        .public()
        .register();
    let module = module
        .action_fn(action_name("edit")?, {
            let items = Arc::clone(&items);
            move |ctx: ActionContext, input: edit::EditInput| {
                edit::handle(ctx, input, Arc::clone(&items))
            }
        })
        .route(HttpMethod::Put, "/api/v1/demo/items")
        .display_name("编辑项目")
        .description("通用行表单编辑演示")
        .public()
        .register();
    let module = module
        .action_fn(action_name("delete")?, {
            let items = Arc::clone(&items);
            move |ctx: ActionContext, input: delete::DeleteInput| {
                delete::handle(ctx, input, Arc::clone(&items))
            }
        })
        .route(HttpMethod::Delete, "/api/v1/demo/items")
        .display_name("删除项目")
        .description("通用确认调用演示")
        .public()
        .register();
    let module = module
        .action_fn(action_name("insight")?, {
            let items = Arc::clone(&items);
            move |ctx: ActionContext, input: NoInput| {
                insight::handle(ctx, input, Arc::clone(&items))
            }
        })
        .route(HttpMethod::Get, "/api/v1/demo/items/insight")
        .display_name("项目洞察")
        .description("展示静态 view_id 自定义页面覆盖")
        .public()
        .register();
    Ok(module)
}

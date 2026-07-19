//! 新前端验收专用的无数据库 YANG HTTP 服务。

use anyhow::Context;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use yang_base::action::{
    Action as ActionHandler, ActionContext, ResponseBody, UiCatalogAction, UploadedFile,
};
use yang_base::definition::{
    ActionConfirmation, ActionInteraction, ActionPlacement, ActionPresentationSpec, AddonName,
    AddonSpec, AppBuilder, Fields, Int, Key, Module, ModuleName, ModuleSpec, ParamInput, Params,
    SortDirection, Str, Table, TableSortSpec, TreeViewSpec, ViewName, ViewSpec,
};
use yang_base::table::{RelationOption, RelationOptionsRequest, RelationOptionsResponse};
use yang_base::tools::ToolsBuilder;
use yang_base::transport::axum::{serve, AxumTransportConfig};
use yang_base::{Action, BaseError};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoInput {}

impl ParamInput for NoInput {
    fn params() -> Params {
        Params::new()
    }
}

yang_base::params! {
    #[deny_unknown_fields]
    EchoInput {
        message: Str::new()
            .title("消息")
            .description("服务端会原样返回该文本")
            .require(true)
            .min_length(1)
            .max_length(200),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
struct EchoOutput {
    message: String,
    length: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DemoUploadInput {
    title: String,
    file: UploadedFile,
}

impl ParamInput for DemoUploadInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(Debug, Serialize, JsonSchema)]
struct DemoUploadOutput {
    title: String,
    filename: String,
    content_type: String,
    size: u64,
    content: String,
}

#[derive(Debug, Action)]
#[action(
    name = "upload",
    display_name = "上传验收文件",
    description = "验证受限 multipart 表单与请求作用域文件",
    method = "POST",
    path = "/api/demo/upload",
    public,
    request_media = "multipart",
    content_types("text/plain"),
    max_fields = 1,
    max_files = 1,
    max_file_bytes = 1024,
    max_total_bytes = 131072
)]
struct DemoUploadAction;

#[async_trait]
impl ActionHandler for DemoUploadAction {
    type Input = DemoUploadInput;
    type Output = DemoUploadOutput;

    async fn index(
        &self,
        _context: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let content = tokio::fs::read_to_string(input.file.path()).await?;
        Ok(DemoUploadOutput {
            title: input.title,
            filename: input.file.original_filename().to_string(),
            content_type: input.file.content_type().to_string(),
            size: input.file.size(),
            content,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
struct DemoItem {
    id: i64,
    name: String,
    category_id: i64,
    status: String,
    parent_id: Option<i64>,
}

type DemoItems = Arc<tokio::sync::RwLock<Vec<DemoItem>>>;

fn default_page() -> usize {
    1
}

fn default_page_size() -> usize {
    20
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DemoOrder {
    field: String,
    direction: SortDirection,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DemoListInput {
    #[serde(default = "default_page")]
    page: usize,
    #[serde(default = "default_page_size")]
    page_size: usize,
    #[serde(default)]
    search: Option<String>,
    #[serde(rename = "where", default)]
    where_clause: Option<yang_base::table::WhereCondition>,
    #[serde(default)]
    order_by: Vec<DemoOrder>,
    #[serde(default)]
    count_total: bool,
}

impl ParamInput for DemoListInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(Debug, Serialize, JsonSchema)]
struct DemoListOutput {
    items: Vec<DemoItem>,
    page: usize,
    page_size: usize,
    total: Option<usize>,
}

#[derive(Debug, Action)]
#[action(
    name = "list",
    display_name = "项目列表数据",
    description = "为通用 TableView 提供标准分页数据",
    method = "POST",
    path = "/api/demo/items/query",
    public
)]
struct DemoListAction {
    items: DemoItems,
}

fn item_value(item: &DemoItem, field: &str) -> Option<Value> {
    match field {
        "id" => Some(json!(item.id)),
        "name" => Some(json!(item.name)),
        "category_id" => Some(json!(item.category_id)),
        "status" => Some(json!(item.status)),
        "parent_id" => Some(json!(item.parent_id)),
        _ => None,
    }
}

fn matches_condition(item: &DemoItem, condition: &yang_base::table::WhereCondition) -> bool {
    use yang_base::table::WhereCondition;
    match condition {
        WhereCondition::Eq { field, value } => item_value(item, field).as_ref() == Some(value),
        WhereCondition::And { conditions } => conditions
            .iter()
            .all(|condition| matches_condition(item, condition)),
        WhereCondition::Or { conditions } => conditions
            .iter()
            .any(|condition| matches_condition(item, condition)),
        _ => false,
    }
}

#[async_trait]
impl ActionHandler for DemoListAction {
    type Input = DemoListInput;
    type Output = DemoListOutput;

    async fn index(
        &self,
        _context: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        if input.page == 0 || input.page_size == 0 || input.page_size > 100 {
            return Err(BaseError::ParamInvalid(
                "page/page_size".to_string(),
                "page>=1, 1<=page_size<=100".to_string(),
            ));
        }
        let mut items = self
            .items
            .read()
            .await
            .iter()
            .filter(|item| match input.search.as_ref() {
                Some(search) => item
                    .name
                    .to_lowercase()
                    .contains(&search.trim().to_lowercase()),
                None => true,
            })
            .filter(|item| match input.where_clause.as_ref() {
                Some(condition) => matches_condition(item, condition),
                None => true,
            })
            .cloned()
            .collect::<Vec<_>>();
        for order in input.order_by.iter().rev() {
            items.sort_by(|left, right| {
                let ordering = match order.field.as_str() {
                    "name" => left.name.cmp(&right.name),
                    "status" => left.status.cmp(&right.status),
                    "id" => left.id.cmp(&right.id),
                    _ => std::cmp::Ordering::Equal,
                };
                if order.direction == SortDirection::Desc {
                    ordering.reverse()
                } else {
                    ordering
                }
            });
        }
        let total = items.len();
        let start = input.page.saturating_sub(1).saturating_mul(input.page_size);
        let items = items
            .into_iter()
            .skip(start)
            .take(input.page_size)
            .collect();
        Ok(DemoListOutput {
            items,
            page: input.page,
            page_size: input.page_size,
            total: input.count_total.then_some(total),
        })
    }
}

yang_base::params! {
    #[deny_unknown_fields]
    DemoAddInput {
        name: Str::new().title("名称").require(true).max_length(100),
        category_id: Int::new().title("分类").require(true),
        status: Str::new().title("状态").require(true).max_length(20),
        parent_id: Int::new().title("父节点"),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
struct MutationOutput {
    id: i64,
}

#[derive(Debug, Action)]
#[action(
    name = "add",
    display_name = "新增项目",
    description = "通用表单新增演示",
    method = "POST",
    path = "/api/demo/items",
    public
)]
struct DemoAddAction {
    items: DemoItems,
}

#[async_trait]
impl ActionHandler for DemoAddAction {
    type Input = DemoAddInput;
    type Output = MutationOutput;

    async fn index(
        &self,
        _context: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let mut items = self.items.write().await;
        let id = items.iter().map(|item| item.id).max().unwrap_or(0) + 1;
        items.push(DemoItem {
            id,
            name: input.name,
            category_id: input.category_id,
            status: input.status,
            parent_id: input.parent_id,
        });
        Ok(MutationOutput { id })
    }
}

yang_base::params! {
    #[deny_unknown_fields]
    DemoEditInput {
        id: Int::new().title("ID").require(true),
        name: Str::new().title("名称").require(true).max_length(100),
        category_id: Int::new().title("分类").require(true),
        status: Str::new().title("状态").require(true).max_length(20),
        parent_id: Int::new().title("父节点"),
    }
}

#[derive(Debug, Action)]
#[action(
    name = "edit",
    display_name = "编辑项目",
    description = "通用行表单编辑演示",
    method = "PUT",
    path = "/api/demo/items",
    public
)]
struct DemoEditAction {
    items: DemoItems,
}

#[async_trait]
impl ActionHandler for DemoEditAction {
    type Input = DemoEditInput;
    type Output = MutationOutput;

    async fn index(
        &self,
        _context: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let mut items = self.items.write().await;
        let item = items
            .iter_mut()
            .find(|item| item.id == input.id)
            .ok_or_else(|| BaseError::RecordNotFound(format!("项目 {} 不存在", input.id)))?;
        item.name = input.name;
        item.category_id = input.category_id;
        item.status = input.status;
        item.parent_id = input.parent_id;
        Ok(MutationOutput { id: input.id })
    }
}

yang_base::params! {
    #[deny_unknown_fields]
    DemoDeleteInput {
        id: Int::new().title("ID").require(true),
    }
}

#[derive(Debug, Action)]
#[action(
    name = "delete",
    display_name = "删除项目",
    description = "通用确认调用演示",
    method = "DELETE",
    path = "/api/demo/items",
    public
)]
struct DemoDeleteAction {
    items: DemoItems,
}

#[derive(Debug, Serialize, JsonSchema)]
struct DemoInsightOutput {
    total: usize,
    active: usize,
    draft: usize,
}

#[derive(Debug, Action)]
#[action(
    name = "insight",
    display_name = "项目洞察",
    description = "展示静态 view_id 自定义页面覆盖",
    method = "GET",
    path = "/api/demo/items/insight",
    public
)]
struct DemoInsightAction {
    items: DemoItems,
}

#[async_trait]
impl ActionHandler for DemoInsightAction {
    type Input = NoInput;
    type Output = DemoInsightOutput;

    async fn index(
        &self,
        _context: ActionContext,
        _input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let items = self.items.read().await;
        Ok(DemoInsightOutput {
            total: items.len(),
            active: items.iter().filter(|item| item.status == "active").count(),
            draft: items.iter().filter(|item| item.status == "draft").count(),
        })
    }
}

#[async_trait]
impl ActionHandler for DemoDeleteAction {
    type Input = DemoDeleteInput;
    type Output = MutationOutput;

    async fn index(
        &self,
        _context: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let mut items = self.items.write().await;
        let before = items.len();
        items.retain(|item| item.id != input.id);
        if items.len() == before {
            return Err(BaseError::RecordNotFound(format!(
                "项目 {} 不存在",
                input.id
            )));
        }
        Ok(MutationOutput { id: input.id })
    }
}

#[derive(Debug, Action)]
#[action(
    name = "options",
    display_name = "分类选项",
    description = "通用关系选择器 options",
    method = "POST",
    path = "/api/demo/categories/options",
    public
)]
struct CategoryOptionsAction;

#[async_trait]
impl ActionHandler for CategoryOptionsAction {
    type Input = RelationOptionsRequest;
    type Output = RelationOptionsResponse;

    async fn index(
        &self,
        _context: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let all = [(1_i64, "平台"), (2, "业务"), (3, "实验")];
        let search = input.search.as_deref().unwrap_or_default().trim();
        let mut items = all
            .into_iter()
            .filter(|(value, label)| {
                search.is_empty()
                    || label.contains(search)
                    || input
                        .selected
                        .iter()
                        .any(|selected| selected == &json!(value))
            })
            .map(|(value, label)| RelationOption {
                value: json!(value),
                label: label.to_string(),
            })
            .collect::<Vec<_>>();
        let total = items.len() as u64;
        items.truncate(input.limit);
        Ok(RelationOptionsResponse {
            items,
            page: input.page,
            limit: input.limit,
            total: Some(total),
        })
    }
}

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

fn demo_item_view() -> anyhow::Result<ViewSpec> {
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

#[derive(Debug, Action)]
#[action(
    name = "echo",
    display_name = "回显输入",
    description = "用于验收默认 ActionDemo 的真实 HTTP 调用",
    method = "POST",
    path = "/api/demo/echo",
    public
)]
struct EchoAction;

#[async_trait]
impl ActionHandler for EchoAction {
    type Input = EchoInput;
    type Output = EchoOutput;

    async fn index(
        &self,
        _context: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        Ok(EchoOutput {
            length: input.message.chars().count(),
            message: input.message,
        })
    }
}

#[derive(Debug, Action)]
#[action(
    name = "download",
    display_name = "下载验收文件",
    description = "验证附件下载不会被 JSON 解析",
    method = "GET",
    path = "/api/demo/download",
    response_kind = "download",
    public
)]
struct DownloadAction {
    path: std::path::PathBuf,
}

#[async_trait]
impl ActionHandler for DownloadAction {
    type Input = NoInput;
    type Output = ResponseBody;

    async fn index(
        &self,
        _context: ActionContext,
        _input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        Ok(ResponseBody::download(self.path.clone(), "验收报告.txt"))
    }
}

#[derive(Debug, Action)]
#[action(
    name = "preview",
    display_name = "预览验收文件",
    description = "验证浏览器内联预览通道",
    method = "GET",
    path = "/api/demo/preview",
    response_kind = "preview",
    public
)]
struct PreviewAction {
    path: std::path::PathBuf,
}

#[async_trait]
impl ActionHandler for PreviewAction {
    type Input = NoInput;
    type Output = ResponseBody;

    async fn index(
        &self,
        _context: ActionContext,
        _input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        Ok(ResponseBody::preview(self.path.clone()))
    }
}

#[derive(Debug, Action)]
#[action(
    name = "redirect",
    display_name = "重定向验收",
    description = "验证前端展示 Location 而不是静默跳走",
    method = "GET",
    path = "/api/demo/redirect",
    response_kind = "redirect",
    public
)]
struct RedirectAction;

#[async_trait]
impl ActionHandler for RedirectAction {
    type Input = NoInput;
    type Output = ResponseBody;

    async fn index(
        &self,
        _context: ActionContext,
        _input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        Ok(ResponseBody::redirect("/.well-known/yang/ui-catalog"))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bind = std::env::var("YANG_DEMO_BIND")
        .unwrap_or_else(|_| "127.0.0.1:18080".to_string())
        .parse::<SocketAddr>()
        .context("YANG_DEMO_BIND 必须是有效 SocketAddr")?;
    let tools = Arc::new(ToolsBuilder::new().build().context("构建空 Tools 失败")?);
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("frontend/e2e/fixtures/report.txt");
    let items = Arc::new(tokio::sync::RwLock::new(vec![
        DemoItem {
            id: 1,
            name: "平台能力".to_string(),
            category_id: 1,
            status: "active".to_string(),
            parent_id: None,
        },
        DemoItem {
            id: 2,
            name: "通用渲染器".to_string(),
            category_id: 2,
            status: "draft".to_string(),
            parent_id: Some(1),
        },
    ]));
    let module = ModuleSpec::new(ModuleName::new("demo.api").context("Module 名称无效")?)
        .native_action(UiCatalogAction)
        .native_action(EchoAction)
        .native_action(DemoUploadAction)
        .native_action(DownloadAction {
            path: fixture.clone(),
        })
        .native_action(PreviewAction { path: fixture })
        .native_action(RedirectAction);
    let category = DemoCategoryModule
        .into_spec()
        .native_action(CategoryOptionsAction);
    let item_module = DemoItemModule
        .into_spec()
        .native_action(DemoListAction {
            items: Arc::clone(&items),
        })
        .native_action(DemoAddAction {
            items: Arc::clone(&items),
        })
        .native_action(DemoEditAction {
            items: Arc::clone(&items),
        })
        .native_action(DemoDeleteAction {
            items: Arc::clone(&items),
        })
        .native_action(DemoInsightAction { items })
        .view(demo_item_view()?);
    let app = AppBuilder::new()
        .addon(
            AddonSpec::new(AddonName::new("demo").context("Addon 名称无效")?)
                .module(module)
                .module(category)
                .module(item_module),
        )
        .build(tools)
        .context("构建前端验收应用失败")?;
    serve(bind, Arc::new(app), AxumTransportConfig::default())
        .await
        .context("运行前端验收 HTTP 服务失败")
}

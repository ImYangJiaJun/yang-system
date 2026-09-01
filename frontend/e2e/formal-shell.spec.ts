import { expect, test, type Page } from "@playwright/test";

async function restoreSession(page: Page, identity: "user") {
  await page.addInitScript((selectedIdentity) => {
    sessionStorage.setItem("yang.account-identity", selectedIdentity);
  }, identity);
  await page.route("**/api/v1/users/refresh", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: { access_token: "formal-shell-token" },
      }),
      headers: {
        "Set-Cookie":
          "yang_refresh=formal-shell-refresh; Path=/api/v1/users; HttpOnly; SameSite=Strict",
      },
    }),
  );
}

async function serveBusinessCatalog(page: Page) {
  await page.route("**/.well-known/yang/ui-catalog", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          schema_version: "2.3",
          revision: "b".repeat(64),
          actions: [
            {
              operation_id: "demo.items.list",
              title: "查询项目",
              description: "分页查询项目",
              method: "POST",
              path: "/api/v1/demo/items/query",
              params: [],
              input_schema: { type: "object" },
              output_schema: { type: "object" },
              request_media_type: "json",
              response_kind: "json",
              requires_auth: false,
            },
            {
              operation_id: "demo.items.add",
              title: "新增项目",
              description: "创建项目",
              method: "POST",
              path: "/api/v1/demo/items",
              params: [],
              input_schema: { type: "object" },
              output_schema: { type: "object" },
              request_media_type: "json",
              response_kind: "json",
              requires_auth: false,
            },
          ],
          table_views: [
            {
              view_id: "demo.items.main",
              title: "项目目录",
              table: "demo_items",
              data_action: "demo.items.list",
              columns: [
                {
                  field: "name",
                  title: "名称",
                  description: "项目名称",
                  widget: "text",
                  required: true,
                  searchable: true,
                  filterable: true,
                  sortable: true,
                },
              ],
              form: { fields: [] },
              query: {
                search_fields: ["name"],
                filter_fields: ["name"],
                default_sort: [],
                default_page_size: 20,
                max_page_size: 100,
              },
              actions: ["demo.items.add"],
              action_presentations: [
                {
                  operation_id: "demo.items.add",
                  title: "新增项目",
                  placement: "toolbar",
                  interaction: "form",
                },
              ],
            },
          ],
          modules: [],
        },
      }),
    }),
  );
  await page.route("**/api/v1/demo/items/query", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: { items: [{ name: "示例项目" }], total: 1 },
      }),
    }),
  );
}

async function serveMultiViewModuleCatalog(page: Page) {
  const action = (
    operationId: string,
    title: string,
    path: string,
    method: "GET" | "POST" = "POST",
  ) => ({
    operation_id: operationId,
    title,
    description: title,
    method,
    path,
    params: [],
    input_schema: { type: "object", properties: {} },
    output_schema: { type: "object" },
    request_media_type: "json",
    response_kind: "json",
    requires_auth: true,
  });
  const tableView = (viewId: string, title: string, dataAction: string) => ({
    view_id: viewId,
    title,
    table: viewId,
    data_action: dataAction,
    columns: [
      {
        field: "id",
        title: "ID",
        description: "项目 ID",
        widget: "integer",
        required: true,
        searchable: false,
        filterable: true,
        sortable: true,
      },
      {
        field: "name",
        title: "名称",
        description: "项目名称",
        widget: "text",
        required: true,
        searchable: true,
        filterable: true,
        sortable: true,
      },
    ],
    form: { fields: [] },
    query: {
      search_fields: ["name"],
      filter_fields: ["id", "name"],
      default_sort: [],
      default_page_size: 20,
      max_page_size: 100,
    },
    actions: [],
    action_presentations: [],
  });

  await page.route("**/.well-known/yang/ui-catalog", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          schema_version: "2.3",
          revision: "c".repeat(64),
          actions: [
            action(
              "demo.alpha.query",
              "查询 Alpha",
              "/api/v1/demo/alpha/query",
            ),
            action("demo.beta.query", "查询 Beta", "/api/v1/demo/beta/query"),
            action(
              "demo.items.insight",
              "项目洞察",
              "/api/v1/demo/items/insight",
              "GET",
            ),
            action(
              "demo.items.archive",
              "批量归档",
              "/api/v1/demo/items/archive",
            ),
            {
              ...action(
                "demo.items.rename",
                "重命名项目",
                "/api/v1/demo/items/rename",
              ),
              input_schema: {
                type: "object",
                properties: {
                  item_id: { type: "integer", title: "项目 ID" },
                  name: { type: "string", title: "新名称" },
                },
                required: ["item_id", "name"],
              },
            },
            {
              ...action(
                "demo.items.export",
                "导出项目",
                "/api/v1/demo/items/export",
                "GET",
              ),
              response_kind: "download",
            },
            {
              ...action(
                "demo.items.preview",
                "预览项目",
                "/api/v1/demo/items/preview",
                "GET",
              ),
              response_kind: "preview",
            },
            {
              ...action(
                "demo.items.navigate",
                "前往项目",
                "/api/v1/demo/items/navigate",
                "GET",
              ),
              response_kind: "redirect",
            },
            action(
              "demo.items.unknown-custom",
              "未知自定义页",
              "/api/v1/demo/items/unknown-custom",
              "GET",
            ),
            action(
              "demo.items.disabled",
              "停用操作",
              "/api/v1/demo/items/disabled",
            ),
            action(
              "demo.items.hidden",
              "隐藏操作",
              "/api/v1/demo/items/hidden",
            ),
            action(
              "demo.items.global-only",
              "全局未授权操作",
              "/api/v1/demo/items/global-only",
            ),
          ],
          table_views: [
            tableView("demo.alpha", "Alpha 项目", "demo.alpha.query"),
            tableView("demo.beta", "Beta 项目", "demo.beta.query"),
          ],
          modules: [
            {
              module_id: "demo.multi",
              identity: {
                id: "user",
                title: "个人账户",
                icon: "person",
                order: 10,
              },
              title: "多视图项目",
              description: "正式模块页完整契约",
              icon: "account",
              order: 10,
              actions: [
                "demo.items.insight",
                "demo.items.archive",
                "demo.items.rename",
                "demo.items.export",
                "demo.items.preview",
                "demo.items.navigate",
                "demo.items.unknown-custom",
                "demo.items.disabled",
                "demo.items.hidden",
              ],
              action_presentations: [
                {
                  operation_id: "demo.items.insight",
                  title: "项目洞察",
                  placement: "toolbar",
                  interaction: "custom",
                  view_id: "demo.items.insight",
                  appearance: { order: 1 },
                },
                {
                  operation_id: "demo.items.unknown-custom",
                  title: "未知自定义页",
                  placement: "toolbar",
                  interaction: "custom",
                  view_id: "demo.items.not-registered",
                  appearance: { order: 2 },
                },
                {
                  operation_id: "demo.items.export",
                  title: "导出项目",
                  placement: "toolbar",
                  interaction: "download",
                  appearance: { order: 3, overflow: true },
                },
                {
                  operation_id: "demo.items.preview",
                  title: "预览项目",
                  placement: "toolbar",
                  interaction: "preview",
                  appearance: { order: 4, overflow: true },
                },
                {
                  operation_id: "demo.items.navigate",
                  title: "前往项目",
                  placement: "toolbar",
                  interaction: "navigate",
                  appearance: { order: 5, overflow: true },
                },
                {
                  operation_id: "demo.items.disabled",
                  title: "停用操作",
                  placement: "toolbar",
                  interaction: "invoke",
                  availability: {
                    state: "disabled",
                    reason: "当前状态不可用",
                  },
                  appearance: { order: 6, overflow: true },
                },
                {
                  operation_id: "demo.items.hidden",
                  title: "隐藏操作",
                  placement: "toolbar",
                  interaction: "invoke",
                  availability: { state: "hidden", reason: "无权访问" },
                },
                {
                  operation_id: "demo.items.global-only",
                  title: "全局未授权操作",
                  placement: "toolbar",
                  interaction: "invoke",
                },
                {
                  operation_id: "demo.items.rename",
                  title: "重命名项目",
                  placement: "row",
                  interaction: "form",
                  record_parameter: "item_id",
                },
                {
                  operation_id: "demo.items.archive",
                  title: "批量归档",
                  placement: "bulk",
                  interaction: "invoke",
                },
              ],
              views: ["demo.alpha", "demo.beta"],
            },
          ],
        },
      }),
    }),
  );
  await page.route("**/api/v1/demo/alpha/query", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          items: [{ id: 1, name: "Alpha 一号" }],
          page: 1,
          page_size: 20,
          total: 1,
        },
      }),
    }),
  );
  await page.route("**/api/v1/demo/beta/query", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          items: [{ id: 2, name: "Beta 二号" }],
          page: 1,
          page_size: 20,
          total: 1,
        },
      }),
    }),
  );
  await page.route("**/api/v1/demo/items/insight", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: { total: 2, active: 1, draft: 1 },
      }),
    }),
  );
  await page.route("**/api/v1/demo/items/export", (route) =>
    route.fulfill({
      status: 200,
      contentType: "text/plain",
      body: "formal module export",
      headers: {
        "Content-Disposition": 'attachment; filename="formal-module.txt"',
      },
    }),
  );
  await page.route("**/api/v1/demo/items/preview", (route) =>
    route.fulfill({
      status: 200,
      contentType: "text/plain",
      body: "formal module preview",
      headers: { "Content-Disposition": "inline" },
    }),
  );
  await page.route("**/api/v1/demo/items/navigate", (route) =>
    route.fulfill({
      status: 302,
      headers: { Location: "/roles" },
      body: "",
    }),
  );
}

test("正式控制台模块只有一个导航入口", async ({ page }) => {
  await restoreSession(page, "user");
  await page.route("**/.well-known/yang/ui-catalog", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          schema_version: "2.3",
          revision: "f".repeat(64),
          actions: [
            {
              operation_id: "account.user.me",
              title: "当前用户",
              description: "查看当前登录账号",
              method: "GET",
              path: "/api/v1/account/user/me",
              params: [],
              input_schema: {},
              output_schema: {},
              request_media_type: "json",
              response_kind: "json",
              requires_auth: true,
            },
          ],
          table_views: [],
          modules: [
            {
              module_id: "account.user",
              identity: {
                id: "user",
                title: "个人账户",
                icon: "person",
                order: 10,
              },
              title: "用户中心",
              description: "",
              icon: "account",
              order: 10,
              primary_action: "account.user.me",
              actions: [],
              action_presentations: [],
              views: [],
            },
          ],
        },
      }),
    }),
  );
  await page.goto("/module/account.user");

  await expect(page.getByRole("tab")).toHaveCount(0);
  await expect(page.getByTestId("module-nav-account.user")).toBeVisible();
  await expect(page.getByTestId("module-navigation")).toHaveCount(1);
  await expect(page.getByText("account.user", { exact: true })).toHaveCount(0);
  await expect(
    page.locator(".formal-context").getByText("个人账户", { exact: true }),
  ).toBeVisible();
  await expect(page.getByText("切换角色", { exact: true })).toHaveCount(0);
  await expect(page.locator(".navigation-mode")).toHaveCount(0);
  await expect(page.getByRole("tab", { name: "开发工作台" })).toHaveCount(0);

  await page.goto("/workbench");
  await expect(page).toHaveURL("/workbench");
  await expect(
    page.getByText("YANG 接口工作台", { exact: true }),
  ).toBeVisible();
  await expect(page.locator(".navigation-mode")).toBeVisible();
});

test("正式业务入口使用目录投影的通用页面", async ({ page }) => {
  await restoreSession(page, "user");
  await serveBusinessCatalog(page);
  await page.goto("/business");

  await expect(page).toHaveURL("/business");
  await expect(page.getByRole("heading", { name: "项目目录" })).toBeVisible();
  await expect(
    page.locator(".formal-nav-list").getByText("项目目录", { exact: true }),
  ).toBeVisible();
  await expect(page.locator(".navigation-mode")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "新增项目" })).toBeVisible();
  await expect(
    page.locator(".table-view-heading").getByText("demo.items.main", {
      exact: true,
    }),
  ).toHaveCount(0);
  await expect(
    page.locator(".table-view-heading").getByText(/数据源/),
  ).toHaveCount(0);
});

test("正式模块页解释多 View、custom 与 bulk 契约", async ({ page }) => {
  await restoreSession(page, "user");
  await serveMultiViewModuleCatalog(page);
  const archivedRequests: unknown[] = [];
  const renamedRequests: unknown[] = [];
  await page.route("**/api/v1/demo/items/archive", async (route) => {
    archivedRequests.push(route.request().postDataJSON());
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ code: 0, message: "归档成功", data: {} }),
    });
  });
  await page.route("**/api/v1/demo/items/rename", async (route) => {
    renamedRequests.push(route.request().postDataJSON());
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ code: 0, message: "重命名成功", data: {} }),
    });
  });

  await page.goto("/module/demo.multi");

  await expect(page.getByRole("tab", { name: "Alpha 项目" })).toBeVisible();
  await expect(page.getByRole("tab", { name: "Beta 项目" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Alpha 项目" })).toBeVisible();
  await expect(page.getByText("Alpha 一号", { exact: true })).toBeVisible();

  await page.getByRole("tab", { name: "Beta 项目" }).click();
  await expect(page.getByRole("heading", { name: "Beta 项目" })).toBeVisible();
  await expect(page.getByText("Beta 二号", { exact: true })).toBeVisible();

  await page.getByRole("button", { name: "项目洞察" }).click();
  await expect(
    page.getByRole("heading", { name: "项目运行洞察" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "返回通用表格" }).click();

  await page.getByRole("button", { name: "未知自定义页" }).click();
  await expect(page.getByText(/未注册，已保留通用模块页/)).toBeVisible();
  await expect(page.getByRole("heading", { name: "Beta 项目" })).toBeVisible();
  await expect(page.getByText("隐藏操作", { exact: true })).toHaveCount(0);
  await expect(page.getByText("全局未授权操作", { exact: true })).toHaveCount(
    0,
  );

  const betaRow = page.getByRole("row", { name: /Beta 二号/ });
  await betaRow.getByRole("button", { name: "重命名项目" }).click();
  await page.getByRole("dialog").getByLabel("新名称").fill("Beta 已重命名");
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "重命名项目" })
    .click();
  await expect
    .poll(() => renamedRequests)
    .toEqual([{ item_id: 2, name: "Beta 已重命名" }]);

  await page.getByRole("button", { name: "更多工具操作" }).click();
  await expect(
    page.locator(".q-menu .q-item").filter({ hasText: "停用操作" }),
  ).toHaveClass(/disabled/);
  const downloadPromise = page.waitForEvent("download");
  await page.getByText("导出项目", { exact: true }).click();
  const download = await downloadPromise;
  expect(download.suggestedFilename()).toBe("formal-module.txt");

  await page.getByRole("button", { name: "更多工具操作" }).click();
  const popupPromise = page.waitForEvent("popup");
  await page.getByText("预览项目", { exact: true }).click();
  const preview = await popupPromise;
  await preview.close();

  await page.getByRole("button", { name: "更多工具操作" }).click();
  await page.getByText("前往项目", { exact: true }).click();
  await expect(page).toHaveURL("/module/demo.multi");

  await page.locator("tbody .q-checkbox").first().click();
  await page.getByRole("button", { name: "批量归档" }).click();
  await expect
    .poll(() => archivedRequests)
    .toEqual([{ selected: [{ id: 2, name: "Beta 二号" }] }]);
});
